//! The one count path: `tally` and the reply footer built from it.
//!
//! Why: the footer, `tally` and any report count must come from one function,
//! never a hand count, and a store that cannot be read must give an error,
//! never a zero (DOC-82 LDG-5.7, #10063 AC 6).
//! What: [`tally`] reads `items` and `questions` from a [`Store`] and counts
//! open items by kind (parked counted apart, never in `open`),
//! answered-not-relayed, running, and open `questions` as `answers_waiting`
//! (absent when zero). [`tally_line`] and [`footer`] render it in the
//! prototype's words (`dashboard-db.py`); the two links are the caller's
//! (the prototype hard-codes one host's names). A missing root, a corrupt
//! document and a store in the other format are errors.
//! Test: `footer_equals_tally_line`, `unreadable_store_is_error_not_zero`,
//! `tally_counts_kinds_parked_running_and_waiting`.

use serde::Serialize;
use serde_json::Value;

use super::derive::{is_parked, running_count};
use super::error::DashError;
use super::store::{ListQuery, Store};

/// Counts over the store, in the prototype's key order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Tally {
    /// Open, unparked items of kind question, action or task.
    pub open: usize,
    /// Open, unparked questions (D).
    pub questions: usize,
    /// Open, unparked actions (A).
    pub actions: usize,
    /// Open, unparked tasks (T).
    pub tasks: usize,
    /// Answered items whatever their relay state.
    pub answered_unrelayed: usize,
    /// Parked open items (never counted in `open`).
    pub parked: usize,
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
    let mut t = Tally::default();
    for doc in &items {
        let d = &doc.data;
        let status = d.get("status").and_then(Value::as_str);
        if is_parked(d) {
            t.parked += 1;
        } else if status == Some("open") {
            match d.get("kind").and_then(Value::as_str) {
                Some("question") => t.questions += 1,
                Some("action") => t.actions += 1,
                Some("task") => t.tasks += 1,
                _ => {}
            }
        } else if status == Some("answered") {
            t.answered_unrelayed += 1;
        }
    }
    t.open = t.questions + t.actions + t.tasks;
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

/// The reply footer: `Dashboard: [localhost](…) · [tailnet](…) — <tally line>`,
/// from [`tally`] only.
///
/// # Errors
/// Whatever [`tally`] reports; no footer is built from a guessed count.
pub fn footer(store: &Store, now: f64, links: &FooterLinks) -> Result<Footer, DashError> {
    let tally = tally(store, now)?;
    let footer = format!(
        "Dashboard: [localhost]({}) \u{b7} [tailnet]({}) \u{2014} {}",
        links.local,
        links.tailnet,
        tally_line(&tally)
    );
    Ok(Footer { footer, tally })
}
