//! D5r / D5t: push a failure the Architect has not cleared in 30 min to the
//! owner, once per incident (#9779).
//!
//! Why: the watchdog's last resort is a message that does not depend on the
//! Architect reading its inbox.
//! What: [`push_step`] tracks each failing alert's key in `state["push"]`
//! (`since`, `reported`, `failed_at`). A key active for [`PUSH_AFTER`] is
//! due once. With `ARCHITECT_PUSH_CHANNEL` unset it logs that nothing was
//! sent; a dry or shadow run logs "would push"; `slack` sends a DM to
//! `ARCHITECT_PUSH_SLACK_USER` through the [`PushSender`]. A sent key is
//! `reported` and never sent again until it clears and comes back. A failed
//! send (unknown channel, no recipient, no sender, or a [`PushError`]) is a
//! `push_failed` alert with the failure kind as fingerprint, and the key is
//! tried again only after another [`PUSH_AFTER`].
//! Test: `push_due_fires_once_after_thirty_minutes_and_clears`
//! (tests/d5r_shadow.rs), `push_off_sends_nothing`,
//! `push_on_sends_once_per_incident`, `push_failure_kinds_are_alerts`,
//! `push_shadow_sends_nothing` (tests/push_slack.rs).

use serde_json::{Map, Value};

use super::slack::{PushError, PushSender};
use crate::classify::{Alert, alert};

/// Seconds a failure stays active before it is due a push; also the retry
/// interval after a failed send.
pub const PUSH_AFTER: f64 = 1800.0;
/// Alert kind of a failed push.
pub const PUSH_FAILED: &str = "push_failed";
/// The one channel with a sender.
pub const SLACK_CHANNEL: &str = "slack";

/// Where and how a due push goes.
#[derive(Clone, Copy)]
pub struct PushRoute<'a> {
    /// `ARCHITECT_PUSH_CHANNEL`; `None` sends nothing.
    pub channel: Option<&'a str>,
    /// `ARCHITECT_PUSH_SLACK_USER`: the Slack user id that gets the DM.
    pub recipient: Option<&'a str>,
    /// The sender; `None` is a failure when a channel is set.
    pub sender: Option<&'a dyn PushSender>,
    /// A dry or shadow run: log what would be sent, send nothing.
    pub hands_off: bool,
    /// Session that `push_failed` alerts are raised on.
    pub supervisor: &'a str,
}

/// The owner-facing text for one incident.
pub fn push_text(a: &Alert) -> String {
    format!(
        "Architect is broken: {} ({}:{}) has not been cleared for 30 min.",
        a.detail, a.kind, a.fingerprint
    )
}

/// One cycle's push bookkeeping and sends; returns the `push_failed`
/// alerts and log lines. See the module docs.
pub fn push_step(
    other: &mut Map<String, Value>,
    failing: &[Alert],
    now: f64,
    route: &PushRoute<'_>,
) -> (Vec<Alert>, Vec<String>) {
    if failing.is_empty() && !other.contains_key("push") {
        return (Vec::new(), Vec::new()); // nothing tracked: leave the state untouched
    }
    let entry = other
        .entry("push".to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    let (mut alerts, mut lines) = (Vec::new(), Vec::new());
    if !entry.is_object() {
        *entry = Value::Object(Map::new());
        lines.push("push: state record was not an object; reset".to_owned());
    }
    let Value::Object(book) = entry else {
        return (alerts, lines);
    };
    let key = |a: &Alert| format!("{}:{}", a.kind, a.fingerprint);
    let active: Vec<String> = failing.iter().map(key).collect();
    book.retain(|k, _| active.contains(k));
    for a in failing {
        let k = key(a);
        let rec = book.entry(k.clone()).or_insert_with(|| {
            let mut m = Map::new();
            m.insert("since".to_owned(), Value::from(now));
            Value::Object(m)
        });
        let Value::Object(m) = rec else { continue };
        let since = m.get("since").and_then(Value::as_f64).unwrap_or(now);
        let last = m.get("failed_at").and_then(Value::as_f64).unwrap_or(since);
        if m.get("reported").and_then(Value::as_bool) == Some(true) || now - last < PUSH_AFTER {
            continue;
        }
        match deliver(a, route) {
            Ok(line) => {
                m.insert("reported".to_owned(), Value::Bool(true));
                m.remove("failed_at");
                lines.push(line);
            }
            Err((kind, detail)) => {
                m.insert("failed_at".to_owned(), Value::from(now));
                lines.push(format!("push ERROR {k}: {detail}"));
                let detail = format!("push of {k} failed: {detail}");
                alerts.push(alert(PUSH_FAILED, kind, &detail, route.supervisor, "", now));
            }
        }
    }
    (alerts, lines)
}

/// Sends one due push; `Ok` is the log line, `Err` the fingerprint and detail.
fn deliver(a: &Alert, route: &PushRoute<'_>) -> Result<String, (&'static str, String)> {
    let k = format!("{}:{}", a.kind, a.fingerprint);
    let Some(channel) = route.channel else {
        return Ok(format!(
            "push due: {k} uncleared 30 min (ARCHITECT_PUSH_CHANNEL unset; nothing sent)"
        ));
    };
    if channel != SLACK_CHANNEL {
        let why = format!("ARCHITECT_PUSH_CHANNEL={channel} has no sender (want slack)");
        return Err(("channel", why));
    }
    let Some(to) = route.recipient.filter(|r| !r.is_empty()) else {
        return Err(("recipient", "ARCHITECT_PUSH_SLACK_USER unset".to_owned()));
    };
    if route.hands_off {
        return Ok(format!("would push: {k} to slack user {to}"));
    }
    let Some(sender) = route.sender else {
        return Err(("sender", "no push sender in this run".to_owned()));
    };
    sender
        .send(to, &push_text(a))
        .map(|()| format!("pushed: {k} to slack user {to}"))
        .map_err(|e: PushError| (e.kind(), e.to_string()))
}
