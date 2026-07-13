//! Plain data models used across the app, decoupled from bollard's response types.

use crate::util::ImageRef;

/// A Docker image as shown in the images view.
#[derive(Debug, Clone)]
pub struct ImageInfo {
    /// Full image id (`sha256:...`).
    pub id: String,
    /// Repository tags, e.g. `["nginx:latest"]`. Empty for dangling images.
    pub repo_tags: Vec<String>,
    /// On-disk size in bytes.
    pub size: i64,
    /// Unix creation timestamp (seconds).
    pub created: i64,
    /// Version label of the image, if published.
    pub version: Option<String>,
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

    /// A shortened name with the registry host stripped (shown in SOURCE instead).
    pub fn short_name(&self) -> String {
        let full = self.display_name();
        if let Some((first, rest)) = full.split_once('/') {
            if first.contains('.') || first.contains(':') {
                return rest.to_string();
            }
        }
        full
    }

    /// A short origin label (e.g. `docker`, `ghcr`, `lscr`, or a custom host).
    pub fn source_short(&self) -> String {
        match self.primary_reference() {
            Some(reference) => {
                let reg = ImageRef::parse(&reference).registry;
                match reg.as_str() {
                    "index.docker.io" | "docker.io" => "docker".to_string(),
                    "ghcr.io" => "ghcr".to_string(),
                    "lscr.io" => "lscr".to_string(),
                    "quay.io" => "quay".to_string(),
                    "registry.gitlab.com" => "gitlab".to_string(),
                    other => other.split(':').next().unwrap_or(other).to_string(),
                }
            }
            None => "—".to_string(),
        }
    }

    /// Creation date as `YYYY-MM-DD`, or `—` if unknown.
    pub fn created_date(&self) -> String {
        if self.created <= 0 {
            return "—".to_string();
        }
        chrono::DateTime::from_timestamp(self.created, 0)
            .map(|dt| dt.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "—".to_string())
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
}

/// A step in the container-update (apply) process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyStage {
    Inspect,
    Pull,
    Stop,
    Rename,
    Create,
    Start,
    Cleanup,
    Rollback,
    /// Terminal marker for the whole operation.
    Done,
}

impl ApplyStage {
    /// The ordered checklist of stages shown to the user.
    pub const SEQUENCE: [ApplyStage; 7] = [
        ApplyStage::Inspect,
        ApplyStage::Pull,
        ApplyStage::Stop,
        ApplyStage::Rename,
        ApplyStage::Create,
        ApplyStage::Start,
        ApplyStage::Cleanup,
    ];

    /// A human-readable label for the stage.
    pub fn label(&self) -> &'static str {
        match self {
            ApplyStage::Inspect => "Inspect container",
            ApplyStage::Pull => "Pull new image",
            ApplyStage::Stop => "Stop old container",
            ApplyStage::Rename => "Set old container aside",
            ApplyStage::Create => "Create new container",
            ApplyStage::Start => "Start new container",
            ApplyStage::Cleanup => "Remove old container",
            ApplyStage::Rollback => "Roll back",
            ApplyStage::Done => "Finished",
        }
    }
}

/// The state a stage transitioned into.
#[derive(Debug, Clone)]
pub enum StageState {
    Start,
    Done,
    Failed(String),
}

/// A progress event emitted while applying a container update.
#[derive(Debug, Clone)]
pub enum ApplyProgress {
    /// A stage changed state.
    Stage(ApplyStage, StageState),
    /// A detail line (e.g. image pull progress).
    Log(String),
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
    /// The local image id this result was computed against (for cache staleness).
    #[serde(default)]
    pub local_id: Option<String>,
    /// When this check completed.
    #[serde(default)]
    pub checked_at: Option<chrono::DateTime<chrono::Utc>>,
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
            local_id: None,
            checked_at: None,
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

