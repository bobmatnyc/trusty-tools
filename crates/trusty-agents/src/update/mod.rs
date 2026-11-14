//! Version update checking against GitHub releases (#368).
//!
//! Why: Users running trusty-agents from `cargo install` or the om wrapper
//! don't get update notifications automatically. A lightweight background
//! check on startup surfaces new releases without blocking the REPL.
//! What: `check_for_update()` GETs the GitHub releases API with a 10s
//! timeout, compares semver tags to CARGO_PKG_VERSION, returns Some if
//! newer. `UpdateInfo` carries the tag and release URL for display.
//! `check_for_update_on_startup()` is the automatic start-time check; it makes
//! no request when `TRUSTY_NO_UPDATE_CHECK` or `CI` is set (#11075). The
//! explicit `/update` command keeps calling `check_for_update()`, which always
//! checks, as `trusty_common::update` leaves its explicit paths unguarded.
//! Test: `parse_version_tag_strips_v_prefix`, `newer_version_detected`,
//! `startup_check_skips_network_when_opted_out`,
//! `explicit_check_reaches_network_when_opted_out`.

use serde::Deserialize;
use trusty_common::update::NO_UPDATE_CHECK_ENV;

/// Standard CI marker; set means "suppress the update check".
// See trusty_common::update::check_throttled — the shared guard keeps its own
// copy private, so this mirrors it.
const CI_ENV: &str = "CI";

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

/// Whether the environment opts out of update checks.
///
/// Why: DOC-90 §7 — every trusty binary honours one opt-out, and CI must
/// never reach the network for a notification nobody reads (#11075).
/// What: true when `TRUSTY_NO_UPDATE_CHECK` or `CI` holds a non-empty value,
/// matching `trusty_common::update::check_throttled`. `env` is the lookup, so
/// a test injects values instead of writing process-global env.
/// Test: `update_check_suppressed_matches_shared_guard`.
fn update_check_suppressed(env: impl Fn(&str) -> Option<String>) -> bool {
    [NO_UPDATE_CHECK_ENV, CI_ENV]
        .iter()
        .any(|key| env(key).is_some_and(|v| !v.is_empty()))
}

/// What started an update check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trigger {
    /// The automatic start-time check; honours the opt-out.
    Startup,
    /// The user's `/update` command; always checks.
    Explicit,
}

/// Check GitHub releases for a newer version. Non-blocking, 10s timeout.
/// Returns `Some(UpdateInfo)` when a newer release exists, `None` otherwise
/// (including network failures — the check is best-effort). Used by the
/// explicit `/update` command, so it ignores `TRUSTY_NO_UPDATE_CHECK` and `CI`.
pub async fn check_for_update() -> Option<UpdateInfo> {
    check_for_update_from(RELEASES_URL, Trigger::Explicit, |key| {
        std::env::var(key).ok()
    })
    .await
}

/// The automatic start-time update check.
///
/// Why: DOC-90 §7 — the unasked-for check must stay off the network when
/// `TRUSTY_NO_UPDATE_CHECK` or `CI` is set (#11075).
/// What: like [`check_for_update`], but returns `None` with no request when
/// the environment opts out.
/// Test: `startup_check_skips_network_when_opted_out`.
pub async fn check_for_update_on_startup() -> Option<UpdateInfo> {
    check_for_update_from(RELEASES_URL, Trigger::Startup, |key| {
        std::env::var(key).ok()
    })
    .await
}

/// Body of both checks with the URL, trigger and env lookup injected.
///
/// Why: lets a test point the request at a local listener and set the
/// opt-out without touching process env.
/// What: for [`Trigger::Startup`], returns `None` before building a client
/// when opted out; otherwise GETs `releases_url` and compares the tag to
/// `CARGO_PKG_VERSION`.
/// Test: `startup_check_skips_network_when_opted_out`,
/// `startup_check_reaches_network_when_not_opted_out`,
/// `explicit_check_reaches_network_when_opted_out`.
async fn check_for_update_from(
    releases_url: &str,
    trigger: Trigger,
    env: impl Fn(&str) -> Option<String>,
) -> Option<UpdateInfo> {
    // #11075: only the automatic check honours the opt-out; `/update` always checks.
    if trigger == Trigger::Startup && update_check_suppressed(env) {
        tracing::trace!("update check suppressed by env var");
        return None;
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent(USER_AGENT)
        .build()
        .ok()?;

    let release: GhRelease = client
        .get(releases_url)
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

    /// Env lookup over a fixed table, so no test writes process-global env.
    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_string())
        }
    }

    /// Local listener that counts accepted connections and drops each one,
    /// so a request fails fast instead of waiting out the 10s timeout.
    async fn counting_listener() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/releases/latest", listener.local_addr().unwrap());
        let hits = std::sync::Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                drop(stream);
            }
        });
        (url, hits)
    }

    #[test]
    fn update_check_suppressed_matches_shared_guard() {
        let cases: &[(&'static [(&'static str, &'static str)], bool)] = &[
            (&[], false),
            (&[(NO_UPDATE_CHECK_ENV, "1")], true),
            (&[(CI_ENV, "true")], true),
            (&[(NO_UPDATE_CHECK_ENV, ""), (CI_ENV, "")], false),
        ];
        for (pairs, want) in cases {
            assert_eq!(update_check_suppressed(env_of(pairs)), *want, "{pairs:?}");
        }
    }

    #[tokio::test]
    async fn startup_check_skips_network_when_opted_out() {
        for pairs in [&[(NO_UPDATE_CHECK_ENV, "1")][..], &[(CI_ENV, "true")][..]] {
            let (url, hits) = counting_listener().await;
            let got = check_for_update_from(&url, Trigger::Startup, env_of(pairs)).await;
            assert!(got.is_none());
            let n = hits.load(std::sync::atomic::Ordering::SeqCst);
            assert_eq!(
                n, 0,
                "{pairs:?} opted out, yet {n} connection(s) reached the listener"
            );
        }
    }

    /// Control: the listener does see a request when nothing opts out, so the
    /// zero-connection assertion above can fail.
    #[tokio::test]
    async fn startup_check_reaches_network_when_not_opted_out() {
        let (url, hits) = counting_listener().await;
        let got = check_for_update_from(&url, Trigger::Startup, env_of(&[])).await;
        assert!(got.is_none());
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// The explicit `/update` path is a user request, so the opt-out that
    /// silences the start-time check must not silence it (#11075).
    #[tokio::test]
    async fn explicit_check_reaches_network_when_opted_out() {
        for pairs in [&[(NO_UPDATE_CHECK_ENV, "1")][..], &[(CI_ENV, "true")][..]] {
            let (url, hits) = counting_listener().await;
            let got = check_for_update_from(&url, Trigger::Explicit, env_of(pairs)).await;
            assert!(got.is_none());
            let n = hits.load(std::sync::atomic::Ordering::SeqCst);
            assert_eq!(
                n, 1,
                "{pairs:?}: /update must still check, got {n} connection(s)"
            );
        }
    }

    #[test]
    fn same_or_older_not_newer() {
        assert!(!is_newer("0.8.3", "0.8.3"));
        assert!(!is_newer("0.7.0", "0.8.3"));
        assert!(!is_newer("0.8.2", "0.8.3"));
    }
}
