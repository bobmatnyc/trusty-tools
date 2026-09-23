//! Titles, marker blocks and body rendering for `tm issue epic` (#8447).
//!
//! Why: the tracker body has three zones with three different maintenance
//! rules, and only one of them — the `phases` block — is machine-owned.
//! Everything outside the markers is authored prose that a regeneration must
//! not touch, which is why [`replace_block`] rebuilds the body from the
//! original segments rather than re-serialising it: the bytes outside the two
//! marker lines are the same bytes, not an equal-looking rendering of them.
//! What: the marker constants, [`replace_block`] with its refusal set
//! ([`BlockError`]), the `[EPIC …]` title grammar, the SHA-pinned plan
//! permalink, the phases-table renderer, and the tracker/phase body builders.
//! Test: `render_replaces_the_whole_phases_block`,
//! `sync_leaves_every_byte_outside_the_markers_identical`,
//! `render_refuses_a_body_with_no_markers`,
//! `render_refuses_a_body_with_two_start_markers`,
//! `render_refuses_an_empty_replacement_body`,
//! `render_escapes_a_pipe_in_a_phase_title`,
//! `tracker_body_links_the_plan_by_sha`, `phase_title_carries_both_numbers`,
//! `next_phase_number_never_reuses_a_deleted_number`.

use std::fmt::Write as _;

use super::backend::ChildIssue;
use super::plan::{EpicPlan, PhasePlan};

/// Opening marker of the machine-owned phases block.
pub(crate) const PHASES_START: &str = "<!-- phases:start -->";
/// Closing marker of the machine-owned phases block.
pub(crate) const PHASES_END: &str = "<!-- phases:end -->";
/// Opening marker of the deliberately-amended deferred block (D2).
pub(crate) const DEFERRED_START: &str = "<!-- deferred:start -->";
/// Closing marker of the deferred block.
pub(crate) const DEFERRED_END: &str = "<!-- deferred:end -->";
/// Opening marker of the follow-ups block (D2).
pub(crate) const FOLLOWUPS_START: &str = "<!-- followups:start -->";
/// Closing marker of the follow-ups block.
pub(crate) const FOLLOWUPS_END: &str = "<!-- followups:end -->";

/// The phases table's two header rows.
const PHASES_HEADER: &str =
    "| # | Phase | Issue | State | Gate |\n|---|-------|-------|-------|------|";

/// Why a body cannot be rewritten.
///
/// Why: every one of these is a case where the hand-run procedure would have
/// written something — an appended block, a body with two tables, an empty
/// body — and the resulting tracker would look plausible. D3 makes each of
/// them a refusal instead.
/// What: one variant per guard, each naming the marker at fault.
/// Test: the `render_refuses_*` tests.
#[derive(Debug, thiserror::Error)]
pub(crate) enum BlockError {
    /// The body carries no such marker line.
    #[error(
        "the body carries no `{0}` line — refusing to append a block to a tracker that declares \
         none, since a hand-authored body is not a tracker"
    )]
    Missing(&'static str),
    /// The body carries the marker more than once.
    #[error(
        "the body carries {count} `{marker}` lines — exactly one is required; refusing to guess \
         which block to replace"
    )]
    Duplicate {
        /// The repeated marker.
        marker: &'static str,
        /// How many times it appears.
        count: usize,
    },
    /// The closing marker precedes the opening one.
    #[error("the body's `{end}` line precedes its `{start}` line — refusing to rewrite it")]
    Inverted {
        /// The opening marker.
        start: &'static str,
        /// The closing marker.
        end: &'static str,
    },
    /// The rewrite produced nothing, or lost a marker.
    #[error(
        "the rewritten body is empty or lost the `{0}` marker — refusing to write it. This is \
         the failure that wiped #8445 when empty output was accepted as a body"
    )]
    LostContent(&'static str),
}

/// Replace everything between two marker lines, keeping every other byte.
///
/// Why: the one operation `sync` exists to perform, and the one with the
/// largest blast radius — a wrong rewrite silently destroys authored prose. It
/// therefore refuses rather than repairs: no marker pair, a duplicated marker,
/// or an empty result is an error, never an append.
/// What: splits `body` into newline-terminated segments, locates exactly one
/// `start` and one `end` segment, and concatenates
/// `[..=start] + content + [end..]`. The untouched segments are re-emitted
/// verbatim, so line endings, trailing whitespace and a missing final newline
/// all survive unchanged. `content` is normalised to exactly one trailing
/// newline so repeated runs converge.
/// Test: `render_replaces_the_whole_phases_block`,
/// `sync_leaves_every_byte_outside_the_markers_identical`,
/// `render_refuses_a_body_with_no_markers`,
/// `render_refuses_a_body_with_two_start_markers`,
/// `render_refuses_an_empty_replacement_body`.
pub(crate) fn replace_block(
    body: &str,
    start: &'static str,
    end: &'static str,
    content: &str,
) -> Result<String, BlockError> {
    let segments: Vec<&str> = body.split_inclusive('\n').collect();
    let start_idx = sole_index(&segments, start)?;
    let end_idx = sole_index(&segments, end)?;
    if end_idx < start_idx {
        return Err(BlockError::Inverted { start, end });
    }

    let mut out = String::with_capacity(body.len() + content.len());
    for segment in &segments[..=start_idx] {
        out.push_str(segment);
    }
    // The start marker is the last segment copied; a body whose start marker is
    // its final line has no newline yet, and the block needs one.
    if !out.ends_with('\n') {
        out.push('\n');
    }
    let trimmed = content.trim_end_matches('\n');
    if !trimmed.is_empty() {
        out.push_str(trimmed);
        out.push('\n');
    }
    for segment in &segments[end_idx..] {
        out.push_str(segment);
    }

    // #8447: the two guards the hand-run procedure learned the hard way. An
    // empty body, or one that lost a marker, is never written.
    if out.trim().is_empty() {
        return Err(BlockError::LostContent(start));
    }
    if !out.contains(start) {
        return Err(BlockError::LostContent(start));
    }
    if !out.contains(end) {
        return Err(BlockError::LostContent(end));
    }
    Ok(out)
}

