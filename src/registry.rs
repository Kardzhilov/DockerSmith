//! Registry-adjacent helpers: fetching changelogs from GitHub Releases.
//!
//! The digest-based update check itself lives in [`crate::docker`]; this module
//! covers the "What's new?" changelog viewer feature.

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::util::ImageRef;

/// A single GitHub release entry.
#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    #[serde(default)]
    pub name: Option<String>,
    pub tag_name: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub html_url: Option<String>,
}

/// Resolve the GitHub `owner/repo` to use for an image's changelog.
///
/// Uses a pinned override if provided, otherwise guesses from the image name.
pub fn resolve_source(image: &str, pinned: Option<&str>) -> Option<String> {
    if let Some(repo) = pinned {
        return Some(repo.to_string());
    }
    ImageRef::parse(image).guess_github_repo()
}

/// Fetch the most recent releases for a GitHub `owner/repo`.
pub async fn fetch_releases(
    repo: &str,
    token: Option<&str>,
    limit: usize,
) -> Result<Vec<Release>> {
    let url = format!("https://api.github.com/repos/{repo}/releases?per_page={limit}");
    let client = reqwest::Client::builder()
        .user_agent("dockersmith")
        .build()
        .context("building HTTP client")?;

    let mut req = client.get(&url).header("Accept", "application/vnd.github+json");
    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }

    let resp = req.send().await.context("requesting GitHub releases")?;
    if !resp.status().is_success() {
        anyhow::bail!("GitHub API returned {} for {repo}", resp.status());
    }
    let releases: Vec<Release> = resp.json().await.context("parsing GitHub releases")?;
    Ok(releases.into_iter().take(limit).collect())
}
