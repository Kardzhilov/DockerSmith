//! Persistent user configuration and state.
//!
//! Config lives at `~/.config/dockersmith/config.toml`.
//! Mutable runtime state (deferred updates, last-check times) lives at
//! `~/.config/dockersmith/state.json` so it survives restarts.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::docker::model::UpdateInfo;

/// A remote (or extra local) Docker host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostConfig {
    /// Friendly name shown in the UI.
    pub name: String,
    /// Connection endpoint, e.g. `unix:///var/run/docker.sock`,
    /// `ssh://user@host`, or `tcp://host:2376`.
    pub endpoint: String,
}

/// Notification settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NotifyConfig {
    /// Optional ntfy topic URL (e.g. `https://ntfy.sh/my-topic`) or generic webhook.
    #[serde(default)]
    pub url: Option<String>,
}

/// Scheduled background update checks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleConfig {
    /// Enable the periodic background check.
    #[serde(default)]
    pub enabled: bool,
    /// Interval between checks, in minutes.
    #[serde(default = "default_interval_minutes")]
    pub interval_minutes: u64,
}

fn default_interval_minutes() -> u64 {
    360
}

impl Default for ScheduleConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_minutes: default_interval_minutes(),
        }
    }
}

/// The full user configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Active theme name (see [`crate::theme`]).
    #[serde(default = "default_theme")]
    pub theme: String,

    /// Remote/extra hosts.
    #[serde(default)]
    pub hosts: Vec<HostConfig>,

    /// Notification settings.
    #[serde(default)]
    pub notify: NotifyConfig,

    /// Scheduled check settings.
    #[serde(default)]
    pub schedule: ScheduleConfig,

    /// GitHub token for higher changelog API rate limits (optional).
    #[serde(default)]
    pub github_token: Option<String>,
}

fn default_theme() -> String {
    "midnight".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            hosts: Vec::new(),
            notify: NotifyConfig::default(),
            schedule: ScheduleConfig::default(),
            github_token: None,
        }
    }
}

impl Config {
    /// Directory that holds config + state.
    fn dir() -> Result<PathBuf> {
        let dirs = ProjectDirs::from("", "", "dockersmith")
            .context("could not determine config directory")?;
        Ok(dirs.config_dir().to_path_buf())
    }

    fn config_path() -> Result<PathBuf> {
        Ok(Self::dir()?.join("config.toml"))
    }

    /// Load config from disk, creating a default file if none exists.
    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        if !path.exists() {
            let cfg = Config::default();
            cfg.save()?;
            return Ok(cfg);
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let cfg: Config = toml::from_str(&text)
            .with_context(|| format!("parsing config {}", path.display()))?;
        Ok(cfg)
    }

    /// Persist config to disk.
    pub fn save(&self) -> Result<()> {
        let dir = Self::dir()?;
        fs::create_dir_all(&dir)
            .with_context(|| format!("creating config dir {}", dir.display()))?;
        let path = Self::config_path()?;
        let text = toml::to_string_pretty(self).context("serializing config")?;
        fs::write(&path, text).with_context(|| format!("writing config {}", path.display()))?;
        Ok(())
    }
}

/// Mutable runtime state, persisted separately from user config.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    /// image reference -> time until which updates are deferred/ignored.
    /// A far-future timestamp means "ignore indefinitely".
    #[serde(default)]
    pub deferred: HashMap<String, DateTime<Utc>>,

    /// image reference -> a pinned changelog source (owner/repo) override.
    #[serde(default)]
    pub changelog_sources: HashMap<String, String>,

    /// image reference -> last update-check result, remembered across restarts.
    #[serde(default)]
    pub update_cache: HashMap<String, UpdateInfo>,

    /// Last time a full update check completed.
    #[serde(default)]
    pub last_check: Option<DateTime<Utc>>,
}

impl State {
    fn state_path() -> Result<PathBuf> {
        Ok(Config::dir()?.join("state.json"))
    }

    /// Load state, returning defaults if the file is missing or corrupt.
    pub fn load() -> Self {
        let Ok(path) = Self::state_path() else {
            return State::default();
        };
        match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => State::default(),
        }
    }

    /// Persist state to disk.
    pub fn save(&self) -> Result<()> {
        let dir = Config::dir()?;
        fs::create_dir_all(&dir)?;
        let path = Self::state_path()?;
        let text = serde_json::to_string_pretty(self)?;
        fs::write(path, text)?;
        Ok(())
    }

    /// Is this image currently deferred/ignored?
    pub fn is_deferred(&self, image: &str) -> bool {
        self.deferred
            .get(image)
            .map(|until| *until > Utc::now())
            .unwrap_or(false)
    }
}
