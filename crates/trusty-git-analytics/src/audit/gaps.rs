//! What the audit could not assess, in the words the report uses (#5239, #5244).
//!
//! Why: DOC-67 §9 turns on one distinction — a dimension missing because a
//! stage failed must not look, on the page, like a dimension that came back
//! clean. The sweep already records every stage's fate ([`AuditSweepStats`]);
//! this module is where those records become sentences an acquirer's reviewer
//! reads, so the wording lives in one place instead of being formatted at the
//! call site.
//! What: [`sweep_gap_lines`] (one line per failed stage) and
//! [`DATA_HANDLING_NOTE`] (§10's placeholder attestation, #5244).
//! Test: `super::tests`.
//!
//! ## Redaction happens here, before the excerpt is cut
//!
//! A stage failure carries an `anyhow` cause chain the process did not author,
//! so it can quote a credential back at us. This module both scrubs and
//! truncates that text, in that order and in one function, because doing them
//! in the other order leaks: [`trusty_common::credentials::scrub_secrets`]
//! matches a credential's *whole* value, so a token that starts before the
//! excerpt boundary and ends after it survives the cut as an unmatchable prefix
//! fragment, and every later scrub — including
//! [`crate::report::dd_manifest::build_dd_manifest`]'s — sees only that
//! fragment and passes it through into `manifest.toml` and the delivered report.
//!
//! The invariant is therefore structural rather than a convention: the needle
//! set is a required argument of [`sweep_gap_lines`], so there is no way to
//! obtain a truncated stage message without having supplied the credentials to
//! remove from it first. `build_dd_manifest` still scrubs everything it emits —
//! that stays the manifest-wide guarantee, and re-scrubbing an already-clean
//! string is a no-op.

use std::collections::BTreeMap;

use trusty_common::credentials::scrub_secrets;

use super::repo_index::RepoIndexStatus;
use super::stage::{AuditSweepStats, StageStatus};

/// Longest stage-failure message carried into the report, in characters.
///
/// Why: an `anyhow` cause chain can run to several hundred characters of
/// transport detail that means nothing to the report's reader, and the Gaps
/// section is read under time pressure. The full message is already on stderr
/// and in the sweep's own record; this is the reader's excerpt.
pub(crate) const MAX_REASON_CHARS: usize = 160;

/// The placeholder data-retention statement AUDIT carries until #5218 ships.
///
/// Why: DOC-67 §10 — an acquirer's counterparty asks what the tool retained
/// before granting access, and #5218 is the authoritative mechanism for that
/// answer. Until it ships, the report must say an attestation is *pending*
/// rather than assert one, and must not paraphrase a claim it cannot yet
/// enforce.
/// What: states that the formal attestation is pending, and states §10's
/// verified scope claim exactly as §10 words it — "no file content, diffs,
/// patches, hunks, or blobs", never the broader "no code", because free-text
/// columns can carry whatever an author pasted into them.
/// Test: `super::tests::data_handling_note_is_a_pending_claim`.
pub const DATA_HANDLING_NOTE: &str = "Data handling: a formal data-retention attestation for \
this run is pending (#5218) and is not asserted here. tga's database records commit, \
pull-request, and ticket metadata; it stores no file content, diffs, patches, hunks, or blobs. \
Free-text fields it does store — commit messages, pull-request and ticket titles — are retained \
verbatim and carry whatever their authors wrote into them.";

/// The words a stale-refs gap line opens with (#6782).
///
/// Why: #5321 put the fallback on the page, but as one mid-list sentence in a
/// section a reader skims, so a due-diligence reader taking commit and PR
/// figures at face value had nothing to stop them. Leading with the verdict is
/// what makes it visible; exporting the phrase is what lets a downstream
/// consumer — `trusty-audit`'s run index — recognise the line instead of
/// re-deriving the wording.
/// What: the literal prefix every stale-refs line starts with, before the
/// parenthesised reason.
/// Test: `super::tests::a_stale_fetch_line_leads_with_the_headline`.
pub const STALE_FETCH_HEADLINE: &str = "git history is stale: fetch failed";

