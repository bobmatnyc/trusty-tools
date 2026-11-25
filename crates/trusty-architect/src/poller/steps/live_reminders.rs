//! A shadow cycle's `reminder` alerts for reminders the live poller fired.
//!
//! Why: `fleet-poll.py`'s reminder step fires each due reminder once in the
//! Intent Ledger (pending to `fired`) and alerts for it. The shadow's ledger
//! shim runs the same step dry, which sees only pending reminders, so a
//! reminder the live poller fired before the shadow's next cycle never
//! reached the shadow (MB1595 window 3: the 11:05:01Z reminder).
//! What: in a shadow run, [`shadow_reminder_step`] reads the ledger (read
//! only) and, for each reminder in state `fired` whose `fired_at` minute is at
//! or after the minute of the previous shadow look ([`SEEN_KEY`] in the
//! state), raises the `reminder` alert the live step raised: fingerprint
//! `fp("<task>#<n>@<at>")`, detail `<task> #<n>: <note> (due <at>; task:
//! <title[:60]>)`. A reminder the dry shim already raised while pending is
//! still active in the dedup state, so it is not new twice. A ledger that
//! cannot be read logs `live reminders ERROR` and keeps the mark, so the next
//! cycle reads the same window again.
//! Test: `shadow_reminder_fired_by_the_live_poller_is_raised_once`,
//! `shadow_reminder_unreadable_ledger_logs_and_keeps_the_window`.

use serde_json::{Map, Value};

use crate::classify::alerts::fingerprint;
use crate::classify::{Alert, alert};
use crate::il::json::Json;
use crate::il::store::{Where, load_state, resolve};
use crate::il::time::{Shape, strptime};
use crate::poller::Config;

/// The state key holding the time of the previous shadow look.
pub const SEEN_KEY: &str = "shadow_reminders_through";
/// The first look covers this many seconds back (one shim cadence).
const FIRST_LOOK_S: f64 = 60.0;

/// `reminder` alerts for reminders fired at or after `since`'s minute.
pub fn fired_since(st: &Json, since: f64, now: f64) -> Vec<Alert> {
    let since = since as i64;
    let floor = since - since.rem_euclid(60);
    let mut out = Vec::new();
    for t in st.arr("tasks") {
        for r in t.arr("reminders") {
            let fired = strptime(r.s("fired_at"), Shape::Minute);
            if r.s("state") != "fired" || fired.is_none_or(|f| f < floor || f as f64 > now) {
                continue;
            }
            let (task, n, at) = (t.s("id"), r.i("n").unwrap_or(0), r.s("at"));
            let title: String = t.s("title").chars().take(60).collect();
            let detail = format!("{task} #{n}: {} (due {at}; task: {title})", r.s("note"));
            let fp = fingerprint(&format!("{task}#{n}@{at}"));
            out.push(alert("reminder", &fp, &detail, "", "", now));
        }
    }
    out
}

/// The shadow cycle's step; see the module doc. Returns alerts and log lines.
pub fn shadow_reminder_step(
    cfg: &Config,
    other: &mut Map<String, Value>,
    now: f64,
) -> (Vec<Alert>, Vec<String>) {
    let since = other
        .get(SEEN_KEY)
        .and_then(Value::as_f64)
        .unwrap_or(now - FIRST_LOOK_S);
    let w = Where {
        project: cfg.root.join("local").join("il"),
        ..Where::default()
    };
    match resolve(&w).and_then(|p| load_state(&p)) {
        Ok(st) => {
            other.insert(SEEN_KEY.to_owned(), now.into());
            (fired_since(&st, since, now), Vec::new())
        }
        Err(e) => (Vec::new(), vec![format!("live reminders ERROR {e}")]),
    }
}
