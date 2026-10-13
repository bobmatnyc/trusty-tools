//! Apply a sealed review ledger to one store's import plan (#8624, PR-C).
//!
//! Why: `import kuzu --review-ledger` must admit exactly the rows an agent
//! judged, in the form it judged, and refuse everything else. The ledger is
//! read-only here; every admission is decided from the live row.
//! What: [`apply_verdicts`] re-classifies each secret-refused memory with
//! [`classify_mapped`] (an auto-refuse row never reaches the ledger), looks
//! its live key up, and calls [`decide`]. A redacted row gets its new content
//! and then goes through the unchanged import path, whose screen and
//! `HandleSink` check run again; a carve-out row is marked
//! `review_override` and tagged [`OVERRIDE_TAG`]. Every other outcome leaves
//! the row as it was, so the import refuses it as before.
//! Test: `review::admit_tests`.

use crate::engine::filter::{SECRET_FILTER_VERSION, find_secret_spans};

use super::super::apply::StorePlan;
use super::super::ledger::MemoryPlan;
use super::super::mapping::OVERRIDE_TAG;
use super::super::screen::screen;
use super::carve_out::row_admissible;
use super::export::{MaskedRecord, RowVerdict, classify_mapped};
use super::ledger::{Lookup, ReviewLedger};
use super::ops::apply_ops;
use super::verdict::Verdict;

/// What the ledger decides for one refused row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Admit this redacted content; it screened clean.
    Redacted(String),
    /// Admit the content verbatim under the carve-out.
    Override,
    /// Leave the row refused, for this reason label.
    Refuse(&'static str),
}

/// Refusal labels, as the store report tallies them.
pub(crate) mod reason {
    pub const AUTO_REFUSE: &str = "auto_refuse";
    pub const NO_VERDICT: &str = "no_verdict";
    pub const DUPLICATE: &str = "duplicate_verdict";
    pub const KEY: &str = "key_mismatch";
    pub const FILTER_VERSION: &str = "filter_version";
    pub const DIGEST: &str = "digest_mismatch";
    pub const KEEP_REFUSED: &str = "keep_refused";
    pub const NOT_CARVE_OUT: &str = "not_carve_out";
    pub const SPANS: &str = "span_mismatch";
    pub const BAD_OP: &str = "bad_op";
    pub const EMPTY: &str = "empty_after_redaction";
    pub const STILL_FLAGGED: &str = "still_flagged";
}

/// The ledger's decision for an exported row whose trimmed content is `text`.
///
/// Why (#8624 rules 3, 6, 8, 9): each check is a way a stale, forged or
/// malformed verdict could admit a secret, so each one refuses.
/// What: refuses a missing or duplicate verdict, a key or filter version
/// other than the live one, and a digest that does not match the entry.
/// `admit` needs [`row_admissible`] over the live span shapes. `admit-redacted`
/// applies the ops to the live `find_secret_spans(text)` (whose count must
/// match the record's), trims the result, and admits it only when it is
/// non-empty and the secret screen passes it.
/// Test: `verdict_under_another_filter_version_refuses_the_row`,
/// `verdict_for_changed_content_refuses_the_row`,
/// `verdict_with_a_changed_digest_refuses_the_row`,
/// `out_of_range_op_refuses_the_row`,
/// `redaction_that_leaves_a_flagged_span_refuses_the_row`,
/// `carve_out_admits_low_entropy_and_refuses_at_the_threshold`.
pub(crate) fn decide(record: &MaskedRecord, text: &str, found: Lookup<'_>) -> Decision {
    let entry = match found {
        Lookup::Missing => return Decision::Refuse(reason::NO_VERDICT),
        Lookup::Duplicate => return Decision::Refuse(reason::DUPLICATE),
        Lookup::Entry(e) => e,
    };
    if entry.key != record.key {
        return Decision::Refuse(reason::KEY);
    }
    if entry.filter_version != SECRET_FILTER_VERSION
        || record.filter_version != SECRET_FILTER_VERSION
    {
        return Decision::Refuse(reason::FILTER_VERSION);
    }
    if !entry.digest_matches() {
        return Decision::Refuse(reason::DIGEST);
    }
    match &entry.verdict {
        Verdict::KeepRefused {} => Decision::Refuse(reason::KEEP_REFUSED),
        Verdict::Admit {} => {
            let shapes = record.spans.iter().map(|s| (s.rule, s.len, s.ent_tenths));
            if row_admissible(shapes) {
                Decision::Override
            } else {
                Decision::Refuse(reason::NOT_CARVE_OUT)
            }
        }
        Verdict::AdmitRedacted { ops } => {
            let spans = find_secret_spans(text);
            if spans.len() != record.spans.len() {
                return Decision::Refuse(reason::SPANS);
            }
            let Ok(out) = apply_ops(text, &spans, ops) else {
                return Decision::Refuse(reason::BAD_OP);
            };
            let out = out.trim();
            if out.is_empty() {
                Decision::Refuse(reason::EMPTY)
            } else if screen(out).is_some() {
                Decision::Refuse(reason::STILL_FLAGGED)
            } else {
                Decision::Redacted(out.to_string())
            }
        }
    }
}

/// Apply `ledger` to every secret-refused memory `plan` would write.
///
/// What: skips clean memories and those the plan will not write
/// (unchanged, shared id). Counts each admission in `review_admitted` or
/// `review_overrides` and each refusal in `review_refusals` by reason.
/// Test: `redacted_and_override_rows_land_through_the_handle_sink`,
/// `auto_refuse_row_is_never_admitted_even_with_a_forged_verdict`.
pub(crate) fn apply_verdicts(plan: &mut StorePlan, ledger: &ReviewLedger) {
    let counts = &mut plan.counts;
    for (m, p) in plan.memories.iter_mut() {
        if matches!(p, MemoryPlan::Unchanged(_) | MemoryPlan::SharedId(_))
            || screen(m.content.trim()).is_none()
        {
            continue;
        }
        let decision = match classify_mapped(m) {
            RowVerdict::Clean => continue,
            RowVerdict::AutoRefuse(_) => Decision::Refuse(reason::AUTO_REFUSE),
            RowVerdict::Export(record) => {
                decide(&record, m.content.trim(), ledger.lookup(&record.key))
            }
        };
        match decision {
            Decision::Redacted(text) => {
                m.content = text;
                counts.review_admitted += 1;
            }
            Decision::Override => {
                m.review_override = true;
                m.tags.push(OVERRIDE_TAG.to_string());
                counts.review_overrides += 1;
            }
            Decision::Refuse(label) => {
                *counts.review_refusals.entry(label).or_default() += 1;
            }
        }
    }
}
