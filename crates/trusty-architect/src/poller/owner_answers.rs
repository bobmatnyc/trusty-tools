//! The owner steps that read dashboard items: `fleet-poll.py`'s
//! `owner_answer_alerts` (#10670, ported for `tm architect poll` in #11474)
//! and `owner_wont_do_alerts` (#11826, parity window 7).
//!
//! Why: an answered dashboard decision, and an item the owner marked Won't
//! do, must wake the Architect. The port raised neither until #11474, and
//! no `owner_wont_do` until window 7, so those owner turns woke nobody.
//! What: [`owner_answer_alerts`] reads only the `items` collection of the
//! owner-queue store and takes the pending events of the `owner_answers`
//! port (`dashboard_derive.pending`, #12029 P2e), without the items the
//! ILIO trial owns ([`ILIO_REF`]; under [`ILIO_ACTOR`] only those, as
//! `owner_answers.pending` does). It makes one `owner_answer` alert per
//! event whose kind is in [`ANSWER_KINDS`], then one `owner_wont_do` alert per
//! `wont_do` event, each fingerprinted `<id>@<at>` (a re-answer alerts
//! again). A read that fails is `Err` with the error class; a store with no
//! items collection is that error too, except on a store never read before,
//! which is not an install yet (logged only).
//! Test: `tests/owner_answers.rs` (`poller_owner_answers_follow_the_python_pending_rule`),
//! `tests/parity_window7.rs`.

use std::path::{Path, PathBuf};

use crate::classify::{Alert, alert, clean};
use crate::dashboard::pyanswers::{field, pending_of};
use crate::dashboard::{Format, Store};

// #12029 P2e: one ILIO split, the `owner_answers` port's.
pub use crate::dashboard::pyanswers::{ILIO_ACTOR, ILIO_REF};

/// The pending kinds that are owner answers: every kind `fleet-poll.py`'s
/// skip list leaves out of the derive rule's.
pub const ANSWER_KINDS: [&str; 3] = ["answered", "cancelled", "architect_do"];
/// Dedup's key prefix for these alerts (session and pane are empty).
pub const ANSWER_KEY: &str = "||owner_answer|";
/// Dedup's key prefix for `owner_wont_do` alerts (#11826).
pub const WONT_DO_KEY: &str = "||owner_wont_do|";

/// Where the answers live: `DASHBOARD_ROOT`, else `<root>/local/owner-queue`,
/// in `DASHBOARD_FORMAT` (`jsonl`, else the directory layout).
#[derive(Debug, Clone)]
pub struct AnswerStore {
    /// The store root.
    pub root: PathBuf,
    /// The store layout.
    pub format: Format,
    /// `DASHBOARD_ACTOR` is [`ILIO_ACTOR`]: keep only the ILIO items.
    pub ilio_trial: bool,
}

impl AnswerStore {
    /// Reads `DASHBOARD_ROOT`, `DASHBOARD_FORMAT` and `DASHBOARD_ACTOR`
    /// through `get`.
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
            ilio_trial: get("DASHBOARD_ACTOR").as_deref() == Some(ILIO_ACTOR),
        }
    }
}

/// The outcome of one read: the alerts, or the failure's class (`None` when
/// the store is simply not there yet and was never read).
pub type AnswerRead = Result<Vec<Alert>, Option<String>>;

/// The cycle's step: adds the answers and won't-dos (or one
/// `owner_answer_error` alert) to `candidates`; `false` when the read
/// failed, so the caller keeps the [`ANSWER_KEY`] and [`WONT_DO_KEY`] keys
/// through dedup. The store comes from [`AnswerStore::from_env`].
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

/// Python's `text[:77] + "..." if len(text) > 80 else text`, after `clean`.
fn short(text: &str) -> String {
    let text = clean(text);
    if text.chars().count() > 80 {
        format!("{}...", text.chars().take(77).collect::<String>())
    } else {
        text
    }
}

/// The fields of a pending event the alerts read.
struct Event {
    id: String,
    kind: String,
    at: String,
    text: String,
}

/// One `owner_answer` alert per answer still pending, then one
/// `owner_wont_do` alert per won't-do; see the module doc.
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
    // #12029 P2e: the derive rule (self-test canaries skipped, an `answered`
    // text without its note, "(needs target resolution)" on an unlinked
    // Architect Do) replaces the serde copy; the ILIO split is the port's.
    let items = snap.get("items").map_or(&[][..], Vec::as_slice);
    let py = std::iter::once((
        "items".to_owned(),
        items.iter().map(crate::dashboard::ids::py_doc).collect(),
    ))
    .collect();
    let text = |e: &crate::pyjson_load::PyValue, k: &str| field(e, k).unwrap_or_default();
    let events: Vec<Event> = pending_of(&py, now, store.ilio_trial)
        .iter()
        .map(|e| Event {
            id: text(e, "id"),
            kind: text(e, "kind"),
            at: text(e, "at"),
            text: text(e, "text"),
        })
        .collect();
    let mut out = Vec::new();
    for it in events
        .iter()
        .filter(|e| ANSWER_KINDS.contains(&e.kind.as_str()))
    {
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
        let text = short(&it.text);
        a.display = format!("[owner] {}", a.detail)
            + &(if text.is_empty() {
                String::new()
            } else {
                format!(": {text}")
            });
        out.push(a);
    }
    // #11826 window 7: fleet-poll.py:819-836 `owner_wont_do_alerts`.
    for it in events.iter().filter(|e| e.kind == "wont_do") {
        let detail = format!("{} won't do", it.id);
        let fp = format!("{}@{}", it.id, it.at);
        let mut a = alert("owner_wont_do", &fp, &detail, "", "", now);
        a.display = format!("[owner] {} won't do: {}", it.id, short(&it.text));
        out.push(a);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ILIO trial (`DASHBOARD_ACTOR=architect-ilio`) keeps only
    /// its own items; the live Architect drops them.
    #[test]
    fn ilio_trial_keeps_only_the_ilio_items() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let items = dir.path().join("items");
        std::fs::create_dir_all(&items)?;
        let doc = |refs: &str| {
            format!(
                r#"{{"status": "answered", "answer": "A", "answered_at": "2026-11-25T10:00:00Z", "refs": {refs}, "version": 1}}"#
            )
        };
        std::fs::write(items.join("Dnj.json"), doc(r#"["arch:ilio"]"#))?;
        std::fs::write(items.join("D0i.json"), doc("[]"))?;
        let root = dir.path().display().to_string();
        let root = root.as_str();
        let get = |trial: bool| {
            move |k: &str| match k {
                "DASHBOARD_ROOT" => Some(root.to_owned()),
                "DASHBOARD_ACTOR" if trial => Some(ILIO_ACTOR.to_owned()),
                _ => None,
            }
        };
        let fps = |trial: bool| -> Result<Vec<String>, Option<String>> {
            let store = AnswerStore::from_env(dir.path(), &get(trial));
            let found = owner_answer_alerts(&store, 0.0, true)?;
            Ok(found.into_iter().map(|a| a.fingerprint).collect())
        };
        let id = |fp: &[String]| fp.iter().map(|f| f[..3].to_owned()).collect::<Vec<_>>();
        assert_eq!(id(&fps(false).map_err(|e| format!("{e:?}"))?), ["D0i"]);
        assert_eq!(id(&fps(true).map_err(|e| format!("{e:?}"))?), ["Dnj"]);
        Ok(())
    }
}
