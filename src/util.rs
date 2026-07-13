//! Small pure helpers, kept free of I/O so they are easy to unit-test.

use humansize::{format_size, DECIMAL};

/// Format a byte count as a short human-readable string (e.g. `1.5 GB`).
pub fn format_bytes(bytes: i64) -> String {
    if bytes < 0 {
        return "-".to_string();
    }
    format_size(bytes as u64, DECIMAL)
}

/// A parsed OCI image reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
    pub registry: String,
    pub repository: String,
    pub tag: String,
}

impl ImageRef {
    /// Parse an image reference like `ghcr.io/owner/app:1.2` or `redis`.
    ///
    /// Applies Docker Hub normalization: bare names get the `library/` prefix
    /// and the `index.docker.io` registry; a missing tag defaults to `latest`.
    pub fn parse(reference: &str) -> Self {
        // Split off digest if present (`repo@sha256:...`) — tag/digest both dropped for repo.
        let (ref_no_digest, _digest) = match reference.split_once('@') {
            Some((r, d)) => (r, Some(d)),
            None => (reference, None),
        };

        // Determine registry vs repository by inspecting the first path segment.
        let first_segment = ref_no_digest.split('/').next().unwrap_or("");
        let has_registry = first_segment.contains('.')
            || first_segment.contains(':')
            || first_segment == "localhost";

        let (registry, remainder) = if has_registry {
            let (reg, rest) = ref_no_digest.split_once('/').unwrap_or((first_segment, ""));
            (reg.to_string(), rest.to_string())
        } else {
            ("index.docker.io".to_string(), ref_no_digest.to_string())
        };

        // Split repository:tag on the LAST segment only (registry may contain a port colon).
        let (repository, tag) = match remainder.rsplit_once(':') {
            // Guard against the colon belonging to a path (it never should after split above).
            Some((repo, tag)) if !tag.contains('/') => (repo.to_string(), tag.to_string()),
            _ => (remainder.clone(), "latest".to_string()),
        };

        // Docker Hub official images need the library/ prefix.
        let repository = if registry == "index.docker.io" && !repository.contains('/') {
            format!("library/{repository}")
        } else {
            repository
        };

        ImageRef {
            registry,
            repository,
            tag,
        }
    }

    /// Best-guess GitHub `owner/repo` for changelog lookup.
    ///
    /// - `ghcr.io/owner/repo` -> `owner/repo`
    /// - `library/redis` -> `redis/redis`
    /// - `owner/app` -> `owner/app`
    pub fn guess_github_repo(&self) -> Option<String> {
        if self.registry == "ghcr.io" {
            return Some(self.repository.clone());
        }
        if let Some(name) = self.repository.strip_prefix("library/") {
            return Some(format!("{name}/{name}"));
        }
        if self.repository.contains('/') {
            return Some(self.repository.clone());
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_official_image() {
        let r = ImageRef::parse("redis");
        assert_eq!(r.registry, "index.docker.io");
        assert_eq!(r.repository, "library/redis");
        assert_eq!(r.tag, "latest");
    }

    #[test]
    fn parses_user_image_with_tag() {
        let r = ImageRef::parse("grafana/grafana:10.2.0");
        assert_eq!(r.registry, "index.docker.io");
        assert_eq!(r.repository, "grafana/grafana");
        assert_eq!(r.tag, "10.2.0");
    }

    #[test]
    fn parses_ghcr_with_registry() {
        let r = ImageRef::parse("ghcr.io/owner/app:1.2.3");
        assert_eq!(r.registry, "ghcr.io");
        assert_eq!(r.repository, "owner/app");
        assert_eq!(r.tag, "1.2.3");
    }

    #[test]
    fn parses_registry_with_port() {
        let r = ImageRef::parse("localhost:5000/team/app:dev");
        assert_eq!(r.registry, "localhost:5000");
        assert_eq!(r.repository, "team/app");
        assert_eq!(r.tag, "dev");
    }

    #[test]
    fn guesses_github_repo() {
        assert_eq!(
            ImageRef::parse("ghcr.io/owner/app:1").guess_github_repo(),
            Some("owner/app".to_string())
        );
        assert_eq!(
            ImageRef::parse("redis").guess_github_repo(),
            Some("redis/redis".to_string())
        );
    }

    #[test]
    fn formats_bytes_readably() {
        assert_eq!(format_bytes(0), "0 B");
        assert!(format_bytes(1_500_000_000).contains("GB"));
    }
}
