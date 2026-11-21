//! The `owner_answer` step: `fleet_poll_alerts.py`'s `owner_answer_alerts`
//! (#10670), ported for `tm architect poll` (#11474).
//!
//! Why: an answered dashboard decision must wake the Architect. The Python
//! poller raised one `owner_answer` alert per answer; the port raised none, so
//! an answer woke nobody.
//! What: [`owner_answer_alerts`] reads only the `items` collection of the
//! owner-queue store, takes the pending events whose kind is in
//! [`ANSWER_KINDS`], and makes one `owner_answer` alert each, fingerprinted
//! `<id>@<at>` (a re-answer alerts again). A read that fails is `Err` with the
//! error class; a store with no items collection is that error too, except on
//! a store never read before, which is not an install yet (logged only).
//! Test: `tests/owner_answers.rs`.

use std::path::{Path, PathBuf};

use crate::classify::{Alert, alert, clean};
use crate::dashboard::{Format, Store, pending};

/// The pending kinds that are owner answers.
pub const ANSWER_KINDS: [&str; 3] = ["answered", "cancelled", "architect_do"];
/// Dedup's key prefix for these alerts (session and pane are empty).
pub const ANSWER_KEY: &str = "||owner_answer|";

/// Where the answers live: `DASHBOARD_ROOT`, else `<root>/local/owner-queue`,
/// in `DASHBOARD_FORMAT` (`jsonl`, else the directory layout).
#[derive(Debug, Clone)]
pub struct AnswerStore {
    /// The store root.
    pub root: PathBuf,
    /// The store layout.
    pub format: Format,
}

impl AnswerStore {
    /// Reads `DASHBOARD_ROOT` and `DASHBOARD_FORMAT` through `get`.
    pub fn from_env(root: &Path, get: &dyn Fn(&str) -> Option<String>) -> Self {
        let store = get("DASHBOARD_ROOT")
            .filter(|v| !v.is_empty())
            .map_or_else(|| root.join("local/owner-queue"), PathBuf::from);
        let format = match get("DASHBOARD_FORMAT").as_deref() {
            Some("jsonl") => Format::Jsonl,
            _ => Format::Dir,
        };
        Self {
            root: store,
            format,
        }
    }
}

/// The outcome of one read: the alerts, or the failure's class (`None` when
/// the store is simply not there yet and was never read).
pub type AnswerRead = Result<Vec<Alert>, Option<String>>;

/// The cycle's step: adds the answers (or one `owner_answer_error` alert) to
/// `candidates`; `false` when the read failed, so the caller keeps the
/// answer keys through dedup. The store comes from [`AnswerStore::from_env`].
pub(crate) fn owner_answer_step(
    config: &super::Config,
    state: &mut super::state::PollState,
    now: f64,
    candidates: &mut Vec<Alert>,
    log: &mut Vec<String>,
) -> bool {
    let store = AnswerStore::from_env(&config.root, &|k| std::env::var(k).ok());
    let ever_read = state.other.get("answers_read") == Some(&serde_json::Value::Bool(true));
    match owner_answer_alerts(&store, now, ever_read) {
        Ok(found) => {
            state
                .other
                .insert("answers_read".to_owned(), serde_json::Value::Bool(true));
            candidates.extend(found);
            true
        }
        Err(class) => {
            let why = class.as_deref().unwrap_or("FileNotFoundError");
            log.push(format!(
                "owner answers ERROR {why}: {}; retrying next cycle",
                store.root.display()
            ));
            if let Some(class) = class {
                let detail = format!("cannot read owner answers ({class}); see poll.log");
                candidates.push(alert("owner_answer_error", &class, &detail, "", "", now));
            }
            false
        }
    }
}

/// One `owner_answer` alert per answer still pending; see the module doc.
pub fn owner_answer_alerts(store: &AnswerStore, now: f64, ever_read: bool) -> AnswerRead {
    let root = &store.root;
    let present = root.join("items").is_dir() || root.join("items.jsonl").exists();
    if !present {
        return Err((ever_read).then(|| "FileNotFoundError".to_owned()));
    }
    let snap = Store::new(root, store.format)
        .snapshot(&["items"])
        // The fingerprint is the error class, as Python's `type(e).__name__`.
        .map_err(|_| Some("StoreError".to_owned()))?;
    let mut out = Vec::new();
    for it in pending(&snap, now).events() {
        if !ANSWER_KINDS.contains(&it.kind.as_str()) {
            continue;
        }
        let verb = if it.kind == "cancelled" {
            "cancelled"
        } else {
            "answered"
        };
        let hhmm = it
            .at
            .get(11..16)
            .map_or_else(|| "?".to_owned(), |t| format!("{t}Z"));
        let detail = format!("{} {verb} {hhmm}", it.id);
        let mut a = alert(
            "owner_answer",
            &format!("{}@{}", it.id, it.at),
            &detail,
            "",
            "",
            now,
        );
        let text = clean(&it.text);
        let text = if text.chars().count() > 80 {
            format!("{}...", text.chars().take(77).collect::<String>())
        } else {
            text
        };
        a.display = format!("[owner] {}", a.detail)
            + &(if text.is_empty() {
                String::new()
            } else {
                format!(": {text}")
            });
        out.push(a);
    }
    Ok(out)
}