/// One Gaps & Caveats line per repository collected from stale local refs, then
/// one per stage that did not complete.
///
/// Why: a stage that failed took a whole class of data with it — no `dora` run
/// means no delivery-health figures, no `jira sync` means no ticket
/// correlation — and DOC-67 §9 requires that absence be stated, not inferred
/// from an empty table. The sweep deliberately does not abort on a stage
/// failure (§2, one shot), which is exactly why the failure has to reappear
/// here. A stale-refs fallback (#5321) is the same obligation one step
/// further in: the stage SUCCEEDED, so nothing about the data's age is visible
/// anywhere on the page unless it is stated here.
/// What: for each unreachable remote FIRST (#6782), a line opening with
/// [`STALE_FETCH_HEADLINE`] and naming the repository, the remote, and that its
/// figures may be behind the true remote state; then, for each failure in
/// execution order, a line naming the stage, a redacted excerpt of the reason,
/// and the fact that the affected area is unassessed; then,
/// for each leg the config declared absent (#6130), a line naming the leg and
/// why it was never attempted. Returns an empty vec when every stage succeeded,
/// every remote was reached, and every leg ran — a clean run adds no line.
///
/// `secrets` are the credential values to remove from each stage message —
/// [`crate::report::dd_manifest::configured_secrets`] derives the set the same
/// audit run's manifest uses. It is a required argument, not a convenience: see
/// the module docs for why truncating before scrubbing leaks a prefix fragment.
/// A fetch error is scrubbed on the same path, and needs it just as much: git2
/// quotes the remote URL back, which for an HTTPS remote carries whatever
/// credential was embedded in it.
/// Test: `super::tests::{sweep_gap_lines_name_each_failed_stage,
/// sweep_gap_lines_are_empty_for_a_clean_run,
/// a_repo_that_fell_back_to_stale_local_refs_is_named_in_the_gap_lines,
/// a_stale_fetch_line_leads_with_the_headline}`, and
/// `crate::report::dd_manifest_tests::a_token_straddling_the_excerpt_boundary_leaves_no_fragment`.
pub fn sweep_gap_lines<S: AsRef<str>>(stats: &AuditSweepStats, secrets: &[S]) -> Vec<String> {
    let failed_stages = stats.failures().map(|outcome| {
        let reason = match &outcome.status {
            StageStatus::Failed(msg) => redacted_excerpt(msg, secrets),
            _ => String::new(),
        };
        format!(
            "Collection stage `{}` did not complete ({reason}) — the data it produces is \
             not assessed in this report. Read the affected sections as unassessed, not as \
             a clean result.",
            outcome.stage
        )
    });

    // #5321: worded from the collector's own log line, because an operator who
    // has the run log needs to match the two up without translating.
    // #6782: the verdict leads, and the line is emitted ahead of the stage
    // failures below — a reader who stops after the first bullet has still read
    // the one fact that invalidates the commit and PR figures.
    let stale = stats.stale_fetches.iter().map(|fetch| {
        let reason = redacted_excerpt(&fetch.error, secrets);
        format!(
            "**{STALE_FETCH_HEADLINE} ({reason})** — repository `{}` could not be fetched \
             from remote `{}`, so collection continued on stale local refs and its data may \
             be behind the true remote state. Read its figures as a snapshot of the local \
             clone, not of the remote.",
            fetch.repo, fetch.remote
        )
    });

    // #6130: a leg nobody attempted. Worded as a NOT-ATTEMPTED rather than a
    // failure, because the two mean different things to a reader deciding
    // whether to chase the missing data — and both end in the same instruction,
    // that the affected sections are unassessed.
    let declared = stats.declared_skips.iter().map(|skip| {
        let reason = redacted_excerpt(&skip.reason, secrets);
        format!(
            "Collection leg `{}` was not attempted ({reason}) — the data it produces is not \
             assessed in this report. Read the affected sections as unassessed, not as a \
             clean result.",
            skip.leg
        )
    });

    // #6782: stale-refs lines first. A failed stage leaves an EMPTY section,
    // which a reader notices; a stale fetch leaves a full one that is quietly
    // wrong, which they do not.
    stale.chain(failed_stages).chain(declared).collect()
}