/// The index of the one segment equal to `marker`, or the matching refusal.
fn sole_index(segments: &[&str], marker: &'static str) -> Result<usize, BlockError> {
    let hits: Vec<usize> = segments
        .iter()
        .enumerate()
        .filter(|(_, s)| s.trim() == marker)
        .map(|(i, _)| i)
        .collect();
    match hits.as_slice() {
        [] => Err(BlockError::Missing(marker)),
        [one] => Ok(*one),
        many => Err(BlockError::Duplicate {
            marker,
            count: many.len(),
        }),
    }
}

/// The tracker's title once its number is known.
///
/// `TICKETING.md`'s `epics.title_format` is authoritative: `[EPIC <n>] <outcome>`.
/// Test: `tracker_title_carries_the_number`.
pub(crate) fn tracker_title(epic: u64, outcome: &str) -> String {
    format!("[EPIC {epic}] {outcome}")
}

/// The title a tracker is FILED with, before its number is known.
///
/// Why: D1's two-step creation needs a title for the one call that returns the
/// number. It is replaced by [`tracker_title`] in the very next call, so no
/// phase ever derives its title from it.
/// Test: `tracker_title_carries_the_number`.
pub(crate) fn placeholder_tracker_title(outcome: &str) -> String {
    format!("[EPIC] {outcome}")
}

/// A phase issue's title.
///
/// Test: `phase_title_carries_both_numbers`.
pub(crate) fn phase_title(epic: u64, phase: u64, what: &str) -> String {
    format!("[EPIC_{epic} PHASE_{phase}] {what}")
}

/// Whether `title` is the tracker title for `number` carrying `outcome`.
///
/// Why: resume has to recognise a tracker this command itself filed, and
/// nothing else. Requiring the embedded number to equal the issue's own number
/// means an issue that merely mentions the outcome cannot be adopted.
/// Test: `a_tracker_title_is_recognised_by_its_own_number`.
pub(crate) fn is_tracker_title(title: &str, number: u64, outcome: &str) -> bool {
    title.trim() == tracker_title(number, outcome)
}

/// The phase number embedded in a child's title, if it carries one.
///
/// Test: `next_phase_number_never_reuses_a_deleted_number`.
pub(crate) fn phase_number_of(title: &str) -> Option<u64> {
    let rest = title.trim().strip_prefix("[EPIC_")?;
    let (_, rest) = rest.split_once(" PHASE_")?;
    let (digits, _) = rest.split_once(']')?;
    digits.trim().parse().ok()
}

/// The text after a child's `[EPIC_<n> PHASE_<m>] ` prefix.
///
/// Why: `create` decides whether a plan phase already exists by comparing this
/// to the plan's phase title, so a re-run cannot file a duplicate under a new
/// number.
/// Test: `create_skips_a_phase_that_already_exists`.
pub(crate) fn phase_what(title: &str) -> String {
    title
        .trim()
        .split_once("] ")
        .map_or_else(|| title.trim().to_string(), |(_, what)| what.to_string())
}

/// The next phase number for a tracker: max+1 over every existing child.
///
/// Why: D3 and `TICKETING.md` both say phase numbers are assigned once and
/// never reused. Counting children instead of taking the maximum is the bug
/// this function exists to not have — a deleted `PHASE_6` would make the next
/// phase `PHASE_6` again.
/// What: the largest number any child's title carries, plus one; `1` when no
/// child carries one.
/// Test: `next_phase_number_never_reuses_a_deleted_number`.
pub(crate) fn next_phase_number(children: &[ChildIssue]) -> u64 {
    children
        .iter()
        .filter_map(|c| phase_number_of(&c.title))
        .max()
        .map_or(1, |m| m + 1)
}

/// The SHA-pinned permalink a tracker links its plan document by.
///
/// Why: D4 — a `/blob/main/` link points at whatever `main` says later, so the
/// tracker would stop describing the plan it was filed from. A 40-hex commit
/// path cannot drift.
/// Test: `tracker_body_links_the_plan_by_sha`.
pub(crate) fn plan_permalink(repo: &str, sha: &str, path: &str) -> String {
    format!("https://github.com/{repo}/blob/{sha}/{path}")
}

