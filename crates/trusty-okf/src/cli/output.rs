//! Text and JSON renderings of plans, reports and status (SPEC-OKF-01
//! Appendix A, §4.9 "Reporting rule").
//!
//! What: one line per axis, so saved, index and verification counts never
//! share a line. The summary says "fully searchable" only when a feed pass
//! ran and every touched concept reached its index goal; otherwise "not
//! fully searchable", with "no feed pass ran" when none did (criteria 60,
//! 85). An output write error is ignored: the exit code still reports the
//! run.

use std::io::Write;

use serde::Serialize;

use crate::import::{ImportPlan, ImportReport, RunStatus, StoreStatus};

fn json<T: Serialize + ?Sized>(out: &mut dyn Write, v: &T) {
    let text = serde_json::to_string_pretty(v).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"));
    let _ = writeln!(out, "{text}");
}

/// A plan: one line per item, then the listing summary.
pub(crate) fn plan(out: &mut dyn Write, plan: &ImportPlan, as_json: bool) {
    if as_json {
        return json(out, plan);
    }
    for p in &plan.items {
        let id = p.id.as_ref().map_or("-", |i| i.as_str());
        let _ = writeln!(
            out,
            "{:?} {} -> {} extractor={} reason={}",
            p.action,
            p.item_id,
            id,
            p.extractor.as_deref().unwrap_or("-"),
            p.reason.as_deref().unwrap_or("-")
        );
    }
    let _ = writeln!(
        out,
        "plan {} store {}: listed {}, pagination_exhausted {}, deletion_safe {}",
        plan.run_id, plan.store, plan.listed, plan.pagination_exhausted, plan.deletion_safe
    );
}

/// An import report, axis by axis.
pub(crate) fn report(out: &mut dyn Write, r: &ImportReport, as_json: bool) {
    if as_json {
        return json(out, r);
    }
    let w = |out: &mut dyn Write, s: String| {
        let _ = writeln!(out, "{s}");
    };
    w(out, format!("run {} store {}", r.run_id, r.store));
    w(
        out,
        format!(
            "list: listed {}, pagination_exhausted {}, deletion_safe {}",
            r.listed, r.pagination_exhausted, r.deletion_safe
        ),
    );
    w(
        out,
        format!(
            "read: read {}, skipped {}, failed {}, degraded {}",
            r.read, r.skipped, r.failed, r.degraded
        ),
    );
    w(
        out,
        format!(
            "save: written {} (created {}, updated {}, tombstoned {}), unchanged {}, write_failed {}, missing {}",
            r.written, r.created, r.updated, r.tombstoned, r.unchanged, r.write_failed, r.missing
        ),
    );
    let fed = if r.fed { "" } else { " (no feed pass ran)" };
    w(
        out,
        format!(
            "index: pending {}, acknowledged {}, searchable {}, withdrawn {}{fed}",
            r.pending, r.acknowledged, r.searchable, r.withdrawn
        ),
    );
    w(
        out,
        format!(
            "verification: verified_current {}, verification_invalidated {}",
            r.verified_current, r.verification_invalidated
        ),
    );
    for i in r.items.iter().filter(|i| i.reason.is_some()) {
        w(
            out,
            format!(
                "item {}: read {}, write {}, reason {}",
                i.item_id,
                i.read,
                i.write,
                i.reason.as_deref().unwrap_or("")
            ),
        );
    }
    for f in &r.findings {
        w(
            out,
            format!(
                "finding {} {} {} {}",
                f.severity, f.kind, f.item_id, f.detail
            ),
        );
    }
    let complete = if r.complete { "complete" } else { "incomplete" };
    let search = if r.fully_searchable {
        "fully searchable".to_string()
    } else {
        format!("not fully searchable{fed}")
    };
    w(
        out,
        format!("summary: {complete}, {search}; exit {}", r.exit_code),
    );
}

/// `import status`.
pub(crate) fn status(
    out: &mut dyn Write,
    stores: &[StoreStatus],
    runs: &[RunStatus],
    as_json: bool,
) {
    if as_json {
        return json(out, &serde_json::json!({ "stores": stores, "runs": runs }));
    }
    for s in stores {
        let index = match s.index {
            Some([p, a, sr, wp, wd]) => format!(
                "pending {p}, acknowledged {a}, searchable {sr}, withdraw_pending {wp}, withdrawn {wd}"
            ),
            None => "not read (pass --index and --state-dir)".to_string(),
        };
        let _ = writeln!(
            out,
            "store {} ({}): ledger live {}, tombstoned {}\n  save: saved {}, saved_external {}, absent {}\n  index: {index}\n  verification: verified {}, unverified {}",
            s.store,
            s.kind,
            s.live,
            s.tombstoned,
            s.saved,
            s.saved_external,
            s.absent,
            s.verified,
            s.unverified
        );
    }
    for r in runs {
        let state = if r.interrupted {
            "interrupted"
        } else {
            "finished"
        };
        let _ = writeln!(
            out,
            "run {} store {} started {} {state}",
            r.attempt, r.store, r.started
        );
    }
}
