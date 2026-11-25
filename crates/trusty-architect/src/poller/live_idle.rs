//! A shadow cycle's idle epoch: the live poller's `idle_since`, per pane.
//!
//! Why: the `idle` fingerprint is `since-<idle_since>-<screen>`, and
//! `idle_since` is the time of the first cycle that saw the pane idle. The
//! shadow and the live poller run their cycles at different seconds, so each
//! stamped its own epoch and the same idle stretch gave two fingerprints
//! (MB1595 window 3: 18 cycles). A pane one side caught busy for one cycle
//! started a new epoch on that side only.
//! What: [`read`] takes the `panes` map of the live `poll-state.json` (read
//! only) as `session|pane` to `idle_since`. [`adopt`] puts the live value into
//! the shadow's pane state before `evaluate`: a live epoch is taken, a live
//! `null` (the poller saw the pane busy) clears the shadow's, a pane the live
//! state lacks keeps the shadow's own. A live state that cannot be read keeps
//! every pane's own epoch and logs `live idle ERROR`; a missing one is quiet.
//! Test: `shadow_idle_epoch_follows_the_live_poller`,
//! `shadow_idle_epoch_unreadable_live_state_keeps_its_own_and_logs`.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::Path;

use serde_json::Value;

use crate::classify::PaneState;

/// `session|pane` to the live `idle_since` (`None`: live saw it not idle).
pub type LiveIdle = BTreeMap<String, Option<f64>>;

/// The live poller's idle epochs from `path`; `None` when there are none to
/// follow (missing file, or an unreadable one, which also pushes a log line).
pub fn read(path: &Path, log: &mut Vec<String>) -> Option<LiveIdle> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => return None,
        Err(e) => {
            log.push(format!("live idle ERROR read {}: {e}", path.display()));
            return None;
        }
    };
    let doc: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            log.push(format!("live idle ERROR parse {}: {e}", path.display()));
            return None;
        }
    };
    let Some(panes) = doc.get("panes").and_then(Value::as_object) else {
        return Some(LiveIdle::new());
    };
    // A pane entry without a readable `idle_since` says nothing: skipped.
    let epochs = panes
        .iter()
        .filter_map(|(k, v)| match v.get("idle_since")? {
            Value::Null => Some((k.clone(), None)),
            n => n.as_f64().map(|f| (k.clone(), Some(f))),
        })
        .collect();
    Some(epochs)
}

/// Puts pane `key`'s live epoch into `ps`; see the module doc.
pub fn adopt(ps: &mut PaneState, live: &LiveIdle, key: &str) {
    if let Some(epoch) = live.get(key) {
        ps.idle_since = *epoch;
    }
}
