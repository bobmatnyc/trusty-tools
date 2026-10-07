//! Installing the instructional content on tm's first use (#9396).
//!
//! Why: since #9136 no content is compiled in, and nothing else writes
//! `content-lock.toml` on a fresh install or an upgrade, so every session
//! failed to compose its PM instructions until the operator ran
//! `tm content update` by hand. ADR-0064 promises that tm fetches the pinned
//! release on first run, and says so when it cannot.
//! What: [`resolve_or_fetch`] resolves content; when nothing is installed
//! and no dev override applies, it fetches the release `tm content update`
//! would pick, through [`install_if_missing`], once, then resolves again. A
//! present lock never triggers a fetch. A failed fetch is
//! [`AgentContentError::FetchFailed`], which names `tm content update` and
//! the offline `--from` install; no unverified byte is ever served.
//! [`OFFLINE_ENV`] skips the fetch.
//! Test: `first_use_tests.rs`.
//!
//! # Spec References
//! - ADR-0064 decision 5: `docs/adr/0064-instructional-content-tracked-separately-from-code.md`

use std::path::Path;

use trusty_agents_common::agent_content::{
    AgentContentError, DevOverride, ResolvedContent, resolve_content_in,
};

use super::bundle_cache::{
    CacheError, Fallback, GithubReleases, UpdateOutcome, install_if_missing,
};

/// Set to anything but `0` or empty to skip the first-use fetch: the
/// not-installed error is returned as it is. Test harnesses set it so no
/// spawned `tm` reaches the network.
pub const OFFLINE_ENV: &str = "TRUSTY_CONTENT_OFFLINE";

/// Resolves content from the default cache, fetching the release on first use.
///
/// Why: the production entry point behind every PM-instruction composition
/// and the `tm install` gate (#9396).
/// What: [`AgentContentError::NoCacheDir`] with no home directory; with
/// [`OFFLINE_ENV`] set, plain resolution; otherwise [`resolve_or_fetch_in`]
/// against GitHub, the fetch running on its own thread so a caller inside an
/// async runtime cannot hit the blocking HTTP client's runtime panic.
/// Test: `missing_lock_fetches_the_release_once` (via
/// [`resolve_or_fetch_in`]); `offline_env_is_read_as_a_switch`.
pub fn resolve_or_fetch(dev: DevOverride) -> Result<ResolvedContent, AgentContentError> {
    let cache = trusty_common::content::default_cache_dir().ok_or(AgentContentError::NoCacheDir)?;
    if offline_from(std::env::var(OFFLINE_ENV).ok().as_deref()) {
        return resolve_content_in(&cache, dev);
    }
    resolve_or_fetch_in(&cache, dev, fetch_from_github)
}

/// [`install_if_missing`] against GitHub, on a thread of its own: the
/// blocking HTTP client panics when built or dropped inside a tokio runtime,
/// and daemon handlers reach this path.
fn fetch_from_github(cache: &Path) -> Result<Option<UpdateOutcome>, CacheError> {
    let owned = cache.to_path_buf();
    let thread_err = |source: std::io::Error| CacheError::Io {
        action: "fetch the content release into",
        path: cache.to_path_buf(),
        source,
    };
    std::thread::Builder::new()
        .name("content-first-use".to_owned())
        .spawn(move || {
            let source = GithubReleases::new().map_err(|e| CacheError::Network {
                url: e.url,
                reason: e.reason,
                fallback: Fallback::None,
            })?;
            install_if_missing(&owned, &source)
        })
        .map_err(thread_err)?
        .join()
        .map_err(|_| thread_err(std::io::Error::other("the fetch thread panicked")))?
}

/// [`resolve_or_fetch`] against an explicit cache, with the fetch injected.
///
/// Why: the fetch is the only network step, so tests drive it with a fake
/// release source and never touch the network.
/// What: resolves under `dev`. Only a `NotInstalled` answer calls `fetch`
/// (any other error, including an unreadable lock, is returned untouched).
/// `fetch` returns `Ok(None)` when another writer pinned first. After a
/// successful fetch, content is resolved again, so what is served is what
/// the resolver verified against the new lock.
/// Test: `missing_lock_fetches_the_release_once`, `present_lock_never_fetches`,
/// `missing_lock_offline_fails_closed_naming_update_and_from`,
/// `concurrent_first_use_leaves_one_valid_lock`.
pub fn resolve_or_fetch_in<F>(
    cache: &Path,
    dev: DevOverride,
    fetch: F,
) -> Result<ResolvedContent, AgentContentError>
where
    F: FnOnce(&Path) -> Result<Option<UpdateOutcome>, CacheError>,
{
    match resolve_content_in(cache, dev.clone()) {
        Err(AgentContentError::NotInstalled { .. }) => {}
        other => return other,
    }
    match fetch(cache) {
        Ok(Some(outcome)) => tracing::info!(
            tag = %outcome.tag,
            sha256 = %outcome.sha256,
            "no instructional content was installed; pinned {} on first use",
            outcome.tag
        ),
        // Another writer pinned while this one waited: serve that pin.
        Ok(None) => tracing::debug!("another writer pinned the content release first"),
        Err(e) => {
            return Err(AgentContentError::FetchFailed {
                reason: e.to_string(),
            });
        }
    }
    resolve_content_in(cache, dev)
}

/// Whether an [`OFFLINE_ENV`] value switches the fetch off.
fn offline_from(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.trim().is_empty() && v.trim() != "0")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #9396: the switch reads as off for unset, empty and `0`.
    #[test]
    fn offline_env_is_read_as_a_switch() {
        assert!(!offline_from(None));
        assert!(!offline_from(Some("")));
        assert!(!offline_from(Some("0")));
        assert!(offline_from(Some("1")));
        assert!(offline_from(Some("yes")));
    }
}
