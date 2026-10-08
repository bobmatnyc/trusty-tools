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
//! the manual install; no unverified byte is ever served. A failed fetch is
//! not retried for [`RETRY_AFTER`], and the fetch never holds a tokio worker.
//! [`OFFLINE_ENV`] skips the fetch; a miss under it names the switch.
//! Test: `first_use_tests.rs`.
//!
//! # Spec References
//! - ADR-0064 decision 5: `docs/adr/0064-instructional-content-tracked-separately-from-code.md`

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use trusty_agents_common::agent_content::{
    AgentContentError, DevOverride, ResolvedContent, resolve_content_in,
};

use super::bundle_cache::{CacheError, Fallback, UpdateOutcome, github_source, install_if_missing};

/// Set to anything but `0` or empty to skip the first-use fetch: a missing
/// lock is then an error naming this switch and the remedies. Test harnesses
/// set it so no spawned `tm` reaches the network.
pub const OFFLINE_ENV: &str = "TRUSTY_CONTENT_OFFLINE";

/// How long a failed first-use fetch is not retried (#9396).
pub const RETRY_AFTER: Duration = Duration::from_secs(60);

/// Resolves content from the default cache, fetching the release on first use.
///
/// Why: the production entry point behind every PM-instruction composition
/// and the `tm install` gate (#9396).
/// What: [`AgentContentError::NoCacheDir`] with no home directory; otherwise
/// [`resolve_or_fetch_at`] on the default cache.
/// Test: `missing_lock_fetches_the_release_once` (via
/// [`resolve_or_fetch_in`]); `offline_env_is_read_as_a_switch`.
pub fn resolve_or_fetch(dev: DevOverride) -> Result<ResolvedContent, AgentContentError> {
    let cache = trusty_common::content::default_cache_dir().ok_or(AgentContentError::NoCacheDir)?;
    resolve_or_fetch_at(&cache, dev)
}

/// [`resolve_or_fetch`] against an explicit cache.
///
/// What: with [`OFFLINE_ENV`] set, [`resolve_offline_in`]; otherwise
/// [`resolve_or_fetch_in`] against GitHub, the fetch running on its own
/// thread so a caller inside an async runtime cannot hit the blocking HTTP
/// client's runtime panic.
pub fn resolve_or_fetch_at(
    cache: &Path,
    dev: DevOverride,
) -> Result<ResolvedContent, AgentContentError> {
    if let Some(value) = std::env::var(OFFLINE_ENV)
        .ok()
        .filter(|v| offline_from(Some(v)))
    {
        return resolve_offline_in(cache, dev, &value);
    }
    resolve_or_fetch_in(cache, dev, fetch_from_github)
}

/// The production fetch's signature.
#[cfg(test)]
type FetchFn = fn(&Path) -> Result<Option<UpdateOutcome>, CacheError>;

#[cfg(test)]
thread_local! {
    /// Replaces the GitHub fetch on this thread, so a test can prove a
    /// production path never reaches it (#9396).
    pub(crate) static FETCH_OVERRIDE: std::cell::Cell<Option<FetchFn>> =
        const { std::cell::Cell::new(None) };
}

/// Resolves with the first-use fetch switched off by `OFFLINE_ENV=value`.
///
/// Why: a plain not-installed error under the switch hides why tm did not
/// fetch, so the operator cannot tell a switched-off fetch from a missing
/// one (#9396).
/// What: a `NotInstalled` answer becomes [`AgentContentError::FetchFailed`]
/// whose reason names the switch and says to unset it; the error's remedy
/// names `tm content update` and the offline `tm content install --from`.
/// Any other result is returned as it is.
/// Test: `sessions_start_offline_names_the_switch_and_both_remedies`.
fn resolve_offline_in(
    cache: &Path,
    dev: DevOverride,
    value: &str,
) -> Result<ResolvedContent, AgentContentError> {
    match resolve_content_in(cache, dev) {
        // #9396: name the switch, so a skipped fetch never reads as a missing one.
        Err(AgentContentError::NotInstalled { .. }) => Err(AgentContentError::FetchFailed {
            reason: format!(
                "`{OFFLINE_ENV}={}` is set, which turns the first-use fetch off \
                 (unset it to let tm fetch the release itself)",
                value.trim()
            ),
        }),
        other => other,
    }
}

