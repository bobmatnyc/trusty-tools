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

use super::stage::{AuditSweepStats, StageStatus};

/// Longest stage-failure message carried into the report, in characters.
///
/// Why: an `anyhow` cause chain can run to several hundred characters of
/// transport detail that means nothing to the report's reader, and the Gaps
/// section is read under time pressure. The full message is already on stderr
/// and in the sweep's own record; this is the reader's excerpt.
const MAX_REASON_CHARS: usize = 160;

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

/// One Gaps & Caveats line per stage that did not complete.
///
/// Why: a stage that failed took a whole class of data with it — no `dora` run
/// means no delivery-health figures, no `jira sync` means no ticket
/// correlation — and DOC-67 §9 requires that absence be stated, not inferred
/// from an empty table. The sweep deliberately does not abort on a stage
/// failure (§2, one shot), which is exactly why the failure has to reappear
/// here.
/// What: for each failure in execution order, a line naming the stage, an
/// excerpt of the reason, and the fact that the affected area is unassessed.
/// Returns an empty vec when every stage succeeded — a clean run adds no line.
/// The reason is passed through verbatim (truncated); scrubbing credentials out
/// of it is [`crate::report::dd_manifest::build_dd_manifest`]'s job, applied to
/// every string that reaches the manifest rather than to this one channel.
/// Test: `super::tests::{sweep_gap_lines_name_each_failed_stage,
/// sweep_gap_lines_are_empty_for_a_clean_run}`.
pub fn sweep_gap_lines(stats: &AuditSweepStats) -> Vec<String> {
    stats
        .failures()
        .map(|outcome| {
            let reason = match &outcome.status {
                StageStatus::Failed(msg) => excerpt(msg),
                _ => String::new(),
            };
            format!(
                "Collection stage `{}` did not complete ({reason}) — the data it produces is \
                 not assessed in this report. Read the affected sections as unassessed, not as \
                 a clean result.",
                outcome.stage
            )
        })
        .collect()
}

/// A single-line excerpt of `msg`, capped at [`MAX_REASON_CHARS`] characters.
///
/// Why/What: newlines would break the Gaps bullet, and the cap keeps one
/// verbose transport error from dominating the section. Truncation is by
/// character, so the same message always yields the same excerpt.
/// Test: `super::tests::long_stage_reasons_are_truncated`.
fn excerpt(msg: &str) -> String {
    let flat = msg.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= MAX_REASON_CHARS {
        return flat;
    }
    let head: String = flat.chars().take(MAX_REASON_CHARS).collect();
    format!("{head}…")
}
