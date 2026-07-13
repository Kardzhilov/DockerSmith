//! Push notifications. Supports ntfy-style topic URLs and generic webhooks.

use anyhow::{Context, Result};

/// Send a notification with `title` and `body` to the configured URL.
///
/// For ntfy URLs (`https://ntfy.sh/<topic>`), the body is POSTed directly with a
/// `Title` header. For other URLs, a small JSON payload is POSTed.
pub async fn notify(url: &str, title: &str, body: &str) -> Result<()> {
    let client = reqwest::Client::builder()
        .user_agent("dockersmith")
        .build()
        .context("building HTTP client")?;

    let is_ntfy = url.contains("ntfy");
    let req = if is_ntfy {
        client
            .post(url)
            .header("Title", title)
            .header("Tags", "whale")
            .body(body.to_string())
    } else {
        client.post(url).json(&serde_json::json!({
            "title": title,
            "message": body,
        }))
    };

    let resp = req.send().await.context("sending notification")?;
    if !resp.status().is_success() {
        anyhow::bail!("notification endpoint returned {}", resp.status());
    }
    Ok(())
}
