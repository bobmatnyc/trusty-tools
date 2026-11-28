//! The one count path: `tally` and the reply footer built from it.
//!
//! Why: the footer, `tally` and any report count must come from one function,
//! never a hand count, and a store that cannot be read must give an error,
//! never a zero (DOC-82 LDG-5.7, #10063 AC 6).
//! What: [`tally`] reads `items` and `questions` from a [`Store`]: the
//! counts are `dashboard_derive.tally` (the [`super::pyderive`] port:
//! self-test canaries skipped, `interactive` items open, a `decision` counted
//! as a question, relayed answers left out, `decisions` and `blockers`),
//! then `running` and open `questions` as `answers_waiting` (absent when
//! zero), as `dashboard-db.py tally` adds them. [`tally_line`] and [`footer`]
//! render it in the prototype's words; the two links are the caller's (the
//! prototype hard-codes one host's names). A missing root, a corrupt
//! document and a store in the other format are errors.
//! Test: `footer_equals_tally_line`, `unreadable_store_is_error_not_zero`,
//! `tally_counts_kinds_parked_running_and_waiting`,
//! `tally_follows_the_python_rule_for_canaries_interactive_decisions_and_relays`.

use serde::Serialize;
use serde_json::Value;

use super::derive::running_count;
use super::error::DashError;
use super::ids::py_doc;
use super::store::{ListQuery, Store};
use crate::pyjson_load::{PyInt, PyValue};
use crate::pyval::get;
use crate::tasks::store_read::PyDoc;

/// Counts over the store, in the prototype's key order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Tally {
    /// Open (or interactive), unparked items of kind question, decision, action or task.
    pub open: usize,
    /// Open, unparked questions and decisions (D).
    pub questions: usize,
    /// Open, unparked actions (A).
    pub actions: usize,
    /// Open, unparked tasks (T).
    pub tasks: usize,
    /// Answered items not yet relayed.
    pub answered_unrelayed: usize,
    /// Parked open items (never counted in `open`).
    pub parked: usize,
    /// Open, unparked `D` items (`decision_counts`' `to_decide`).
    pub decisions: usize,
    /// Open, unparked `B` items.
    pub blockers: usize,
    /// Open items in `interactive` status; absent from the JSON when zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interactive: Option<usize>,
    /// Open items inside their run window.
    pub running: usize,
    /// Open `questions` documents; absent from the JSON when zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answers_waiting: Option<usize>,
}

/// The two dashboard links the footer names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FooterLinks {
    /// The loopback URL.
    pub local: String,
    /// The tailnet URL.
    pub tailnet: String,
}

/// The footer line and the tally it was built from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Footer {
    /// The one-line reply footer.
    pub footer: String,
    /// The counts in it.
    pub tally: Tally,
}

/// Counts over `store` at `now` (Unix seconds).
///
/// # Errors
/// [`DashError::Io`] when the root is not a readable directory; any read
/// error of `items` or `questions` (corrupt file, other format).
pub fn tally(store: &Store, now: f64) -> Result<Tally, DashError> {
    let root = store.root();
    let meta = std::fs::metadata(root).map_err(DashError::io(root))?;
    if !meta.is_dir() {
        return Err(DashError::Io {
            path: root.to_path_buf(),
            source: std::io::Error::other("store root is not a directory"),
        });
    }
    let items = store.list("items", &ListQuery::default())?;
    // #12029 P2e: the one count rule is `dashboard_derive.tally`'s port.
    let py: Vec<PyDoc> = items.iter().map(py_doc).collect();
    let counts = super::pyderive::counts::tally(&py);
    let n = |k: &str| match get(&counts, k) {
        Some(PyValue::Int(PyInt::Small(v))) => usize::try_from(*v).unwrap_or(0),
        _ => 0,
    };
    let mut t = Tally {
        open: n("open"),
        questions: n("questions"),
        actions: n("actions"),
        tasks: n("tasks"),
        answered_unrelayed: n("answered_unrelayed"),
        parked: n("parked"),
        decisions: n("decisions"),
        blockers: n("blockers"),
        interactive: get(&counts, "interactive").map(|_| n("interactive")),
        ..Tally::default()
    };
    t.running = running_count(&items, now);
    let waiting = store
        .list("questions", &ListQuery::default())?
        .iter()
        .filter(|d| d.data.get("status").and_then(Value::as_str) == Some("open"))
        .count();
    t.answers_waiting = (waiting > 0).then_some(waiting);
    Ok(t)
}

/// `"<N> open dashboard items: <q> questions, <a> actions, <t> tasks"` plus
/// the parked, running and answers-waiting parts when non-zero.
pub fn tally_line(t: &Tally) -> String {
    let n = |v: usize, word: &str| format!("{v} {word}{}", if v == 1 { "" } else { "s" });
    let mut line = format!(
        "{} open dashboard item{}: {}, {}, {}",
        t.open,
        if t.open == 1 { "" } else { "s" },
        n(t.questions, "question"),
        n(t.actions, "action"),
        n(t.tasks, "task"),
    );
    if t.parked > 0 {
        line.push_str(&format!(" ({} parked)", t.parked));
    }
    if t.running > 0 {
        line.push_str(&format!(" ({} running)", t.running));
    }
    if let Some(w) = t.answers_waiting.filter(|w| *w > 0) {
        line.push_str(&format!(", {w} answers waiting"));
    }
    line
}

/// The reply footer: `Dashboard: [local](…) · [tail](…) — <tally line>`,
/// from [`tally`] only.
///
/// # Errors
/// Whatever [`tally`] reports; no footer is built from a guessed count.
pub fn footer(store: &Store, now: f64, links: &FooterLinks) -> Result<Footer, DashError> {
    let tally = tally(store, now)?;
    let footer = format!(
        "Dashboard: [local]({}) \u{b7} [tail]({}) \u{2014} {}",
        links.local,
        links.tailnet,
        tally_line(&tally)
    );
    Ok(Footer { footer, tally })
}
