//! The epic set-level rows `tm issue audit <tracker>` appends (#8448).
//!
//! Why: O4 of the plan — the guarantee that a tracker's `phases` block matches
//! its children is checked rather than asserted. Two things can drift: the
//! block itself (a phase closed with no sync), and the child set (a phase-titled
//! issue that was never linked as a native sub-issue, so no sync will ever list
//! it). Each is a FAIL row, never INFO: both are violations the standard names.
//! What: [`epic_rows`] returns nothing for an issue whose body carries no
//! `phases` marker (it is not a tracker), and otherwise two rows — the block
//! comparison, regenerated through the same renderer `sync` writes with, and
//! the linkage check over the enumerated phase-titled candidates.
//!
//! # What the linkage row proves about a lagging search
//!
//! The candidates come from GitHub's search index, which lags a just-created
//! issue — and the workflow audits right after `create`. The row cross-checks
//! the result against the one consistent source it has, the linked children:
//! a search that omits a linked child whose title names this tracker FAILs
//! with the shortfall and a re-run hint. That is all it proves. A lag confined
//! to an unlinked phase newer than every linked one looks exactly like the
//! absence of such a phase, so it is not detected. PASS therefore reports how
//! many linked phases the search saw; with no linked phase at all there is
//! nothing to cross-check against, so the row is INFO rather than a PASS it
//! cannot back (#8448, review MEDIUM). An unlinked phase-titled issue the
//! search does return FAILs in every case.
//!
//! Test: `audit_rows_pass_a_tracker_whose_block_matches`,
//! `audit_rows_fail_a_stale_phases_block_with_the_sync_command`,
//! `audit_rows_fail_a_phase_titled_issue_that_is_not_a_sub_issue`,
//! `audit_rows_fail_when_search_omits_a_linked_phase`,
//! `audit_rows_report_info_when_no_linked_phase_cross_checks_the_search`,
//! `audit_rows_are_empty_for_a_non_tracker`,
//! `audit_rows_fail_a_body_whose_markers_cannot_be_rewritten`.

use std::collections::BTreeSet;

use trusty_mpm::core::issue_audit::{AuditRow, Verdict};

use super::backend::EpicBackend;
use super::render::{self, PHASES_END, PHASES_START};

/// Requirement name of the block-comparison row.
pub(crate) const REQ_PHASES_BLOCK: &str = "phases block";
/// Requirement name of the linkage row.
pub(crate) const REQ_PHASE_LINKAGE: &str = "phase linkage";

/// The set-level rows for `tracker`, or none when it is not a tracker.
///
/// Why: the audit's other rows read one issue's facts; these two read the
/// tracker's children and the repository's phase-titled issues, so they are
/// computed only for an issue that declares a `phases` block.
/// What: reads the body; no `phases:start` line → empty. Otherwise renders the
/// block from the children exactly as `sync` would — under the same
/// `status_prefix` — and compares byte for byte (a body `replace_block`
/// refuses is a FAIL naming the refusal); then lists every issue titled
/// `[EPIC_<tracker> PHASE_…]`, FAILs when the search omitted a linked phase
/// (the index has not caught up), and FAILs on each candidate that is not in
/// the child set, with the `gh` call that links it. With no failure the row
/// is PASS naming how many linked phases the search saw, or INFO when no
/// phase is linked and the result therefore cannot be cross-checked. Any
/// backend failure is propagated — an audit that could not enumerate must not
/// print PASS.
/// Test: see the module doc.
pub(crate) fn epic_rows<B: EpicBackend>(
    backend: &B,
    tracker: u64,
    status_prefix: &str,
) -> anyhow::Result<Vec<AuditRow>> {
    let body = backend.body(tracker)?;
    if !body.lines().any(|l| l.trim() == PHASES_START) {
        return Ok(Vec::new());
    }
    let children = backend.children(tracker)?;
    let table = render::phases_table(&children, status_prefix);
    let rows = children
        .iter()
        .filter(|c| render::phase_number_of(&c.title).is_some())
        .count();
    let block_row = match render::replace_block(&body, PHASES_START, PHASES_END, &table) {
        Err(e) => AuditRow::new(REQ_PHASES_BLOCK, Verdict::Fail, e.to_string()),
        Ok(regenerated) if regenerated == body => AuditRow::new(
            REQ_PHASES_BLOCK,
            Verdict::Pass,
            format!("matches its {rows} phase issue(s)"),
        ),
        // #8448 AC4: the exact wording is what an operator greps for.
        Ok(_) => AuditRow::new(
            REQ_PHASES_BLOCK,
            Verdict::Fail,
            format!("phases block stale — run tm issue epic sync {tracker}"),
        ),
    };

    let linked: BTreeSet<u64> = children.iter().map(|c| c.number).collect();
    let candidates = backend.phase_titled_issues(tracker)?;
    let seen: BTreeSet<u64> = candidates.iter().map(|c| c.number).collect();
    let mut failures: Vec<String> = candidates
        .iter()
        .filter(|c| render::epic_number_of(&c.title) == Some(tracker))
        .filter(|c| !linked.contains(&c.number))
        .map(|c| {
            format!(
                "#{} `{}` is phase-titled but not a native sub-issue — link it: gh issue edit \
                 {tracker} --add-sub-issue {}",
                c.number,
                c.title.trim(),
                c.number
            )
        })
        .collect();
    // #8448: the search is eventually consistent. Every linked phase is known
    // without it, so a linked phase it did not return proves the result is
    // short — and a short result cannot vouch for the unlinked set.
    let known = children
        .iter()
        .filter(|c| render::epic_number_of(&c.title) == Some(tracker))
        .count();
    let seen_known = children
        .iter()
        .filter(|c| render::epic_number_of(&c.title) == Some(tracker))
        .filter(|c| seen.contains(&c.number))
        .count();
    if seen_known < known {
        failures.push(format!(
            "search index returned {seen_known} of {known} known phases — re-run in a minute"
        ));
    }
    let linkage_row = if !failures.is_empty() {
        AuditRow::new(REQ_PHASE_LINKAGE, Verdict::Fail, failures.join("; "))
    } else if known == 0 {
        // #8448 (review MEDIUM): with nothing linked, an empty search result
        // and a lagging one are the same bytes — the row cannot claim PASS.
        AuditRow::new(
            REQ_PHASE_LINKAGE,
            Verdict::Info,
            "no phase is linked, so the search result cannot be cross-checked — an unlinked \
             phase the index has not returned yet is invisible here",
        )
    } else {
        AuditRow::new(
            REQ_PHASE_LINKAGE,
            Verdict::Pass,
            format!("{known} of {known} linked phases seen by search"),
        )
    };
    Ok(vec![block_row, linkage_row])
}
