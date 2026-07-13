//! Self-update: replace the running binary with the latest GitHub release asset.
//!
//! Safe by construction: the new binary is written to a temp file next to the
//! current executable and atomically renamed over it only after a successful,
//! complete download.

use std::io::Write;

use anyhow::{Context, Result};

use crate::registry;

/// The GitHub repo DockerSmith releases are published to.
const RELEASE_REPO: &str = "Kardzhilov/DockerSmith";

/// Check for a newer release and, if found, download and swap the binary in place.
pub async fn run(token: Option<&str>) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    println!("DockerSmith {current} — checking for updates…");

    let releases = registry::fetch_releases(RELEASE_REPO, token, 1)
        .await
        .context("fetching latest release")?;
    let Some(latest) = releases.into_iter().next() else {
        println!("No releases published yet.");
        return Ok(());
    };

    let latest_tag = latest.tag_name.trim_start_matches('v');
    if latest_tag == current {
        println!("Already up to date ({current}).");
        return Ok(());
    }
    println!("New version available: {latest_tag} (current {current})");

    // Determine the asset URL for this platform.
    let target = current_target();
    let asset_name = format!("dockersmith-{target}-{}", latest.tag_name);
    let asset_url = format!(
        "https://github.com/{RELEASE_REPO}/releases/download/{}/{asset_name}",
        latest.tag_name
    );

    println!("Downloading {asset_name}…");
    let client = reqwest::Client::builder()
        .user_agent("dockersmith")
        .build()?;
    let resp = client.get(&asset_url).send().await.context("downloading asset")?;
    if !resp.status().is_success() {
        anyhow::bail!("no prebuilt asset for {target} ({})", resp.status());
    }
    let bytes = resp.bytes().await.context("reading asset bytes")?;

    // Write to a temp file beside the current executable, then rename over it.
    let exe = std::env::current_exe().context("locating current executable")?;
    let tmp = exe.with_extension("new");
    {
        let mut f = std::fs::File::create(&tmp)
            .with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(&bytes)?;
        f.flush()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = f.metadata()?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&tmp, perms)?;
        }
    }
    std::fs::rename(&tmp, &exe).context("replacing current executable")?;
    println!("Updated to {latest_tag}. Restart DockerSmith to use it.");
    Ok(())
}

/// The Rust target triple this binary was built for.
fn current_target() -> String {
    let arch = std::env::consts::ARCH;
    let os = std::env::consts::OS;
    match (arch, os) {
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu".to_string(),
        ("aarch64", "linux") => "aarch64-unknown-linux-gnu".to_string(),
        ("x86_64", "macos") => "x86_64-apple-darwin".to_string(),
        ("aarch64", "macos") => "aarch64-apple-darwin".to_string(),
        _ => format!("{arch}-{os}"),
    }
}
