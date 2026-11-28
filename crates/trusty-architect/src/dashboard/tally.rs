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
//! render it in the frozen `dashboard-db.py`'s words (#12029 P2g): `<d>
//! decisions, <b> blockers`, then ` · questions: N` (linked when the
//! caller has the questions session's live URL) while interactive items
//! wait, and the `Reminders (ack): ...` line the caller read. The links are
//! the caller's (the prototype hard-codes one host's names); no tailnet URL
//! drops the tail link, as the script does. A missing root, a corrupt
//! document and a store in the other format are errors.
//! Test: `footer_equals_tally_line`, `unreadable_store_is_error_not_zero`,
//! `footer_links_the_questions_part_and_carries_the_reminders_line`,
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

/// What the footer names besides the counts: the dashboard links, the
/// questions session's link and the reminders line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FooterLinks {
    /// The loopback URL.
    pub local: String,
    /// The tailnet URL; `""` drops the tail link.
    pub tailnet: String,
    /// The questions session's live Remote Control URL (`questions-rc.json`
    /// with `rc` "on"); `None` leaves `questions: N` unlinked.
    pub questions_url: Option<String>,
    /// The `Reminders (ack): ...` line; `""` for none.
    pub reminders: String,
}

/// The footer line, the tally it was built from and the reminders line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Footer {
    /// The one-line reply footer.
    pub footer: String,
    /// The counts in it.
    pub tally: Tally,
    /// The second footer line (`""` for none); #12029 P2g.
    pub reminders: String,
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

/// `"<d> decisions, <b> blockers"`: the frozen `dashboard-db.py`'s
/// `tally_line` (Bob 2026-11-15: "only report on decision and blocker counts").
// #12029 P2g: the open/kind wording was an older prototype's.
pub fn tally_line(t: &Tally) -> String {
    format!("{} decisions, {} blockers", t.decisions, t.blockers)
}

/// `questions_part(n)`: `""` for none waiting, else `questions: N`, linked
/// to `url` when the questions session has one.
pub fn questions_part(n: usize, url: Option<&str>) -> String {
    match (n, url.filter(|u| !u.is_empty())) {
        (0, _) => String::new(),
        (n, Some(u)) => format!("[questions: {n}]({u})"),
        (n, None) => format!("questions: {n}"),
    }
}

/// The reply footer: `Dashboard: [local](…) · [tail](…) — <tally line>`,
/// then ` · <questions part>` while interactive items wait, from [`tally`]
/// only; the reminders line rides along.
///
/// # Errors
/// Whatever [`tally`] reports; no footer is built from a guessed count.
pub fn footer(store: &Store, now: f64, links: &FooterLinks) -> Result<Footer, DashError> {
    let tally = tally(store, now)?;
    let tail = if links.tailnet.is_empty() {
        String::new()
    } else {
        format!(" \u{b7} [tail]({})", links.tailnet)
    };
    let mut footer = format!(
        "Dashboard: [local]({}){tail} \u{2014} {}",
        links.local,
        tally_line(&tally)
    );
    let q = questions_part(
        tally.interactive.unwrap_or(0),
        links.questions_url.as_deref(),
    );
    if !q.is_empty() {
        footer.push_str(&format!(" \u{b7} {q}"));
    }
    Ok(Footer {
        footer,
        tally,
        reminders: links.reminders.clone(),
    })
}
