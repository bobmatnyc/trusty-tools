//! Whether a pinned set COULD be installed here, without installing it (#5970).
//!
//! Why: `trusty-audit`'s cold-start launch has to tell a recipient which pinned
//! tools are installable before it commits them to a multi-tool download that
//! runs for minutes. Until this module the only way to learn that was to call
//! [`super::install_pinned_set`], which downloads — so the question could only
//! be answered by installing, or by a consumer growing a second resolver of its
//! own. A second resolver is the drift CLAUDE.md's common-entry-point rule
//! forbids, and it is what this module exists so nobody writes.
//!
//! What: [`preflight_pinned_set`] runs the first two of `stage_one`'s five
//! checks and stops. It downloads nothing, hashes nothing, and executes nothing.
//! [`resolve_pin`] IS those two checks, and `stage_one` calls the same function,
//! so a preflight cannot come to a different answer than the install it precedes.
//!
//! What a clean preflight does not promise: that the artifact downloads, that it
//! matches its published checksum, or that the binary reports its pin. Those are
//! checks 3 to 5, they need the bytes, and they stay in `stage_one`. A preflight
//! is "this pin resolves for this host", never "this install will succeed".
//!
//! Test: `super::tests::preflight_reports_every_tool_as_installable`,
//! `super::tests::preflight_names_the_tool_whose_version_was_never_published`,
//! `super::tests::preflight_downloads_nothing`.

use super::{Endpoints, PinnedError, PinnedTool};
use crate::download::{platform, release};

/// What a preflight found for one pinned tool.
///
/// Why: the caller renders a line per tool, so the answer travels per tool
/// rather than as one verdict for the set — an operator told "the set is not
/// installable" cannot act, and one told which tool and why can.
/// What: the pin, and the reason it could not be resolved when there is one.
/// `problem: None` is the installable case.
/// Test: `super::tests::preflight_names_the_tool_whose_version_was_never_published`.
#[derive(Debug)]
#[non_exhaustive]
pub struct PinnedPreflight {
    /// The crate that was checked.
    pub crate_name: String,
    /// The version that was pinned.
    pub version: String,
    /// Why this pin cannot be installed here, or `None` when it can.
    pub problem: Option<PinnedError>,
}

impl PinnedPreflight {
    /// Whether this pin resolved for this host.
    pub fn installable(&self) -> bool {
        self.problem.is_none()
    }
}

/// Check every pin in the set, without installing any of them.
///
/// Why: the set is reported WHOLE — one entry per input tool, even after an
/// earlier one failed. Stopping at the first problem would make a recipient
/// behind an egress proxy fix one tool, re-run, and meet the next; the whole
/// list in one pass is what lets them fix the network once.
///
/// # Postconditions
/// The returned vector has one entry per input tool, in order. Nothing was
/// downloaded and nothing was written anywhere.
///
/// What: [`resolve_pin`] per tool — a Tier-1 target check, then a release-list
/// lookup for the exact pinned version. Unlike [`super::install_pinned_set`]
/// this is NOT all-or-none: it makes no change to disk, so there is nothing to
/// roll back and no reason to hide the rest of the answer.
///
/// Test: `super::tests::preflight_reports_every_tool_as_installable`,
/// `super::tests::preflight_reports_every_tool_even_after_one_fails`.
pub async fn preflight_pinned_set(
    client: &reqwest::Client,
    tools: &[PinnedTool],
) -> Vec<PinnedPreflight> {
    preflight_pinned_set_at(client, &Endpoints::default(), tools).await
}

/// [`preflight_pinned_set`], against caller-supplied endpoints.
///
/// The same offline seam [`super::install_pinned_set_at`] uses, so every arm is
/// provable against the loopback fixture rather than against real GitHub.
pub(crate) async fn preflight_pinned_set_at(
    client: &reqwest::Client,
    endpoints: &Endpoints<'_>,
    tools: &[PinnedTool],
) -> Vec<PinnedPreflight> {
    let mut checked = Vec::with_capacity(tools.len());
    for tool in tools {
        checked.push(PinnedPreflight {
            crate_name: tool.crate_name.clone(),
            version: tool.version.clone(),
            problem: resolve_pin(client, endpoints, tool).await.err(),
        });
    }
    checked
}