/// One Gaps & Caveats line per distinct reason a repository could not be
/// indexed (#5670).
///
/// Why: a repository trusty-search does not serve reaches the renderer's
/// fail-open path — `AnalyzeGap::NotIndexed`, one generic line, exit 0 — and the
/// operator learns only that the index was missing, never that the audit tried
/// to build it and why that failed. DOC-67 §9's rule for a per-repository
/// failure is exclude-and-name, so the cause is named here, beside the stage
/// failures, in the same words the rest of the section uses.
/// What: groups the failed outcomes by reason so one fault affecting an entire
/// org is one line rather than two hundred, names the affected repositories in
/// manifest order inside each line, and returns an empty vec when every
/// repository is indexed. Reasons are scrubbed and excerpted by
/// [`redacted_excerpt`] — a child's message is text this process did not author,
/// and the manifest's own scrub cannot repair a credential cut in half here.
/// Test: `super::tests::{one_repository_that_fails_to_index_does_not_stop_the_others,
/// a_missing_search_binary_is_named_and_the_run_continues,
/// index_gap_lines_are_empty_when_every_repository_is_served,
/// a_credential_in_an_index_failure_never_reaches_the_gap_line}`.
pub fn index_gap_lines<S: AsRef<str>>(
    outcomes: &[super::repo_index::RepoIndexOutcome],
    secrets: &[S],
) -> Vec<String> {
    // BTreeMap, not HashMap: two runs over the same state must produce
    // byte-identical lines in the same order (DOC-67 §9).
    let mut by_reason: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for outcome in outcomes {
        if let RepoIndexStatus::Failed(reason) = &outcome.status {
            by_reason
                .entry(redacted_excerpt(reason, secrets))
                .or_default()
                .push(outcome.repo.clone());
        }
    }

    by_reason
        .into_iter()
        .map(|(reason, repos)| {
            format!(
                "trusty-search could not index {} ({reason}) — those applications are described \
                 from the repository scan alone. Their findings, complexity, and health factors \
                 are not assessed, not clean.",
                repos.join(", ")
            )
        })
        .collect()
}

/// A single-line, credential-free excerpt of `msg`, capped at
/// [`MAX_REASON_CHARS`] characters.
///
/// Why: newlines would break the Gaps bullet, and the cap keeps one verbose
/// transport error from dominating the section. Redaction and truncation are
/// one operation rather than two so their order cannot be got wrong at a call
/// site — see the module docs.
/// What: scrubs `secrets` out of the raw message first, then flattens
/// whitespace, then truncates. Scrubbing precedes flattening because a needle
/// is matched against the text as the failing stage produced it; flattening only
/// collapses whitespace runs, so it can never rejoin a split credential. The cap
/// applies to the redacted text, so `[REDACTED]` being longer than what it
/// replaces cannot push the excerpt over budget. Truncation is by character, so
/// the same message always yields the same excerpt.
/// Test: `super::tests::long_stage_reasons_are_truncated`.
fn redacted_excerpt<S: AsRef<str>>(msg: &str, secrets: &[S]) -> String {
    // #5239: scrub the full message, THEN cut. Cutting first would leave a
    // credential that spans the boundary behind as a prefix no later scrub can
    // match.
    let clean = scrub_secrets(msg, secrets);
    let flat = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= MAX_REASON_CHARS {
        return flat;
    }
    let head: String = flat.chars().take(MAX_REASON_CHARS).collect();
    format!("{head}…")
}
