//! Cost computation over the shared pricing table, plus small formatters.
//!
//! Why: The cost math is mechanically independent of the collector's lifecycle;
//! isolating it keeps `mod.rs` focused on recording and persisting performance
//! records.
//!
//! **#6875: the rate table itself no longer lives here.** This file used to own
//! a per-family table that priced Sonnet at 4.x-generation rates ($3/$15 per
//! MTok) and defaulted every unrecognised id to those same rates — so a Sonnet 5
//! turn was billed 50% high and an unknown model was billed as if it were
//! Sonnet. `trusty-mpm` carried a second, differently-stale copy. Both now read
//! `trusty_common::pricing`, the one table for the workspace; see that module's
//! doc for the schema, the `effective_from` dates, and the operator override.
//! [`cost_usd`] stays the single cost entry point for THIS crate (#4098,
//! COST-05): `usage::daily::cost_from_tokens`, `usage::aggregate` and
//! `perf::PerfCollector` all route through it.
//!
//! What: `cost_usd` (public) plus the `filename_stamp` / `truncate_preview`
//! formatters used by the collector.
//! Test: `cost_usd_*` / `filename_stamp_format` / `truncate_preview_*` in
//! `perf::tests`.

use trusty_common::pricing::{Usage, shared, warn_unknown_model_once};

/// Compute USD cost for a single LLM call given the model name and token
/// counts. An unpriced model costs `0.00` and is reported once.
///
/// Why: (#47) The rates are shipped rather than fetched, so offline/CI runs
/// still produce comparable cost figures. (#4098) This is the one entry point
/// every cost surface in this crate must call. (#6875) The rates themselves come
/// from `trusty_common::pricing`, shared with trusty-mpm and the #6872 ledger.
/// What: resolves today's [`trusty_common::pricing::Rates`] for `model` and
/// prices each token bucket at its own rate. An id no row claims returns `0.0`
/// after [`warn_unknown_model_once`] — the stale Sonnet-class default it
/// replaced turned an unknown model into a confident wrong number.
///
/// Counts are `u64` rather than `u32` (#4098) because the aggregate surfaces
/// that share this entry point — a whole day of dispatches in
/// `usage::aggregate`, a whole REPL session in `usage::daily` — sum well past
/// `u32::MAX`, and a saturating cast at each of those call sites would silently
/// under-report exactly the totals the Costs tab exists to show.
/// Test: `cost_usd_known_sonnet`, `cost_usd_known_haiku`,
/// `cost_usd_unknown_model_is_zero_not_a_stale_default`,
/// `cost_usd_sonnet_5_is_no_longer_priced_at_4x_rates`,
/// `cost_usd_accepts_counts_beyond_u32`.
pub fn cost_usd(
    model: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
    cache_read: u64,
    cache_creation: u64,
) -> f64 {
    // #6875: one table for the workspace — see trusty_common::pricing.
    let usage = Usage::new(prompt_tokens, completion_tokens, cache_creation, cache_read);
    match shared().rate_today(model) {
        Some(rates) => rates.cost_usd(&usage),
        None => {
            warn_unknown_model_once(model);
            0.0
        }
    }
}

/// Build the canonical filename stamp from an ISO8601 timestamp + build #.
///
/// Why: Deterministic filenames let tests assert exact paths and let humans
/// sort runs chronologically with `ls`.
/// What: Converts `2026-04-22T17:31:30Z` + build=42 to `20260422-173130-build42`.
/// Test: `filename_stamp_format`.
pub(super) fn filename_stamp(iso: &str, build: u64) -> String {
    // Strip non-digit chars to keep only YYYYMMDDHHMMSS, then reinsert the dash.
    let digits: String = iso.chars().filter(|c| c.is_ascii_digit()).collect();
    // Expected layout: YYYYMMDDHHMMSS (14 digits). If shorter, just pad.
    let date = digits.get(0..8).unwrap_or("00000000");
    let time = digits.get(8..14).unwrap_or("000000");
    format!("{date}-{time}-build{build}")
}

pub(super) fn truncate_preview(s: &str, max_chars: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut out = String::with_capacity(max_chars + 3);
    for (i, ch) in trimmed.chars().enumerate() {
        if i >= max_chars {
            break;
        }
        out.push(ch);
    }
    out.push_str("...");
    out
}
