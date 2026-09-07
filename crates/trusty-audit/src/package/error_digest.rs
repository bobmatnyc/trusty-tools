//! Every failure and degradation the run recorded, as one machine-readable
//! member of the return package.
//!
//! Why (#6032): the owner's directive is "let's also include any errors in the
//! log so that the audit runner can send it back to us for fixing." A run
//! recorded its failures in four different shapes and the recipient could reach
//! none of them as a set. A repository that failed appeared in `package.toml`
//! and in `failures/index.md` as prose; a leg that degraded appeared as one
//! line inside the report's Gaps & Caveats; a board the sweep could not collect
//! and a config key this version does not act on appeared only in the
//! recipient's `excluded` list. Reading the whole picture meant opening four
//! files and correlating them by eye, which is what an auditor asking "what
//! went wrong on your machine" has to do at a distance.
//!
//! What: [`render`] flattens all four into one JSON document,
//! [`super::DIGEST_ENTRY`], with a uniform entry shape — the phase that
//! recorded it, whether the run stopped or continued, the target it concerns,
//! when it was recorded, and the message itself. It derives from the sweep's
//! own record rather than from a separate journal, so a degradation in the
//! report's Gaps & Caveats and its digest entry are the SAME string from the
//! same run, not two renderings that can come to disagree.
//!
//! ## Written even when nothing went wrong
//!
//! A run with no failures ships a digest whose `entries` array is empty. That
//! is the same choice [`super::DEBT_ENTRY`] makes and for the same reason: an
//! empty array states that the collector ran and found nothing, where an absent
//! file leaves the recipient unable to tell that from a client too old to
//! write one. It is the opposite choice from `failures/index.md`, which is
//! absent on a clean sweep — that member is prose a human reads, and a page
//! saying "none" is worse than no page; this one is a record a program reads.
//!
//! ## Scrubbed here, refused in `fill_archive`
//!
//! Every message is a string ANOTHER program produced — a child's exit text, a
//! provider's error body, a leg's failure cause — so it can carry a credential
//! the way any other quoted output can. Two guards, in this order:
//!
//! - [`trusty_common::credentials::scrub_secrets`] replaces each configured
//!   secret and the `gh`-derived token with `[REDACTED]` as the entry is built.
//!   Scrubbing rather than refusing is deliberate for this member alone: its
//!   whole purpose is to carry error text back, and refusing the entire package
//!   because one message quoted the key would leave the auditor with neither
//!   the package nor the digest.
//! - [`super::credential_scan::refuse_if_credential`] then scans the rendered
//!   document in [`super::fill_archive`], exactly as it scans every other
//!   generated member. It is not redundant: `scrub_secrets` declines a needle
//!   shorter than its minimum, so a short secret survives the scrub and the
//!   refusal is what still catches it.
//!
//! Test: `super::package_tests`.

use serde::Serialize;

use crate::config::EngagementConfig;
use crate::error::AuditError;
use crate::run::{RepoResult, RepoRun, RunReport};

/// The phase of THIS client's pipeline that recorded an entry.
///
/// Not one of `tga audit`'s nine stages: those are the child's, and by the time
/// a package is assembled the child's stage boundaries have been collapsed into
/// its exit status and the gap lines its manifest states. These five are the
/// phases this crate itself can attribute a record to.
mod stage {
    /// A target that never reached the sweep — it did not clone.
    pub(super) const CLONE: &str = "clone";
    /// Resolving the engagement's registered work-item boards.
    pub(super) const BOARDS: &str = "boards";
    /// One repository's `tga audit` child, and the legs that run around it.
    pub(super) const REPOSITORY: &str = "repository";
    /// The engagement config, as this version of the client reads it.
    pub(super) const CONFIG: &str = "config";
}

/// Whether the run stopped for this entry or carried on with less.
mod kind {
    /// The run did not produce this target's report.
    pub(super) const FAILURE: &str = "failure";
    /// The run continued, with a dimension it could not assess.
    pub(super) const DEGRADATION: &str = "degradation";
    /// The target was never attempted, so there is no result to degrade.
    pub(super) const NOT_ATTEMPTED: &str = "not-attempted";
    /// The config asked for something this version does not act on.
    pub(super) const NOT_ACTED_ON: &str = "not-acted-on";
}

/// The `errors/digest.json` document.
#[derive(Debug, Serialize)]
struct ErrorDigest {
    generated_by: String,
    /// When the digest was written — the package's clock, not the sweep's.
    generated_at: String,
    /// Every failure and degradation, in pipeline order. Empty on a clean run.
    entries: Vec<DigestEntry>,
}