/// [`install_if_missing`] against GitHub, on a thread of its own: the
/// blocking HTTP client panics when built or dropped inside a tokio runtime,
/// and daemon handlers reach this path.
fn fetch_from_github(cache: &Path) -> Result<Option<UpdateOutcome>, CacheError> {
    #[cfg(test)]
    if let Some(fake) = FETCH_OVERRIDE.with(std::cell::Cell::get) {
        return fake(cache);
    }
    let owned = cache.to_path_buf();
    let thread_err = |source: std::io::Error| CacheError::Io {
        action: "fetch the content release into",
        path: cache.to_path_buf(),
        source,
    };
    std::thread::Builder::new()
        .name("content-first-use".to_owned())
        .spawn(move || {
            // #9396: retried on a 5xx, authenticated through `gh` when it can.
            let source = github_source().map_err(|e| CacheError::Network {
                url: e.url,
                reason: e.reason,
                tag: None,
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
/// What: [`resolve_or_fetch_with`] with the process-wide failure memo and
/// the current time.
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
    static FAILURES: LazyLock<FailureMemo> = LazyLock::new(|| FailureMemo::new(RETRY_AFTER));
    resolve_or_fetch_with(cache, dev, fetch, &FAILURES, Instant::now)
}

/// The body of [`resolve_or_fetch_in`], with the memo and the clock injected.
///
/// What: resolves under `dev`. Only a `NotInstalled` answer may fetch (any
/// other error, including an unreadable lock, is returned untouched). A fetch
/// for this cache that failed within the memo's window is not retried: the
/// answer is `FetchFailed` naming that failure. The memo never serves
/// content — a bundle installed meanwhile resolves first. Otherwise, off the
/// tokio worker ([`off_worker`]), [`fetch_serialized`] runs the fetch.
/// Test: `a_failed_fetch_is_not_retried_within_the_window`,
/// `a_fetch_inside_a_runtime_leaves_the_worker_free`.
pub(crate) fn resolve_or_fetch_with<F>(
    cache: &Path,
    dev: DevOverride,
    fetch: F,
    memo: &FailureMemo,
    now: impl Fn() -> Instant,
) -> Result<ResolvedContent, AgentContentError>
where
    F: FnOnce(&Path) -> Result<Option<UpdateOutcome>, CacheError>,
{
    match resolve_content_in(cache, dev.clone()) {
        Err(AgentContentError::NotInstalled { .. }) => {}
        other => return other,
    }
    // #9396: a dead network is not re-hit by every composition.
    memo.refuse_recent(cache, now())?;
    off_worker(|| fetch_serialized(cache, dev, fetch, memo, &now))
}

/// Runs one first-use fetch per cache at a time.
///
/// Why: callers that queued behind a failing fetch each ran their own full
/// fetch during an outage (#9396).
/// What: holds the cache's gate from [`FailureMemo::gate`], then resolves and
/// reads the memo again, so a caller that waited serves what the fetch
/// before it installed, or that fetch's failure. Otherwise `fetch` runs;
/// `Ok(None)` means another writer pinned first. A failure is recorded at
/// `now()` read after the fetch returned. After a success, content is
/// resolved again, so what is served is what the resolver verified against
/// the new lock.
/// Test: `a_slow_failed_fetch_is_memoized_from_when_it_failed`,
/// `callers_queued_behind_a_failing_fetch_do_not_fetch_again`.
fn fetch_serialized<F>(
    cache: &Path,
    dev: DevOverride,
    fetch: F,
    memo: &FailureMemo,
    now: &impl Fn() -> Instant,
) -> Result<ResolvedContent, AgentContentError>
where
    F: FnOnce(&Path) -> Result<Option<UpdateOutcome>, CacheError>,
{
    let gate = memo.gate(cache);
    let _held = gate.lock().unwrap_or_else(PoisonError::into_inner);
    match resolve_content_in(cache, dev.clone()) {
        Err(AgentContentError::NotInstalled { .. }) => {}
        other => return other,
    }
    memo.refuse_recent(cache, now())?;
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
            let reason = e.to_string();
            // #9396: the window starts when the fetch failed, not when it began.
            memo.record(cache, now(), &reason);
            return Err(AgentContentError::FetchFailed { reason });
        }
    }
    memo.clear(cache);
    resolve_content_in(cache, dev)
}

/// Runs `work`, which blocks, without holding a tokio worker (#9396).
///
/// Why: daemon handlers resolve content on a runtime worker, and the fetch
/// can block for minutes on a hung network; a blocked worker stalls every
/// task queued behind it.
/// What: on a multi-thread runtime, `block_in_place` hands the worker's
/// queue to another thread first; anywhere else `work` runs as is (a
/// current-thread runtime cannot hand its queue off).
/// Test: `a_fetch_inside_a_runtime_leaves_the_worker_free`.
fn off_worker<T>(work: impl FnOnce() -> T) -> T {
    use tokio::runtime::{Handle, RuntimeFlavor};
    match Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(work)
        }
        _ => work(),
    }
}