/// Checks 1 and 2 of the pinned pipeline: a prebuilt exists for this host, and
/// the exact pinned version is published.
///
/// Why: shared by [`preflight_pinned_set`] and `stage_one` so the preflight and
/// the install that follows it cannot disagree. Two copies of this pair would
/// drift the moment either grew a case — and a preflight that says "installable"
/// over an install that then refuses is worse than no preflight.
///
/// # Postconditions
/// On `Ok`, `tool` names a Tier-1 target and a published stable release; nothing
/// was downloaded either way.
///
/// What: [`platform::current_target`], then
/// [`release::resolve_pinned_tag_from_url`] — never `latest`, per #5491.
///
/// Test: every `super::tests` case reaches it through one of the two callers.
///
/// # Errors
///
/// [`PinnedError::UnsupportedTarget`] off a Tier-1 host,
/// [`PinnedError::VersionNotPublished`] when the pin names no release, and
/// [`PinnedError::ReleaseLookupFailed`] when the list could not be read.
pub(super) async fn resolve_pin(
    client: &reqwest::Client,
    endpoints: &Endpoints<'_>,
    tool: &PinnedTool,
) -> Result<(&'static str, release::ResolvedTag), PinnedError> {
    let (name, version) = (tool.crate_name.as_str(), tool.version.as_str());

    // Check 1 — a prebuilt exists for this host. Unlike the latest path, an
    // unsupported target is terminal, not a cargo fallback.
    let target = platform::current_target().ok_or_else(|| PinnedError::UnsupportedTarget {
        crate_name: name.to_owned(),
        version: version.to_owned(),
        os: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
    })?;

    // Check 2 — the EXACT pinned version is published. #5491: never `latest`.
    // #6164: retried, because a release list read minutes after a publish can
    // answer without the new version in it.
    let resolved = resolve_published_tag(client, endpoints, name, version).await?;
    Ok((target, resolved))
}

/// How long to wait before each retry of the release-list lookup (#6164).
///
/// Why: `trusty-audit audit` failed hard about thirty minutes after
/// trusty-review 0.23.0 went live, reporting `0.22.1` as the newest published
/// version; a manual retry with no other change succeeded. Two attempts across
/// twenty seconds is what covers a cached list, and it is deliberately far
/// short of that thirty-minute window: an installer that blocks for half an
/// hour is a worse failure than one that stops and says to try again, which is
/// why [`PinnedError::VersionNotPublished`] also names the cause.
/// What: one entry per retry, so the total added wait is their sum. Zero in a
/// test build — every case that reaches this asks for a version the fixture
/// server will never publish, so real sleeps would only make the suite slower
/// without exercising anything.
#[cfg(not(test))]
const RETRY_AFTER: &[std::time::Duration] = &[
    std::time::Duration::from_secs(5),
    std::time::Duration::from_secs(15),
];
#[cfg(test)]
const RETRY_AFTER: &[std::time::Duration] = &[std::time::Duration::ZERO, std::time::Duration::ZERO];

/// The release-list lookup, retried while the answer looks like a stale list.
///
/// Why: see [`RETRY_AFTER`]. A version that was published minutes ago and a
/// version that never existed produce the identical answer from a release list,
/// so the only thing separating them is asking again.
/// What: [`release::resolve_pinned_tag_from_url`], retried per [`RETRY_AFTER`]
/// while [`worth_retrying`] holds. A transport failure is NOT retried — it is
/// already [`PinnedError::ReleaseLookupFailed`], which says the list could not
/// be read rather than that the version is missing, and a proxy refusing does
/// not start working in twenty seconds.
/// Test: `super::tests::a_pin_that_names_no_release_still_fails_after_retrying`.
async fn resolve_published_tag(
    client: &reqwest::Client,
    endpoints: &Endpoints<'_>,
    name: &str,
    version: &str,
) -> Result<release::ResolvedTag, PinnedError> {
    let mut waits = RETRY_AFTER.iter();
    loop {
        let error = match release::resolve_pinned_tag_from_url(
            client,
            endpoints.releases_url,
            name,
            version,
        )
        .await
        {
            Ok(resolved) => return Ok(resolved),
            Err(e) => e,
        };
        match waits.next() {
            Some(wait) if worth_retrying(&error) => tokio::time::sleep(*wait).await,
            _ => {
                return Err(match error {
                    release::ResolveError::NotPublished { available } => {
                        PinnedError::VersionNotPublished {
                            crate_name: name.to_owned(),
                            version: version.to_owned(),
                            available,
                        }
                    }
                    release::ResolveError::Fetch(source) => PinnedError::ReleaseLookupFailed {
                        crate_name: name.to_owned(),
                        version: version.to_owned(),
                        source,
                    },
                });
            }
        }
    }
}

/// Whether this lookup failure could be a release list that has not caught up.
///
/// Why: the closure condition #6164 states — retry when the version is absent
/// but the crate itself exists. A crate the list has never heard of is a typo
/// or a wrong crate name, and waiting twenty seconds to say so helps nobody.
/// What: true only for [`release::ResolveError::NotPublished`] carrying at
/// least one published version, which is how "this crate exists" is expressed
/// here.
/// Test: `super::tests::only_a_missing_version_of_a_known_crate_is_retried`.
pub(super) fn worth_retrying(error: &release::ResolveError) -> bool {
    matches!(error, release::ResolveError::NotPublished { available } if !available.is_empty())
}
