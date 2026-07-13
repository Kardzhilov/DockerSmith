//! Thin async wrapper around bollard's [`Docker`] client, exposing just the
//! operations DockerSmith needs, in terms of our own [`model`] types.

pub mod model;

use anyhow::{anyhow, Context, Result};
use bollard::query_parameters::{
    ListContainersOptionsBuilder, ListImagesOptionsBuilder, LogsOptionsBuilder,
    RemoveContainerOptionsBuilder, RestartContainerOptions, StatsOptionsBuilder,
    StopContainerOptions,
};
use bollard::{Docker, API_DEFAULT_VERSION};
use futures_util::StreamExt;

pub use model::{
    ApplyProgress, ApplyStage, ContainerInfo, DiskUsage, ImageInfo, StageState, UpdateInfo,
    UpdateStatus,
};
/// A connected Docker client for a single host.
#[derive(Clone)]
pub struct DockerClient {
    docker: Docker,
}

impl DockerClient {
    /// Connect to the local daemon (socket / named pipe) with version negotiation.
    pub async fn connect_local() -> Result<Self> {
        let docker = Docker::connect_with_local_defaults()
            .context("connecting to local Docker daemon")?;
        let docker = docker
            .negotiate_version()
            .await
            .context("negotiating Docker API version")?;
        Ok(Self { docker })
    }

    /// Connect to an arbitrary endpoint: `unix://`, `tcp://`, `http://`, or `ssh://`.
    pub async fn connect(endpoint: &str) -> Result<Self> {
        let docker = if let Some(path) = endpoint.strip_prefix("unix://") {
            Docker::connect_with_socket(path, 120, API_DEFAULT_VERSION)
                .with_context(|| format!("connecting to socket {path}"))?
        } else if endpoint.starts_with("tcp://") || endpoint.starts_with("http://") {
            Docker::connect_with_http(endpoint, 120, API_DEFAULT_VERSION)
                .with_context(|| format!("connecting to {endpoint}"))?
        } else if endpoint.starts_with("ssh://") {
            // bollard's default connector understands ssh:// via DOCKER_HOST when the
            // `ssh` feature is enabled; point it at this endpoint.
            std::env::set_var("DOCKER_HOST", endpoint);
            Docker::connect_with_defaults()
                .with_context(|| format!("connecting over ssh to {endpoint}"))?
        } else {
            return Err(anyhow!("unsupported Docker endpoint: {endpoint}"));
        };
        let docker = docker
            .negotiate_version()
            .await
            .context("negotiating Docker API version")?;
        Ok(Self { docker })
    }

    /// Return the daemon's API version string.
    pub async fn version(&self) -> Result<String> {
        let v = self.docker.version().await?;
        Ok(v.api_version.unwrap_or_else(|| "unknown".to_string()))
    }

    /// List images. When `all` is false, intermediate images are hidden.
    pub async fn list_images(&self, all: bool) -> Result<Vec<ImageInfo>> {
        let options = ListImagesOptionsBuilder::default().all(all).build();
        let images = self.docker.list_images(Some(options)).await?;
        Ok(images
            .into_iter()
            .map(|i| {
                let version = crate::registry::VERSION_LABELS
                    .iter()
                    .find_map(|k| i.labels.get(*k).filter(|v| !v.is_empty()).cloned());
                ImageInfo {
                    id: i.id,
                    repo_tags: i.repo_tags,
                    size: i.size,
                    created: i.created,
                    version,
                }
            })
            .collect())
    }