/// One thing that went wrong, in the shape every reader gets.
#[derive(Debug, Serialize)]
struct DigestEntry {
    /// Which phase recorded it — see [`stage`].
    stage: &'static str,
    /// Whether the run stopped for it — see [`kind`].
    kind: &'static str,
    /// When the phase recorded it. The repository's own completion time where
    /// the sweep recorded one; otherwise the digest's `generated_at`, which is
    /// the case for a run-wide entry and for a checkpoint written before
    /// [`RepoRun::finished_at`] existed.
    at: String,
    /// The repository or board this concerns, absent for a run-wide entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<String>,
    /// What went wrong, scrubbed of every credential this process can name.
    message: String,
}

/// Flatten everything the run recorded into the digest document.
///
/// # Postconditions
/// On `Ok`, the returned text is a JSON object carrying an `entries` array —
/// empty when the run recorded nothing, never absent — in which no configured
/// secret and no `gh`-derived token appears in a scrubbable form.
///
/// What: one entry per failed repository, one per gap line any repository
/// stated, one per board gap, one per `unattempted` target, and one per config
/// key this version does not act on.
///
/// `github_token` is the same needle [`super::credential_scan::secret_needles`]
/// takes, and for the same reason: it reaches this process from the recipient's
/// `gh` keychain, so [`EngagementConfig::configured_secrets`] cannot name it.
/// Test: `super::package_tests::the_package_carries_an_error_digest_on_a_clean_run`,
/// `super::package_tests::a_stage_failure_reaches_the_error_digest`,
/// `super::package_tests::a_credential_in_a_digest_message_is_scrubbed_rather_than_refused`,
/// `super::package_tests::the_error_digest_leaves_the_rest_of_the_package_unchanged`.
///
/// # Errors
///
/// [`AuditError::Package`] when the document cannot be serialised, which needs
/// a `serde_json` failure over plain owned strings.
pub(super) fn render(
    report: &RunReport,
    config: &EngagementConfig,
    unattempted: &[String],
    github_token: Option<&str>,
) -> Result<String, AuditError> {
    let generated_at = crate::index_report::local_now();
    let mut needles: Vec<String> = config
        .configured_secrets()
        .into_iter()
        .map(str::to_owned)
        .collect();
    if let Some(token) = github_token {
        needles.push(token.to_owned());
    }
    let scrub = |message: &str| trusty_common::credentials::scrub_secrets(message, &needles);

    let mut entries = Vec::new();
    for line in unattempted {
        entries.push(DigestEntry {
            stage: stage::CLONE,
            kind: kind::NOT_ATTEMPTED,
            at: generated_at.clone(),
            target: None,
            message: scrub(line),
        });
    }
    for gap in &report.board_gaps {
        entries.push(DigestEntry {
            stage: stage::BOARDS,
            kind: kind::DEGRADATION,
            at: generated_at.clone(),
            target: None,
            message: scrub(gap),
        });
    }
    for run in &report.repos {
        entries.extend(repository_entries(run, &generated_at, &scrub));
    }
    for key in config.unsupported_keys() {
        entries.push(DigestEntry {
            stage: stage::CONFIG,
            kind: kind::NOT_ACTED_ON,
            at: generated_at.clone(),
            target: None,
            message: format!(
                "the engagement config declares `{key}` — this version of trusty-audit does not \
                 act on it"
            ),
        });
    }

    let digest = ErrorDigest {
        generated_by: format!("trusty-audit {}", env!("CARGO_PKG_VERSION")),
        generated_at,
        entries,
    };
    serde_json::to_string_pretty(&digest).map_err(|e| AuditError::Package {
        path: std::path::PathBuf::from(super::DIGEST_ENTRY),
        source: std::io::Error::other(e),
    })
}

/// One repository's failure, then every gap it stated.
///
/// The failure and the gaps carry the SAME `stage`, because by package time
/// they are indistinguishable in provenance — `RepoRun::gaps` merges what the
/// child's manifest stated with what the grounding, inference-record and OSV
/// legs appended after it, and there is no marker that separates them. `kind`
/// is what tells a reader whether the run stopped: a repository can state gaps
/// and still succeed, which is the ordinary case (#6078).
fn repository_entries(
    run: &RepoRun,
    generated_at: &str,
    scrub: &impl Fn(&str) -> String,
) -> Vec<DigestEntry> {
    let at = run
        .finished_at
        .clone()
        .unwrap_or_else(|| generated_at.to_owned());
    let mut entries = Vec::new();
    if let RepoResult::Failed { reason } = &run.result {
        entries.push(DigestEntry {
            stage: stage::REPOSITORY,
            kind: kind::FAILURE,
            at: at.clone(),
            target: Some(run.repo.name.clone()),
            message: scrub(reason),
        });
    }
    for gap in &run.gaps {
        entries.push(DigestEntry {
            stage: stage::REPOSITORY,
            kind: kind::DEGRADATION,
            at: at.clone(),
            target: Some(run.repo.name.clone()),
            message: scrub(gap),
        });
    }
    entries
}
