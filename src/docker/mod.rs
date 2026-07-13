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

pub use model::{ContainerInfo, DiskUsage, ImageInfo, UpdateInfo, UpdateStatus};

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
            .map(|i| ImageInfo {
                id: i.id,
                repo_tags: i.repo_tags,
                size: i.size,
                containers: i.containers,
            })
            .collect())
    }

    /// List containers. When `all` is false, only running containers are returned.
    pub async fn list_containers(&self, all: bool) -> Result<Vec<ContainerInfo>> {
        let options = ListContainersOptionsBuilder::default().all(all).build();
        let containers = self.docker.list_containers(Some(options)).await?;
        Ok(containers
            .into_iter()
            .map(|c| {
                let labels = c.labels.unwrap_or_default();
                ContainerInfo {
                    id: c.id.unwrap_or_default(),
                    name: c
                        .names
                        .unwrap_or_default()
                        .first()
                        .map(|n| n.trim_start_matches('/').to_string())
                        .unwrap_or_default(),
                    image: c.image.unwrap_or_default(),
                    state: c.state.map(|s| s.to_string()).unwrap_or_default(),
                    status: c.status.unwrap_or_default(),
                    compose_service: labels
                        .get("com.docker.compose.service")
                        .cloned(),
                    compose_working_dir: labels
                        .get("com.docker.compose.project.working_dir")
                        .cloned(),
                }
            })
            .collect())
    }

    /// Aggregate disk usage, computed like `docker system df`.
    pub async fn disk_usage(&self) -> Result<DiskUsage> {
        let df = self.docker.df(None).await?;
        Ok(compute_disk_usage(&df))
    }

    /// Check whether a newer image than the local one exists in the registry.
    ///
    /// Compares the local `RepoDigest` for this reference against the registry's
    /// manifest descriptor digest. Returns `true` when they differ.
    pub async fn check_update(&self, image: &str) -> Result<bool> {
        // Local digest(s) for this image.
        let local = self
            .docker
            .inspect_image(image)
            .await
            .with_context(|| format!("inspecting local image {image}"))?;

        let repo_digests = local.repo_digests.unwrap_or_default();
        if repo_digests.is_empty() {
            return Err(anyhow!("image {image} has no registry digest (built locally?)"));
        }

        // Registry descriptor digest (no layers pulled).
        let remote = self
            .docker
            .inspect_registry_image(image, None)
            .await
            .with_context(|| format!("querying registry for {image}"))?;
        let remote_digest = remote
            .descriptor
            .digest
            .filter(|d| !d.is_empty())
            .ok_or_else(|| anyhow!("registry returned no digest for {image}"))?;

        // If any local repo digest matches the remote, we're up to date.
        let up_to_date = repo_digests.iter().any(|rd| {
            rd.rsplit_once('@')
                .map(|(_, d)| d == remote_digest)
                .unwrap_or(false)
        });
        Ok(!up_to_date)
    }

    /// A rich update check: compares the local and registry images and returns
    /// their version labels and creation dates plus a changelog source guess.
    ///
    /// Never fails — failures are captured in the returned status.
    pub async fn check_update_detailed(&self, image: &str) -> UpdateInfo {
        let changelog_repo = crate::util::ImageRef::parse(image).guess_github_repo();

        // Local image metadata.
        let local = match self.docker.inspect_image(image).await {
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

        // Locally-built images have no registry digest to compare against.
        if local.repo_digests.clone().unwrap_or_default().is_empty() {
            return UpdateInfo {
                status: UpdateStatus::LocalOnly,
                current_version,
                current_date,
                changelog_repo,
                latest_version: None,
                latest_date: None,
            };
        }

        // Remote image metadata (no layers pulled).
        match crate::registry::fetch_remote_meta(image).await {
            Ok(meta) => {
                let status = if local_id == meta.config_digest {
                    UpdateStatus::UpToDate
                } else {
                    UpdateStatus::UpdateAvailable
                };
                UpdateInfo {
                    status,
                    current_version,
                    latest_version: meta.version,
                    current_date,
                    latest_date: meta.created,
                    changelog_repo,
                }
            }
            Err(e) => UpdateInfo {
                status: UpdateStatus::Error(format!("{e:#}")),
                current_version,
                current_date,
                changelog_repo,
                latest_version: None,
                latest_date: None,
            },
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
