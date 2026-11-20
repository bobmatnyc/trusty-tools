//! The oldest content release this binary reads (#11470).
//!
//! Why: trusty-mpm 1.14.8 required `output-styles/owner-plain.md`, which no
//! published release carried yet. A host pinned to content-v2.4.0 then failed
//! `tm fleet init` with a missing-file error naming `tm content update`, and
//! that command answered "already pinned and verified". The binary must state
//! the content it needs and check the pin against it before it reads a file.
//! What: [`MIN_CONTENT_TAG`]; [`is_below`] compares two tags by SemVer;
//! [`check_pin_with`] refuses an installed pin below the minimum with a remedy
//! that names `tm content update` only when a qualifying release is
//! published; [`check_pin`] is that check with the GitHub probe. A checkout
//! (dev override) is never checked: it serves its own working tree.
//! Test: `min_content_tests.rs`; the release gate is
//! `scripts/check-content-release-coverage.sh` (selftest beside it).

use trusty_common::content::{ContentSource, TAG_PREFIX};

use super::bundle_cache::{Fallback, github_source, latest_tag};
use crate::core::content_source::AgentContentError;

/// The oldest content release that carries every file this binary reads.
///
/// Raise it in the same PR that makes the binary require a file only a newer
/// release ships; `scripts/check-content-release-coverage.sh` blocks a
/// trusty-mpm release whose minimum, or whose required files, no published
/// release meets.
// #11470: content-v2.5.0 is the first release with `output-styles/owner-plain.md`.
pub const MIN_CONTENT_TAG: &str = "content-v2.5.0";

/// Whether `tag` is an older release than `minimum`.
///
/// What: compares the SemVer after [`TAG_PREFIX`]; a tag that does not parse
/// is below any minimum, so it is never accepted by mistake.
/// Test: `tags_compare_by_version_not_by_string`.
pub fn is_below(tag: &str, minimum: &str) -> bool {
    match (version(tag), version(minimum)) {
        (Some(tag), Some(minimum)) => tag < minimum,
        _ => true,
    }
}

fn version(tag: &str) -> Option<semver::Version> {
    semver::Version::parse(tag.strip_prefix(TAG_PREFIX)?).ok()
}

/// The newest published release, if it is at or above `minimum`; `Err` is
/// why the listing could not be read.
pub type ProbeResult = Result<Option<String>, String>;

/// Refuses an installed pin below `minimum`, before any file is read.
///
/// Why: a stale pin must read as a stale pin, with a remedy that works, not
/// as a missing file (#11470 close criterion 1).
/// What: `Ok` for a checkout and for a pin at or above `minimum`. Otherwise
/// [`AgentContentError::BelowMinimum`], running `probe` once to pick the
/// remedy: a qualifying release names `tm content update`; none says that no
/// release is published; a failed probe says why and names both.
/// Test: `a_stale_pin_names_the_update_when_a_qualifying_release_exists`,
/// `a_stale_pin_says_none_is_published_when_none_qualifies`,
/// `a_pin_at_the_minimum_and_a_checkout_pass_without_a_probe`.
pub fn check_pin_with(
    source: &ContentSource,
    minimum: &str,
    probe: impl FnOnce() -> ProbeResult,
) -> Result<(), AgentContentError> {
    let ContentSource::Installed { tag, .. } = source else {
        return Ok(());
    };
    if !is_below(tag, minimum) {
        return Ok(());
    }
    let remedy = match probe() {
        Ok(Some(newest)) => format!("run `tm content update` to pin {newest}"),
        Ok(None) => format!(
            "no content release at or above {minimum} is published yet, so `tm content \
             update` cannot fix this; install the previous tm release until one is"
        ),
        Err(reason) => format!(
            "tm could not check for a newer release ({reason}); run `tm content update` \
             once GitHub is reachable"
        ),
    };
    Err(AgentContentError::BelowMinimum {
        tag: tag.clone(),
        minimum: minimum.to_owned(),
        remedy,
    })
}

/// [`check_pin_with`] against [`MIN_CONTENT_TAG`] and the GitHub releases.
///
/// What: the probe runs only for a stale pin, on its own thread (the blocking
/// HTTP client panics inside a tokio runtime), and never with
/// `TRUSTY_CONTENT_OFFLINE` set.
pub fn check_pin(source: &ContentSource) -> Result<(), AgentContentError> {
    check_pin_with(source, MIN_CONTENT_TAG, probe_github)
}

fn probe_github() -> ProbeResult {
    #[cfg(test)]
    if let Some(fake) = PROBE_OVERRIDE.with(std::cell::Cell::get) {
        return fake();
    }
    if std::env::var(super::first_use::OFFLINE_ENV)
        .ok()
        .is_some_and(|v| super::first_use::offline_from(Some(&v)))
    {
        return Err(format!("`{}` is set", super::first_use::OFFLINE_ENV));
    }
    std::thread::spawn(|| {
        let source = github_source().map_err(|e| e.reason)?;
        let newest = latest_tag(&source, &Fallback::None).map_err(|e| e.to_string())?;
        Ok((!is_below(&newest, MIN_CONTENT_TAG)).then_some(newest))
    })
    .join()
    .unwrap_or_else(|_| Err("the release probe panicked".to_owned()))
}

#[cfg(test)]
thread_local! {
    /// Replaces the GitHub probe on this thread, so no test reaches the network.
    pub(crate) static PROBE_OVERRIDE: std::cell::Cell<Option<fn() -> ProbeResult>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
#[path = "min_content_tests.rs"]
mod tests;
