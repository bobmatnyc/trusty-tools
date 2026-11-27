//! The `user_state` step: `fleet-poll.py`'s `user_state_alerts`
//! (fleet-poll.py:859-868, reading `user_state.py`'s `read`), #11826 parity
//! window 7.
//!
//! Why: the owner's presence state (dashboard settings pane, auto-sleep, an
//! owner signal) tunes how the Architect engages; each change wakes it. The
//! port had no such step, so a state change woke nobody.
//! What: [`user_state_alerts`] reads `meta/settings` from the owner-queue
//! store. A `user_state` in [`STATES`] with a non-empty `user_state_at` gives
//! one `user_state` alert fingerprinted by that time; a missing document, an
//! unknown state or no time gives none (Python's default, `active`, is not a
//! change). Any other read failure is `Err` with the error class:
//! [`user_state_step`] logs it, raises one `user_state_error` alert and returns
//! `false` so the cycle keeps the [`USER_STATE_KEY`] keys through dedup.
//! Test: `window7_user_state_rows_raise_on_the_scheduler`,
//! `window7_unreadable_settings_raise_one_error_and_keep_the_user_state_key`.

use serde_json::Value;

use super::owner_answers::AnswerStore;
use crate::classify::{Alert, alert};
use crate::dashboard::ids::py_str;
use crate::dashboard::{DashError, Store};

/// The presence states `user_state.py` accepts.
pub const STATES: [&str; 5] = ["active", "remote", "mobile", "away", "sleeping"];
/// Dedup's key prefix for `user_state` alerts (session and pane are empty).
pub const USER_STATE_KEY: &str = "||user_state|";

/// One `user_state` alert for the stored state, or the read failure's class.
///
/// # Errors
/// `"StoreError"` when `meta/settings` exists but cannot be read.
pub fn user_state_alerts(store: &AnswerStore, now: f64) -> Result<Vec<Alert>, String> {
    let doc = match Store::new(&store.root, store.format).get("meta", "settings") {
        Ok(doc) => doc,
        Err(DashError::NotFound(_)) => return Ok(Vec::new()),
        // Python's `ds.StoreError`, raised out of the cycle; here an alert.
        Err(_) => return Err("StoreError".to_owned()),
    };
    let Some(state) = doc
        .data
        .get("user_state")
        .and_then(Value::as_str)
        .filter(|s| STATES.contains(s))
    else {
        return Ok(Vec::new());
    };
    // `str(data.get("user_state_at", ""))`.
    let at = doc
        .data
        .get("user_state_at")
        .map(py_str)
        .unwrap_or_default();
    if at.is_empty() {
        return Ok(Vec::new());
    }
    let detail = format!("user state -> {state}");
    let mut a = alert("user_state", &at, &detail, "", "", now);
    a.display = format!("[dashboard]: user state -> {state}");
    Ok(vec![a])
}

/// The cycle's step; see the module doc. The store comes from
/// [`AnswerStore::from_env`].
pub(crate) fn user_state_step(
    config: &super::Config,
    now: f64,
    candidates: &mut Vec<Alert>,
    log: &mut Vec<String>,
) -> bool {
    let store = AnswerStore::from_env(&config.root, &|k| std::env::var(k).ok());
    match user_state_alerts(&store, now) {
        Ok(found) => {
            candidates.extend(found);
            true
        }
        Err(class) => {
            log.push(format!(
                "user_state ERROR {class}: {}; retrying next cycle",
                store.root.display()
            ));
            let detail = format!("cannot read the owner's user state ({class}); see poll.log");
            candidates.push(alert("user_state_error", &class, &detail, "", "", now));
            false
        }
    }
}