    /// List containers. When `all` is false, only running containers are returned.
    ///
    /// When `docker ps` reports a container's image as a bare image id (which
    /// happens once its tag has been re-pulled to a newer image), the original
    /// `repo:tag` reference is recovered from the container config so update
    /// checks can still query the registry.
    pub async fn list_containers(&self, all: bool) -> Result<Vec<ContainerInfo>> {
        use bollard::query_parameters::InspectContainerOptions;

        let options = ListContainersOptionsBuilder::default().all(all).build();
        let containers = self.docker.list_containers(Some(options)).await?;

        let mut result = Vec::with_capacity(containers.len());
        for c in containers {
            let id = c.id.unwrap_or_default();
            let mut image = c.image.unwrap_or_default();

            // Recover a proper reference if the summary only gave an image id.
            if looks_like_image_id(&image) {
                if let Ok(inspect) = self
                    .docker
                    .inspect_container(&id, None::<InspectContainerOptions>)
                    .await
                {
                    if let Some(cfg_img) = inspect.config.and_then(|cfg| cfg.image) {
                        if !cfg_img.is_empty() && !looks_like_image_id(&cfg_img) {
                            image = cfg_img;
                        }
                    }
                }
            }

            result.push(ContainerInfo {
                id,
                name: c
                    .names
                    .unwrap_or_default()
                    .first()
                    .map(|n| n.trim_start_matches('/').to_string())
                    .unwrap_or_default(),
                image,
                image_id: c.image_id.unwrap_or_default(),
                state: c.state.map(|s| s.to_string()).unwrap_or_default(),
                status: c.status.unwrap_or_default(),
            });
        }
        Ok(result)
    }

    /// Aggregate disk usage, computed like `docker system df`.
    pub async fn disk_usage(&self) -> Result<DiskUsage> {
        let df = self.docker.df(None).await?;
        Ok(compute_disk_usage(&df))
    }

    /// A rich update check.
    ///
    /// - `local_image` identifies the image the target is actually running (an id
    ///   for a container, or a tag for the images view) — used to read the local
    ///   digest, version label, and build date.
    /// - `registry_ref` is the `repo:tag` used to query the registry.
    ///
    /// Comparing the *running* image's digest against the registry means a
    /// container still on an old image is correctly flagged even after its tag was
    /// re-pulled. Never fails — failures are captured in the returned status.
    pub async fn check_update_detailed(&self, local_image: &str, registry_ref: &str) -> UpdateInfo {
        let changelog_repo = crate::util::ImageRef::parse(registry_ref).guess_github_repo();

        // Local image metadata (the image actually in use).
        let local = match self.docker.inspect_image(local_image).await {
            Ok(l) => l,
            Err(e) => {
                return UpdateInfo {
                    status: UpdateStatus::Error(e.to_string()),
                    changelog_repo,
                    ..UpdateInfo::from_status(UpdateStatus::Error(String::new()))
                };
            }
        };

        let local_id = local.id.clone().unwrap_or_default();
        let current_date = local
            .created
            .as_ref()
            .map(|c| c.chars().take(10).collect::<String>())
            .filter(|s| !s.is_empty());
        let current_version = local
            .config
            .as_ref()
            .and_then(|c| c.labels.as_ref())
            .and_then(|labels| {
                crate::registry::VERSION_LABELS
                    .iter()
                    .find_map(|k| labels.get(*k).filter(|v| !v.is_empty()).cloned())
            });

        let repo_digests = local.repo_digests.clone().unwrap_or_default();

        // Without a registry reference or a local digest we cannot compare.
        if repo_digests.is_empty() || looks_like_image_id(registry_ref) {
            return UpdateInfo {
                status: UpdateStatus::LocalOnly,
                current_version,
                current_date,
                changelog_repo,
                latest_version: None,
                latest_date: None,
                local_id: Some(local_id),
                checked_at: Some(chrono::Utc::now()),
            };
        }

        // Determine status by comparing the running image's manifest digest against
        // the registry's current digest (daemon distribution-inspect: robust across
        // image stores, multi-arch, and private registries via daemon credentials).
        let status = match self.docker.inspect_registry_image(registry_ref, None).await {
            Ok(remote) => match remote.descriptor.digest.filter(|d| !d.is_empty()) {
                Some(remote_digest) => {
                    let up_to_date = repo_digests.iter().any(|rd| {
                        rd.rsplit_once('@')
                            .map(|(_, d)| d == remote_digest)
                            .unwrap_or(false)
                    });
                    if up_to_date {
                        UpdateStatus::UpToDate
                    } else {
                        UpdateStatus::UpdateAvailable
                    }
                }
                None => UpdateStatus::Error("registry returned no digest".to_string()),
            },
            Err(e) => UpdateStatus::Error(format!("{e}")),
        };

        // Best-effort enrichment: the latest image's version label and build date,
        // read from the registry config blob (a few KB, no layers).
        let (latest_version, latest_date) =
            match crate::registry::fetch_remote_meta(registry_ref).await {
                Ok(meta) => (meta.version, meta.created),
                Err(_) => (None, None),
            };

        UpdateInfo {
            status,
            current_version,
            latest_version,
            current_date,
            latest_date,
            changelog_repo,
            local_id: Some(local_id),
            checked_at: Some(chrono::Utc::now()),
        }
    }

