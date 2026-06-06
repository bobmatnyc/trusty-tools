//! Version update checking against GitHub releases (#368).
//!
//! Why: Users running trusty-agents from `cargo install` or the om wrapper
//! don't get update notifications automatically. A lightweight background
//! check on startup surfaces new releases without blocking the REPL.
//! What: `check_for_update()` GETs the GitHub releases API with a 10s
//! timeout, compares semver tags to CARGO_PKG_VERSION, returns Some if
//! newer. `UpdateInfo` carries the tag and release URL for display.
//! Test: `parse_version_tag_strips_v_prefix`, `newer_version_detected`.

use serde::Deserialize;

const RELEASES_URL: &str = "https://api.github.com/repos/bobmatnyc/trusty-agents/releases/latest";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const USER_AGENT: &str = concat!("trusty-agents/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub latest_version: String,
    pub release_url: String,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
}

/// Strip a leading `v` from a version tag, e.g. `v0.8.4` → `0.8.4`.
fn strip_v(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}

/// Simple semver comparison: split on `.`, compare u64 tuples.
/// Returns true if `latest > current`.
fn is_newer(latest: &str, current: &str) -> bool {
    fn parse(v: &str) -> (u64, u64, u64) {
        let parts: Vec<u64> = v
            .split('.')
            .take(3)
            .map(|p| p.parse().unwrap_or(0))
            .collect();
        (
            parts.first().copied().unwrap_or(0),
            parts.get(1).copied().unwrap_or(0),
            parts.get(2).copied().unwrap_or(0),
        )
    }
    parse(latest) > parse(current)
}

/// Check GitHub releases for a newer version. Non-blocking, 10s timeout.
/// Returns `Some(UpdateInfo)` when a newer release exists, `None` otherwise
/// (including network failures — the check is best-effort).
pub async fn check_for_update() -> Option<UpdateInfo> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent(USER_AGENT)
        .build()
        .ok()?;

    let release: GhRelease = client
        .get(RELEASES_URL)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;

    let latest = strip_v(&release.tag_name);
    if is_newer(latest, CURRENT_VERSION) {
        Some(UpdateInfo {
            latest_version: latest.to_string(),
            release_url: release.html_url,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_tag_strips_v_prefix() {
        assert_eq!(strip_v("v0.8.4"), "0.8.4");
        assert_eq!(strip_v("0.8.4"), "0.8.4");
    }

    #[test]
    fn newer_version_detected() {
        assert!(is_newer("0.9.0", "0.8.3"));
        assert!(is_newer("0.8.4", "0.8.3"));
        assert!(is_newer("1.0.0", "0.8.3"));
    }

    #[test]
    fn same_or_older_not_newer() {
        assert!(!is_newer("0.8.3", "0.8.3"));
        assert!(!is_newer("0.7.0", "0.8.3"));
        assert!(!is_newer("0.8.2", "0.8.3"));
    }
}