/// Recent first-use fetch failures, per cache directory (#9396).
///
/// Why: with no content, every composition retries the fetch, and each
/// retry against a dead network blocks for the full HTTP timeout.
/// What: `record` stores the failure and when it happened; `refuse_recent`
/// answers it while it is younger than `window`; `clear` drops it after a
/// success; `gate` is the per-cache lock that serializes fetches. It only
/// suppresses a retry; it holds no content.
/// Test: `a_failed_fetch_is_not_retried_within_the_window`,
/// `callers_queued_behind_a_failing_fetch_do_not_fetch_again`.
#[derive(Debug)]
pub(crate) struct FailureMemo {
    window: Duration,
    failures: Mutex<HashMap<PathBuf, (Instant, String)>>,
    gates: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
}

impl FailureMemo {
    /// A memo that suppresses retries for `window` after a failure.
    pub(crate) fn new(window: Duration) -> Self {
        Self {
            window,
            failures: Mutex::new(HashMap::new()),
            gates: Mutex::new(HashMap::new()),
        }
    }

    /// The lock one fetch into `cache` holds while it runs.
    fn gate(&self, cache: &Path) -> Arc<Mutex<()>> {
        let mut gates = self.gates.lock().unwrap_or_else(PoisonError::into_inner);
        gates.entry(cache.to_path_buf()).or_default().clone()
    }

    /// `FetchFailed` naming the failure recorded for `cache`, while it is
    /// younger than the window at `now`.
    fn refuse_recent(&self, cache: &Path, now: Instant) -> Result<(), AgentContentError> {
        let failures = self.failures.lock().unwrap_or_else(PoisonError::into_inner);
        match failures
            .get(cache)
            .filter(|(at, _)| now.saturating_duration_since(*at) < self.window)
        {
            Some((_, reason)) => Err(AgentContentError::FetchFailed {
                reason: format!(
                    "{reason} (a fetch under {} s ago failed, so tm did not retry yet)",
                    self.window.as_secs()
                ),
            }),
            None => Ok(()),
        }
    }

    fn record(&self, cache: &Path, now: Instant, reason: &str) {
        let mut failures = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        failures.insert(cache.to_path_buf(), (now, reason.to_owned()));
    }

    fn clear(&self, cache: &Path) {
        let mut failures = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        failures.remove(cache);
    }
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