    // ── Container lifecycle ────────────────────────────────────────────────

    /// Start a stopped container.
    pub async fn start_container(&self, id: &str) -> Result<()> {
        self.docker
            .start_container(id, None::<bollard::query_parameters::StartContainerOptions>)
            .await?;
        Ok(())
    }

    /// Stop a running container.
    pub async fn stop_container(&self, id: &str) -> Result<()> {
        self.docker
            .stop_container(id, None::<StopContainerOptions>)
            .await?;
        Ok(())
    }

    /// Restart a container.
    pub async fn restart_container(&self, id: &str) -> Result<()> {
        self.docker
            .restart_container(id, None::<RestartContainerOptions>)
            .await?;
        Ok(())
    }

    /// Remove a container (force-stops if running).
    pub async fn remove_container(&self, id: &str) -> Result<()> {
        let options = RemoveContainerOptionsBuilder::default().force(true).build();
        self.docker.remove_container(id, Some(options)).await?;
        Ok(())
    }

    // ── Logs ───────────────────────────────────────────────────────────────

    /// Fetch the last `tail` lines of a container's logs as plain text.
    pub async fn logs(&self, id: &str, tail: usize) -> Result<Vec<String>> {
        let options = LogsOptionsBuilder::default()
            .stdout(true)
            .stderr(true)
            .tail(&tail.to_string())
            .build();
        let mut stream = self.docker.logs(id, Some(options));
        let mut lines = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(output) => {
                    let text = output.to_string();
                    for line in text.split('\n') {
                        if !line.is_empty() {
                            lines.push(line.to_string());
                        }
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(lines)
    }

    // ── Stats ──────────────────────────────────────────────────────────────

    /// Take a single CPU/memory stats sample for a container.
    pub async fn stats_once(&self, id: &str) -> Result<ContainerStats> {
        let options = StatsOptionsBuilder::default().stream(false).one_shot(true).build();
        let mut stream = self.docker.stats(id, Some(options));
        if let Some(item) = stream.next().await {
            let s = item?;
            return Ok(compute_stats(&s));
        }
        Err(anyhow!("no stats returned for {id}"))
    }

    // ── Prune ──────────────────────────────────────────────────────────────

    /// Prune dangling (or all unused) images. Returns space reclaimed in bytes.
    pub async fn prune_images(&self, all_unused: bool) -> Result<i64> {
        use std::collections::HashMap;
        let mut filters: HashMap<String, Vec<String>> = HashMap::new();
        if !all_unused {
            filters.insert("dangling".to_string(), vec!["true".to_string()]);
        }
        let options = bollard::query_parameters::PruneImagesOptionsBuilder::default()
            .filters(&filters)
            .build();
        let resp = self.docker.prune_images(Some(options)).await?;
        Ok(resp.space_reclaimed.unwrap_or(0))
    }

    /// Prune stopped containers. Returns space reclaimed in bytes.
    pub async fn prune_containers(&self) -> Result<i64> {
        let resp = self
            .docker
            .prune_containers(None::<bollard::query_parameters::PruneContainersOptions>)
            .await?;
        Ok(resp.space_reclaimed.unwrap_or(0))
    }

    /// Prune unused volumes. Returns space reclaimed in bytes.
    pub async fn prune_volumes(&self) -> Result<i64> {
        let resp = self
            .docker
            .prune_volumes(None::<bollard::query_parameters::PruneVolumesOptions>)
            .await?;
        Ok(resp.space_reclaimed.unwrap_or(0))
    }

    /// Prune the build cache. Returns space reclaimed in bytes.
    pub async fn prune_build_cache(&self) -> Result<i64> {
        let resp = self
            .docker
            .prune_build(None::<bollard::query_parameters::PruneBuildOptions>)
            .await?;
        Ok(resp.space_reclaimed.unwrap_or(0) as i64)
    }

    // ── Pull ───────────────────────────────────────────────────────────────

    /// Pull the latest version of an image, streaming progress lines to `on_progress`.
    pub async fn pull_image<F: FnMut(String)>(
        &self,
        image: &str,
        mut on_progress: F,
    ) -> Result<()> {
        let (from_image, tag) = split_ref_for_pull(image);
        let options = bollard::query_parameters::CreateImageOptionsBuilder::default()
            .from_image(&from_image)
            .tag(&tag)
            .build();
        let mut stream = self.docker.create_image(Some(options), None, None);
        while let Some(item) = stream.next().await {
            let info = item?;
            if let Some(status) = info.status {
                let progress = info.progress.unwrap_or_default();
                on_progress(format!("{status} {progress}").trim().to_string());
            }
        }
        Ok(())
    }

    // ── Apply update (pull + recreate) ─────────────────────────────────────

    /// Update a container to the latest image by pulling it and recreating the
    /// container with the same configuration (Watchtower pattern).
    ///
    /// Preserves the container's config, host config (volumes, ports, restart
    /// policy, devices, capabilities, etc.), and network attachments. The old
    /// container is renamed aside first and restored automatically if the new
    /// one fails to start.
    ///
    /// Emits structured [`ApplyProgress`] events describing each stage.
    pub async fn apply_update<F: FnMut(ApplyProgress)>(
        &self,
        id: &str,
        image: &str,
        mut report: F,
    ) -> Result<()> {
        use bollard::query_parameters::{
            CreateContainerOptionsBuilder, InspectContainerOptions, RemoveContainerOptionsBuilder,
            RenameContainerOptionsBuilder, StartContainerOptions, StopContainerOptions,
        };
        use model::ApplyProgress::{Log, Stage};
        use model::ApplyStage as St;
        use model::StageState as SS;

        // Emit a final "Done" event and return, so the UI always gets a terminal state.
        macro_rules! fail {
            ($report:expr, $stage:expr, $msg:expr) => {{
                let m: String = $msg;
                $report(Stage($stage, SS::Failed(m.clone())));
                $report(Stage(St::Done, SS::Failed(m.clone())));
                return Err(anyhow!(m));
            }};
        }

        // 1. Inspect.
        report(Stage(St::Inspect, SS::Start));
        let inspect = match self
            .docker
            .inspect_container(id, None::<InspectContainerOptions>)
            .await
        {
            Ok(v) => {
                report(Stage(St::Inspect, SS::Done));
                v
            }
            Err(e) => fail!(report, St::Inspect, format!("inspect failed: {e}")),
        };
        let name = inspect
            .name
            .clone()
            .unwrap_or_default()
            .trim_start_matches('/')
            .to_string();
        if name.is_empty() {
            fail!(report, St::Inspect, "could not determine container name".to_string());
        }
        let Some(config) = inspect.config.clone() else {
            fail!(report, St::Inspect, "container has no config to preserve".to_string());
        };
        let host_config = inspect.host_config.clone();
        let networks = inspect.network_settings.and_then(|n| n.networks);

        // 2. Pull the new image (streaming detail lines).
        report(Stage(St::Pull, SS::Start));
        if let Err(e) = self.pull_image(image, |l| report(Log(l))).await {
            fail!(report, St::Pull, format!("pull failed: {e:#}"));
        }
        report(Stage(St::Pull, SS::Done));

        // Build the create spec from the running config (JSON round-trip).
        let mut body: bollard::models::ContainerCreateBody =
            match serde_json::to_value(&config).and_then(serde_json::from_value) {
                Ok(b) => b,
                Err(e) => fail!(report, St::Create, format!("building create spec: {e}")),
            };
        body.image = Some(image.to_string());
        body.host_config = host_config;
        if let Some(nets) = networks {
            body.networking_config = Some(bollard::models::NetworkingConfig {
                endpoints_config: Some(nets),
            });
        }

        // 3. Stop old container (non-fatal if already stopped).
        report(Stage(St::Stop, SS::Start));
        let _ = self
            .docker
            .stop_container(id, None::<StopContainerOptions>)
            .await;
        report(Stage(St::Stop, SS::Done));

        // 4. Rename old container aside (point of no return once done).
        report(Stage(St::Rename, SS::Start));
        let backup = format!("{name}_dockersmith_old");
        if let Err(e) = self
            .docker
            .rename_container(
                id,
                RenameContainerOptionsBuilder::default().name(&backup).build(),
            )
            .await
        {
            fail!(report, St::Rename, format!("rename failed: {e}"));
        }
        report(Stage(St::Rename, SS::Done));

        let force_remove = RemoveContainerOptionsBuilder::default().force(true).build();

        // Restore the old container after a failure past the rename step.
        macro_rules! rollback {
            ($report:expr, $stage:expr, $msg:expr) => {{
                let m: String = $msg;
                $report(Stage($stage, SS::Failed(m.clone())));
                $report(Stage(St::Rollback, SS::Start));
                let _ = self
                    .docker
                    .remove_container(&name, Some(force_remove.clone()))
                    .await;
                let _ = self
                    .docker
                    .rename_container(
                        &backup,
                        RenameContainerOptionsBuilder::default().name(&name).build(),
                    )
                    .await;
                let _ = self
                    .docker
                    .start_container(&name, None::<StartContainerOptions>)
                    .await;
                $report(Stage(St::Rollback, SS::Done));
                $report(Stage(St::Done, SS::Failed(m.clone())));
                return Err(anyhow!(m));
            }};
        }

        // 5. Create new container.
        report(Stage(St::Create, SS::Start));
        let create_opts = CreateContainerOptionsBuilder::default().name(&name).build();
        let created = match self.docker.create_container(Some(create_opts), body).await {
            Ok(c) => {
                report(Stage(St::Create, SS::Done));
                c
            }
            Err(e) => rollback!(report, St::Create, format!("create failed: {e}")),
        };

        // 6. Start new container.
        report(Stage(St::Start, SS::Start));
        if let Err(e) = self
            .docker
            .start_container(&created.id, None::<StartContainerOptions>)
            .await
        {
            rollback!(report, St::Start, format!("start failed: {e}"));
        }
        report(Stage(St::Start, SS::Done));

        // 7. Remove the old backup container.
        report(Stage(St::Cleanup, SS::Start));
        let _ = self
            .docker
            .remove_container(&backup, Some(force_remove))
            .await;
        report(Stage(St::Cleanup, SS::Done));

        report(Stage(St::Done, SS::Done));
        Ok(())
    }
}

/// A single stats sample.
#[derive(Debug, Clone, Default)]
pub struct ContainerStats {
    /// CPU usage as a percentage (0-100 per core-normalized).
    pub cpu_percent: f64,
    /// Memory used in bytes.
    pub mem_usage: i64,
    /// Memory limit in bytes.
    pub mem_limit: i64,
}

impl ContainerStats {
    /// Memory usage as a percentage of the limit.
    pub fn mem_percent(&self) -> f64 {
        if self.mem_limit > 0 {
            (self.mem_usage as f64 / self.mem_limit as f64) * 100.0
        } else {
            0.0
        }
    }
}

/// Compute CPU%/memory from a bollard stats response (docker's standard formula).
fn compute_stats(s: &bollard::models::ContainerStatsResponse) -> ContainerStats {
    let cpu = s.cpu_stats.as_ref();
    let pre = s.precpu_stats.as_ref();

    let cpu_total = cpu
        .and_then(|c| c.cpu_usage.as_ref())
        .and_then(|u| u.total_usage)
        .unwrap_or(0) as f64;
    let pre_total = pre
        .and_then(|c| c.cpu_usage.as_ref())
        .and_then(|u| u.total_usage)
        .unwrap_or(0) as f64;
    let system = cpu.and_then(|c| c.system_cpu_usage).unwrap_or(0) as f64;
    let pre_system = pre.and_then(|c| c.system_cpu_usage).unwrap_or(0) as f64;
    let online = cpu.and_then(|c| c.online_cpus).unwrap_or(1).max(1) as f64;

    let cpu_delta = cpu_total - pre_total;
    let system_delta = system - pre_system;
    let cpu_percent = if system_delta > 0.0 && cpu_delta > 0.0 {
        (cpu_delta / system_delta) * online * 100.0
    } else {
        0.0
    };

    let mem = s.memory_stats.as_ref();
    let mem_usage = mem.and_then(|m| m.usage).unwrap_or(0) as i64;
    let mem_limit = mem.and_then(|m| m.limit).unwrap_or(0) as i64;

    ContainerStats {
        cpu_percent,
        mem_usage,
        mem_limit,
    }
}

/// Split an image reference into `(from_image, tag)` for the pull API.
fn split_ref_for_pull(image: &str) -> (String, String) {
    // Only split on a colon in the final path segment (avoid registry:port).
    if let Some((repo, tag)) = image.rsplit_once(':') {
        if !tag.contains('/') {
            return (repo.to_string(), tag.to_string());
        }
    }
    (image.to_string(), "latest".to_string())
}

/// Whether a string is a bare image id rather than a `repo:tag` reference.
///
/// `docker ps` reports the image as an id (e.g. `sha256:…` or a long hex string)
/// once a container's tag has been re-pulled to a newer image.
fn looks_like_image_id(s: &str) -> bool {
    if let Some(hex) = s.strip_prefix("sha256:") {
        return hex.len() >= 12 && hex.chars().all(|c| c.is_ascii_hexdigit());
    }
    s.len() >= 12 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Compute [`DiskUsage`] from a bollard `df` response, matching `docker system df`.
fn compute_disk_usage(df: &bollard::models::SystemDataUsageResponse) -> DiskUsage {
    let mut u = DiskUsage {
        images_total: df.layers_size.unwrap_or(0),
        ..Default::default()
    };

    // Images: total = layers_size (deduplicated on-disk total).
    // Reclaimable = unique (non-shared) size of images no container references.
    if let Some(images) = &df.images {
        u.images_count = images.len();
        for img in images {
            if img.containers > 0 {
                u.images_active += 1;
            } else if img.size >= 0 {
                let shared = img.shared_size.max(0);
                u.images_reclaimable += (img.size - shared).max(0);
            }
        }
    }

    // Containers: total = sum(size_rw); reclaimable = stopped containers' size_rw.
    if let Some(containers) = &df.containers {
        u.containers_count = containers.len();
        for c in containers {
            let size = c.size_rw.unwrap_or(0);
            u.containers_total += size;
            let running = matches!(
                c.state,
                Some(bollard::models::ContainerSummaryStateEnum::RUNNING)
            );
            if running {
                u.containers_active += 1;
            } else {
                u.containers_reclaimable += size;
            }
        }
    }

    // Volumes: total = sum(usage.size); reclaimable = volumes with ref_count == 0.
    if let Some(volumes) = &df.volumes {
        u.volumes_count = volumes.len();
        for v in volumes {
            if let Some(usage) = &v.usage_data {
                if usage.size >= 0 {
                    u.volumes_total += usage.size;
                    if usage.ref_count <= 0 {
                        u.volumes_reclaimable += usage.size;
                    } else {
                        u.volumes_active += 1;
                    }
                }
            }
        }
    }

    // Build cache: total = sum(all record sizes); reclaimable = records that are
    // neither in use nor shared with another record.
    if let Some(cache) = &df.build_cache {
        u.build_cache_count = cache.len();
        for c in cache {
            let size = c.size.unwrap_or(0);
            u.build_cache_total += size;
            if !c.in_use.unwrap_or(false) && !c.shared.unwrap_or(false) {
                u.build_cache_reclaimable += size;
            }
        }
    }

    u
}
