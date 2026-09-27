//! `tm issue epic defer` — append one row to a tracker's `deferred` block (#8448).
//!
//! Why: the `deferred` block is the one zone of a tracker body that is AMENDED
//! rather than regenerated (D2), and the hand-run amendment is a session editing
//! a markdown table in place — the same retyping the `phases` block outgrew in
//! phase 1, with the same failure shapes: a row pasted outside the markers, a
//! header duplicated, a `|` in the item text splitting the row.
//! What: [`defer`] reads the body, takes the current block through
//! [`super::render::block_content`] under the same refusal set as a rewrite,
//! appends exactly one escaped row, and writes the result through
//! [`super::render::replace_block`] — so every byte outside the two `deferred`
//! markers, the `phases` and `followups` blocks included, is copied through
//! verbatim. A row already present is a reported no-op, never a duplicate.
//! Test: `defer_appends_exactly_one_row`,
//! `defer_leaves_the_phases_and_followups_blocks_byte_identical`,
//! `defer_is_a_no_op_when_the_row_is_already_present`,
//! `defer_refuses_a_blank_cell_and_writes_nothing`,
//! `defer_seeds_the_header_into_an_empty_block`.

use super::backend::EpicBackend;
use super::render::{self, DEFERRED_END, DEFERRED_HEADER, DEFERRED_START};

/// The three cells of one `deferred` row.
///
/// Test: `defer_appends_exactly_one_row`.
#[derive(Debug, Clone)]
pub(crate) struct DeferOptions {
    /// The scope removed, or the gap it leaves.
    pub(crate) item: String,
    /// Why it left the plan.
    pub(crate) why: String,
    /// Where it went — an issue reference, or `unscheduled`.
    pub(crate) destination: String,
}

/// What one `defer` run did.
///
/// Test: `defer_is_a_no_op_when_the_row_is_already_present`.
#[derive(Debug, Clone)]
pub(crate) struct DeferReport {
    /// The tracker amended.
    pub(crate) tracker: u64,
    /// The row as written (or found), in its rendered form.
    pub(crate) row: String,
    /// Whether the row was already present and no write was made.
    pub(crate) unchanged: bool,
}

/// Append one row to `tracker`'s `deferred` block.
///
/// Why: AC5 — exactly one row, and the other two blocks byte-identical. The
/// second half is structural: the rewrite goes through `replace_block` keyed on
/// the `deferred` markers, which copies every other segment through.
/// What: refuses a blank cell before any read; reads the body; takes the current
/// block content; returns unchanged when the rendered row is already a line of
/// it; otherwise appends the row (seeding the header when the block is empty)
/// and writes. Every refusal names the tracker, and no write happens on any.
/// Test: see the module doc.
pub(crate) fn defer<B: EpicBackend>(
    backend: &B,
    tracker: u64,
    opts: &DeferOptions,
) -> anyhow::Result<DeferReport> {
    for (flag, value) in [
        ("--item", &opts.item),
        ("--why", &opts.why),
        ("--where", &opts.destination),
    ] {
        if value.trim().is_empty() {
            anyhow::bail!("`{flag}` is blank — a deferred row needs all three cells");
        }
    }
    let row = format!(
        "| {} | {} | {} |",
        render::cell(opts.item.trim()),
        render::cell(opts.why.trim()),
        render::cell(opts.destination.trim())
    );

    let body = backend.body(tracker)?;
    let current = render::block_content(&body, DEFERRED_START, DEFERRED_END)
        .map_err(|e| anyhow::anyhow!("#{tracker}: {e}"))?;
    // #8448: a re-run with the same three cells must not stack a second row.
    if current.lines().any(|l| l.trim() == row) {
        return Ok(DeferReport {
            tracker,
            row,
            unchanged: true,
        });
    }
    let content = if current.trim().is_empty() {
        format!("{DEFERRED_HEADER}\n{row}")
    } else {
        format!("{current}\n{row}")
    };
    let next = render::replace_block(&body, DEFERRED_START, DEFERRED_END, &content)
        .map_err(|e| anyhow::anyhow!("#{tracker}: {e}"))?;
    backend.set_body(tracker, &next)?;
    Ok(DeferReport {
        tracker,
        row,
        unchanged: false,
    })
}
