//! Registry-adjacent helpers: fetching changelogs from GitHub Releases, and
//! reading remote image metadata (version label + creation date) from the
//! registry without pulling any layers.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::util::ImageRef;

/// Labels checked, in priority order, for a human-readable image version.
pub const VERSION_LABELS: &[&str] = &[
    "org.opencontainers.image.version",
    "build_version",
    "version",
    "org.label-schema.version",
    "app.version",
];

/// Pick the first non-empty version label from a label map.
pub fn version_from_labels(labels: &serde_json::Map<String, Value>) -> Option<String> {
    VERSION_LABELS.iter().find_map(|key| {
        labels
            .get(*key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    })
}


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

/// Remote image metadata read from the registry (no layers pulled).
#[derive(Debug, Clone)]
pub struct RemoteImageMeta {
    /// Version label of the latest image, if published.
    pub version: Option<String>,
    /// Creation date (YYYY-MM-DD) of the latest image, if available.
    pub created: Option<String>,
}

/// Media types we accept when fetching a manifest.
const MANIFEST_ACCEPT: &str = "application/vnd.oci.image.manifest.v1+json, \
application/vnd.docker.distribution.manifest.v2+json, \
application/vnd.oci.image.index.v1+json, \
application/vnd.docker.distribution.manifest.list.v2+json";

/// Fetch the config digest, version label, and creation date for the current
/// registry image behind `image`, contacting the registry HTTP API directly.
///
/// Downloads only the manifest and the small config blob (a few KB), never the
/// image layers. Works with anonymous and Bearer-token registries (Docker Hub,
/// GHCR, `lscr.io`, and OCI-compliant private registries that allow anonymous
/// pulls).
pub async fn fetch_remote_meta(image: &str) -> Result<RemoteImageMeta> {
    let r = ImageRef::parse(image);

    // Docker Hub uses distinct hosts for auth and the v2 API.
    let (api_host, is_hub) = if r.registry == "index.docker.io" || r.registry == "docker.io" {
        ("registry-1.docker.io".to_string(), true)
    } else {
        (r.registry.clone(), false)
    };

    let client = reqwest::Client::builder()
        .user_agent("dockersmith")
        .build()
        .context("building HTTP client")?;

    let token = get_token(&client, &api_host, &r.repository, is_hub).await;

    let base = format!("https://{api_host}/v2/{}", r.repository);

    // Fetch the top-level manifest (may be a single manifest or an index/list).
    let manifest: Value = fetch_json(&client, &format!("{base}/manifests/{}", r.tag), token.as_deref(), Some(MANIFEST_ACCEPT))
        .await
        .context("fetching manifest")?;

    // Resolve a manifest list/index down to the manifest for this host's arch.
    let manifest = if manifest.get("manifests").is_some() {
        let sub_digest = pick_platform_digest(&manifest)
            .ok_or_else(|| anyhow!("no matching platform in manifest index"))?;
        fetch_json(
            &client,
            &format!("{base}/manifests/{sub_digest}"),
            token.as_deref(),
            Some(MANIFEST_ACCEPT),
        )
        .await
        .context("fetching platform manifest")?
    } else {
        manifest
    };

    let config_digest = manifest
        .get("config")
        .and_then(|c| c.get("digest"))
        .and_then(|d| d.as_str())
        .ok_or_else(|| anyhow!("manifest has no config digest"))?
        .to_string();

    // Fetch the config blob to read labels + creation date.
    let (version, created) = match fetch_json(
        &client,
        &format!("{base}/blobs/{config_digest}"),
        token.as_deref(),
        None,
    )
    .await
    {
        Ok(blob) => {
            let labels = blob
                .get("config")
                .and_then(|c| c.get("Labels"))
                .or_else(|| blob.get("container_config").and_then(|c| c.get("Labels")))
                .and_then(|l| l.as_object());
            let version = labels.and_then(version_from_labels);
            let created = blob
                .get("created")
                .and_then(|c| c.as_str())
                .map(|s| s.chars().take(10).collect::<String>())
                .filter(|s| !s.is_empty());
            (version, created)
        }
        Err(_) => (None, None),
    };

    Ok(RemoteImageMeta { version, created })
}

/// Obtain a Bearer token for the given repository, if the registry requires one.
async fn get_token(
    client: &reqwest::Client,
    api_host: &str,
    repository: &str,
    is_hub: bool,
) -> Option<String> {
    if is_hub {
        let url = format!(
            "https://auth.docker.io/token?service=registry.docker.io&scope=repository:{repository}:pull"
        );
        return request_token(client, &url).await;
    }

    // Probe the registry; a 401 carries a Bearer challenge describing where to
    // request a token.
    let probe = client
        .get(format!("https://{api_host}/v2/"))
        .send()
        .await
        .ok()?;
    if probe.status() != reqwest::StatusCode::UNAUTHORIZED {
        return None; // anonymous access allowed
    }
    let header = probe
        .headers()
        .get("www-authenticate")?
        .to_str()
        .ok()?
        .to_string();
    let realm = extract_directive(&header, "realm")?;
    let service = extract_directive(&header, "service").unwrap_or_default();
    // Always request a token scoped to THIS repository. Some registries — notably
    // lscr.io (LinuxServer, proxied to ghcr.io) — return a placeholder scope like
    // `repository:user/image:pull` in the challenge, which would yield a token that
    // 404s on the real manifest. The realm/service from the challenge are correct.
    let scope = format!("repository:{repository}:pull");
    let url = format!("{realm}?service={service}&scope={scope}");
    request_token(client, &url).await
}

/// GET a token URL and extract the `token`/`access_token` field.
async fn request_token(client: &reqwest::Client, url: &str) -> Option<String> {
    let resp = client.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let json: Value = resp.json().await.ok()?;
    json.get("token")
        .or_else(|| json.get("access_token"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Parse a `key="value"` directive out of a WWW-Authenticate header.
fn extract_directive(header: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let start = header.find(&needle)? + needle.len();
    let rest = &header[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// From a manifest index, pick the digest matching this host's OS/architecture.
fn pick_platform_digest(index: &Value) -> Option<String> {
    let host_arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "arm" => "arm",
        other => other,
    };
    let manifests = index.get("manifests")?.as_array()?;
    // Prefer an exact linux/<arch> match.
    manifests
        .iter()
        .find(|m| {
            let p = m.get("platform");
            let arch = p.and_then(|p| p.get("architecture")).and_then(|a| a.as_str());
            let os = p.and_then(|p| p.get("os")).and_then(|o| o.as_str());
            arch == Some(host_arch) && os == Some("linux")
        })
        .or_else(|| manifests.first())
        .and_then(|m| m.get("digest"))
        .and_then(|d| d.as_str())
        .map(|s| s.to_string())
}

/// GET a URL and parse the JSON body, optionally with a token and Accept header.
async fn fetch_json(
    client: &reqwest::Client,
    url: &str,
    token: Option<&str>,
    accept: Option<&str>,
) -> Result<Value> {
    let mut req = client.get(url);
    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    if let Some(a) = accept {
        req = req.header("Accept", a);
    }
    let resp = req.send().await.context("registry request failed")?;
    if !resp.status().is_success() {
        anyhow::bail!("registry returned {} for {url}", resp.status());
    }
    let json: Value = resp.json().await.context("parsing registry JSON")?;
    Ok(json)
}

