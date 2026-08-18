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
    let resolved =
        release::resolve_pinned_tag_from_url(client, endpoints.releases_url, name, version)
            .await
            .map_err(|e| match e {
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
            })?;
    Ok((target, resolved))
}