/// Render the phases table from live child state.
///
/// Why: the block is regenerated wholesale, so this function's output IS the
/// block — a row hand-edited inside the old block is discarded by construction
/// rather than merged.
/// What: the two header rows plus one row per child, ordered by phase number,
/// each carrying the child's number, its lowercased state, and the `## Gate`
/// section of its body collapsed to one line. A `|` in any cell is escaped so
/// it cannot split the row.
/// Test: `render_replaces_the_whole_phases_block`,
/// `render_escapes_a_pipe_in_a_phase_title`.
pub(crate) fn phases_table(children: &[ChildIssue]) -> String {
    let mut rows: Vec<(u64, &ChildIssue)> = children
        .iter()
        .filter_map(|c| phase_number_of(&c.title).map(|n| (n, c)))
        .collect();
    rows.sort_by_key(|(n, _)| *n);

    let mut out = String::from(PHASES_HEADER);
    for (number, child) in rows {
        let _ = write!(
            out,
            "\n| {number} | {} | #{} | {} | {} |",
            cell(&phase_what(&child.title)),
            child.number,
            cell(&child.state.to_lowercase()),
            cell(&gate_of(&child.body))
        );
    }
    out
}

/// Escape a table cell so its content cannot split the row.
fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

/// The one-line gate a phase issue's `## Gate` section declares.
///
/// Why: the Gate column is what justifies using the tracker pattern at all, so
/// it is read back from the phase issue rather than re-derived from the plan —
/// a phase whose gate was edited on the issue syncs with the edited gate.
/// What: the FIRST paragraph under `## Gate`, joined to one line. Stopping at
/// the paragraph break rather than at the next heading is what keeps the phase
/// summary that follows out of the table cell.
/// Test: `render_replaces_the_whole_phases_block`,
/// `gate_of_reads_only_the_first_paragraph`.
fn gate_of(body: &str) -> String {
    let mut inside = false;
    let mut collected: Vec<&str> = Vec::new();
    for line in body.lines() {
        if line.trim_end() == "## Gate" {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        if line.starts_with("## ") {
            break;
        }
        if line.trim().is_empty() {
            if collected.is_empty() {
                continue;
            }
            break;
        }
        collected.push(line.trim());
    }
    if collected.is_empty() {
        "(no gate declared)".to_string()
    } else {
        collected.join(" ")
    }
}

/// Build the tracker body from a parsed plan.
///
/// Why: everything above the markers is authored once, so it is rendered once,
/// here, from the plan document — never retyped and never regenerated. The
/// three marker blocks are emitted empty-but-present, which is what makes the
/// tracker a valid target for [`replace_block`] from its first second.
/// What: the SHA-pinned plan link, the summary, the outcomes, the ratified
/// decisions, the ordering prose, then the `phases`, `deferred` and `followups`
/// blocks (D2).
/// Test: `tracker_body_links_the_plan_by_sha`,
/// `tracker_body_carries_all_three_marker_blocks`.
pub(crate) fn tracker_body(plan: &EpicPlan, plan_url: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Plan: {plan_url}\n");
    push_block(&mut out, &plan.summary);
    out.push_str("\n## Outcomes\n\n");
    push_block(&mut out, &plan.outcomes);
    out.push_str("\n## Ratified decisions\n\n");
    push_block(&mut out, &plan.decisions);
    out.push_str("\n## Ordering\n\n");
    push_block(&mut out, &plan.ordering);
    let _ = writeln!(
        out,
        "\n## Phases\n\n{PHASES_START}\n{PHASES_HEADER}\n{PHASES_END}"
    );
    let _ = writeln!(out, "\n## Deferred\n\n{DEFERRED_START}");
    if plan.deferred.is_empty() {
        out.push_str(
            "| Item | Why deferred | Where it went |\n|------|--------------|---------------|\n",
        );
    } else {
        push_block(&mut out, &plan.deferred);
    }
    let _ = writeln!(out, "{DEFERRED_END}");
    let _ = writeln!(out, "\n## Follow-ups\n\n{FOLLOWUPS_START}");
    out.push_str(
        "| Finding | Severity | Where it went |\n|---------|----------|---------------|\n",
    );
    let _ = writeln!(out, "{FOLLOWUPS_END}");
    out
}

/// Build a phase issue's body.
///
/// Why: `Part of #<epic>, phase <n> of <N>.` is the first line by convention,
/// and the `## Gate` section is what `sync` reads back to fill the tracker's
/// Gate column — so the write side and the read side agree on one heading.
/// Test: `phase_body_declares_its_gate_and_its_parent`.
pub(crate) fn phase_body(phase: &PhasePlan, epic: u64, index: usize, total: usize) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Part of #{epic}, phase {index} of {total}.\n");
    let _ = writeln!(out, "## Gate\n\n{}\n", phase.gate);
    push_block(&mut out, &phase.body);
    out
}

/// Append a block of lines, each newline-terminated.
fn push_block(out: &mut String, lines: &[String]) {
    for line in lines {
        out.push_str(line);
        out.push('\n');
    }
}
