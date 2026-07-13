//! Plain data models used across the app, decoupled from bollard's response types.

/// A Docker image as shown in the images view.
#[derive(Debug, Clone)]
pub struct ImageInfo {
    /// Full image id (`sha256:...`).
    pub id: String,
    /// Repository tags, e.g. `["nginx:latest"]`. Empty for dangling images.
    pub repo_tags: Vec<String>,
    /// On-disk size in bytes.
    pub size: i64,
    /// Number of containers using this image (-1 if not computed).
    pub containers: i64,
}

impl ImageInfo {
    /// A display name: first repo tag, or short id for dangling images.
    pub fn display_name(&self) -> String {
        if let Some(tag) = self.repo_tags.first() {
            if tag != "<none>:<none>" {
                return tag.clone();
            }
        }
        format!("<none> ({})", self.short_id())
    }

    /// Short image id without the `sha256:` prefix.
    pub fn short_id(&self) -> String {
        self.id
            .strip_prefix("sha256:")
            .unwrap_or(&self.id)
            .chars()
            .take(12)
            .collect()
    }

    /// True when no container references this image.
    pub fn is_unused(&self) -> bool {
        self.containers <= 0
    }

    /// The primary reference to use for registry lookups.
    pub fn primary_reference(&self) -> Option<String> {
        self.repo_tags
            .iter()
            .find(|t| *t != "<none>:<none>")
            .cloned()
    }
}

/// A Docker container as shown in the containers view.
#[derive(Debug, Clone)]
pub struct ContainerInfo {
    pub id: String,
    /// Name without the leading `/`.
    pub name: String,
    /// The image reference the container was created from.
    pub image: String,
    /// e.g. `running`, `exited`.
    pub state: String,
    /// Human status line, e.g. `Up 3 hours`.
    pub status: String,
    /// Compose service label, if any.
    pub compose_service: Option<String>,
    /// Compose working directory label, if any.
    pub compose_working_dir: Option<String>,
}

impl ContainerInfo {
    /// Display name (already stripped of the leading `/`).
    pub fn display_name(&self) -> String {
        self.name.clone()
    }

    /// Whether the container is currently running.
    pub fn is_running(&self) -> bool {
        self.state == "running"
    }

    /// The update command a user should run to apply an update manually.
    pub fn update_command(&self) -> String {
        match (&self.compose_working_dir, &self.compose_service) {
            (Some(dir), Some(service)) if !dir.is_empty() && !service.is_empty() => {
                format!(
                    "cd \"{dir}\" && docker compose pull {service} && docker compose up -d {service}"
                )
            }
            _ => format!(
                "docker pull {img} && docker stop {name} && docker rm {name}  # then recreate",
                img = self.image,
                name = self.name
            ),
        }
    }
}

/// Aggregated disk usage, mirroring `docker system df`.
#[derive(Debug, Clone, Default)]
pub struct DiskUsage {
    pub images_total: i64,
    pub images_reclaimable: i64,
    pub images_count: usize,
    pub images_active: usize,

    pub containers_total: i64,
    pub containers_reclaimable: i64,
    pub containers_count: usize,
    pub containers_active: usize,

    pub volumes_total: i64,
    pub volumes_reclaimable: i64,
    pub volumes_count: usize,
    pub volumes_active: usize,

    pub build_cache_total: i64,
    pub build_cache_reclaimable: i64,
    pub build_cache_count: usize,
}

impl DiskUsage {
    /// Total reclaimable across all categories.
    pub fn total_reclaimable(&self) -> i64 {
        self.images_reclaimable
            + self.containers_reclaimable
            + self.volumes_reclaimable
            + self.build_cache_reclaimable
    }
}

/// The result of an update check for a single image/container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    /// Currently being checked.
    Checking,
    /// Local image matches the registry.
    UpToDate,
    /// A newer image is available in the registry.
    UpdateAvailable,
    /// The image is built locally / has no registry digest to compare.
    LocalOnly,
    /// The check failed (network, auth, etc.).
    Error(String),
}

/// Detailed result of an update check, including version/date comparison.
#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub status: UpdateStatus,
    /// Version label of the running/local image (e.g. `16.2`), if published.
    pub current_version: Option<String>,
    /// Version label of the latest registry image, if published.
    pub latest_version: Option<String>,
    /// Creation date (YYYY-MM-DD) of the local image.
    pub current_date: Option<String>,
    /// Creation date (YYYY-MM-DD) of the latest registry image.
    pub latest_date: Option<String>,
    /// Best-guess GitHub `owner/repo` for the changelog, if determinable.
    pub changelog_repo: Option<String>,
}

impl UpdateInfo {
    /// An info carrying only a status (used for placeholders and the scheduler).
    pub fn from_status(status: UpdateStatus) -> Self {
        Self {
            status,
            current_version: None,
            latest_version: None,
            current_date: None,
            latest_date: None,
            changelog_repo: None,
        }
    }

    /// The best available "current" identifier: version, else date.
    pub fn current_label(&self) -> Option<String> {
        self.current_version
            .clone()
            .or_else(|| self.current_date.clone())
    }

    /// The best available "latest" identifier: version, else date.
    pub fn latest_label(&self) -> Option<String> {
        self.latest_version
            .clone()
            .or_else(|| self.latest_date.clone())
    }

    /// A compact `current → latest` string for the table, when both are known.
    pub fn transition(&self) -> Option<String> {
        match (self.current_label(), self.latest_label()) {
            (Some(a), Some(b)) => Some(format!("{a} → {b}")),
            _ => None,
        }
    }
}

