//! Telegram alert formatting and event-subscription filtering.
//!
//! Why: the Telegram bot pushes alerts for memory pressure and selected hook
//! events. Keeping the *decision* of what to alert on and *how to format* it
//! pure (no network) makes it unit-testable independent of teloxide and the
//! daemon. The bot's runtime just calls these functions.
//! What: [`AlertConfig`] (which event categories the operator subscribed to),
//! [`should_alert`] (the subscription filter), and [`format_memory_alert`] /
//! [`format_event_alert`] (the human-readable message bodies).
//! Test: `cargo test -p trusty-mpm-telegram` covers the filter and formatting.

use trusty_mpm_core::hook::{HookCategory, HookEvent};
use trusty_mpm_core::memory::MemoryPressure;

/// Which hook-event categories an operator wants Telegram alerts for.
///
/// Why: 32 hook events firing on every tool call would spam the chat; the
/// operator opts in to categories (e.g. just permission + memory).
/// What: a set of subscribed [`HookCategory`] values, plus a memory toggle.
/// Test: `subscription_filter_respects_categories`.
#[derive(Debug, Clone, Default)]
pub struct AlertConfig {
    /// Hook categories the operator subscribed to.
    pub categories: Vec<HookCategory>,
    /// When true, memory-pressure alerts are pushed.
    pub memory_alerts: bool,
}

impl AlertConfig {
    /// A sensible default: alert on permission and agent events plus memory.
    ///
    /// Why: these are the categories an absent operator most needs to see —
    /// a session blocked on a permission prompt, an agent failing, or a
    /// session about to hit its context limit.
    /// What: subscribes `Permission` + `Agent` categories and memory alerts.
    /// Test: `default_config_alerts_on_permission`.
    pub fn recommended() -> Self {
        Self {
            categories: vec![HookCategory::Permission, HookCategory::Agent],
            memory_alerts: true,
        }
    }
}

/// True if a hook event should produce a Telegram alert under `config`.
///
/// Why: the bot consults this for every event the daemon reports.
/// What: checks the event's category against the subscribed set.
/// Test: `subscription_filter_respects_categories`.
pub fn should_alert(config: &AlertConfig, event: HookEvent) -> bool {
    config.categories.contains(&event.category())
}

/// True if a memory-pressure level warrants a Telegram alert.
///
/// Why: only `Alert` and `Compact` levels are worth interrupting the operator;
/// `Warn` is shown on the dashboard but not pushed.
/// What: returns true for `Alert`/`Compact` when memory alerts are enabled.
/// Test: `memory_alert_threshold`.
pub fn should_memory_alert(config: &AlertConfig, pressure: MemoryPressure) -> bool {
    config.memory_alerts && pressure >= MemoryPressure::Alert
}

/// Format a memory-pressure alert message.
///
/// Why: the operator needs a glanceable message naming the session and level.
/// What: a one-line string with the session id and pressure level.
/// Test: `memory_alert_message_names_session`.
pub fn format_memory_alert(session_id: &str, pressure: MemoryPressure, fraction: f32) -> String {
    let pct = (fraction * 100.0).round() as u32;
    format!(
        "⚠️ trusty-mpm: session {session_id} memory pressure {pressure:?} ({pct}% of context window)"
    )
}

/// Format a hook-event alert message.
///
/// Why: a uniform, short message for any subscribed event.
/// What: names the event and the originating session.
/// Test: `event_alert_message_names_event`.
pub fn format_event_alert(session_id: &str, event: HookEvent) -> String {
    format!(
        "🔔 trusty-mpm: {} in session {session_id}",
        event.wire_name()
    )
}

/// Format an overseer-block alert message.
///
/// Why: when the overseer blocks a session the operator needs an immediate,
/// glanceable interrupt — this is exactly what push alerts exist for.
/// What: a one-line HTML-safe string naming the session the overseer flagged.
/// Test: `overseer_block_alert_names_session`.
pub fn format_overseer_block_alert(session_id: &str) -> String {
    format!("🛑 trusty-mpm: overseer blocked session {session_id}")
}

/// An alert decided by [`check_and_alert`], ready to be sent to Telegram.
///
/// Why: keeping the polling logic pure (it returns alerts rather than sending
/// them) lets it be unit-tested with no network or teloxide runtime.
/// What: the formatted message body the bot should push.
/// Test: `alert_loop_does_not_panic_on_empty_sessions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAlert {
    /// The fully formatted, ready-to-send message body.
    pub message: String,
}

/// Per-session bookkeeping for the push-alert loop.
///
/// Why: the loop must only alert on *new* events; it tracks the most recent
/// event timestamp it has already alerted on, keyed by session id.
/// What: a plain map of session id to last-seen RFC3339 timestamp string.
/// Test: `check_and_alert` is exercised with empty and populated maps.
pub type LastSeen = std::collections::HashMap<String, String>;

/// Pure core of the push-alert loop: decide which events warrant an alert.
///
/// Why: the I/O (polling `/sessions`, `/sessions/{id}/events/poll`, sending
/// messages) is thin and untestable; the *decision* of which events are new
/// and subscribed is the part worth testing, so it is extracted here.
/// What: walks each session's events, and for every event newer than the
/// `last_seen` timestamp for that session that also passes [`should_alert`],
/// produces a [`PendingAlert`]. Mutates `last_seen` to the newest timestamp
/// observed per session so the next poll does not re-alert.
/// Test: `alert_loop_does_not_panic_on_empty_sessions`,
/// `check_and_alert_emits_for_new_subscribed_event`.
pub fn check_and_alert(
    sessions: &[serde_json::Value],
    events_by_session: &std::collections::HashMap<String, Vec<serde_json::Value>>,
    last_seen: &mut LastSeen,
    config: &AlertConfig,
) -> Vec<PendingAlert> {
    let mut alerts = Vec::new();
    for session in sessions {
        let Some(id) = session["id"].as_str() else {
            continue;
        };
        let Some(events) = events_by_session.get(id) else {
            continue;
        };
        let prev = last_seen.get(id).cloned().unwrap_or_default();
        let mut newest = prev.clone();
        for record in events {
            let at = record["at"].as_str().unwrap_or_default();
            if at <= prev.as_str() {
                continue;
            }
            if at > newest.as_str() {
                newest = at.to_string();
            }
            let Some(event_name) = record["event"].as_str() else {
                continue;
            };
            let Some(event) = HookEvent::from_wire(event_name) else {
                continue;
            };
            if should_alert(config, event) {
                alerts.push(PendingAlert {
                    message: format_event_alert(id, event),
                });
            }
        }
        if !newest.is_empty() {
            last_seen.insert(id.to_string(), newest);
        }
    }
    alerts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_alerts_on_permission() {
        let cfg = AlertConfig::recommended();
        assert!(should_alert(&cfg, HookEvent::PermissionDenied));
        assert!(cfg.memory_alerts);
    }

    #[test]
    fn subscription_filter_respects_categories() {
        let cfg = AlertConfig {
            categories: vec![HookCategory::Permission],
            memory_alerts: false,
        };
        // Subscribed category fires.
        assert!(should_alert(&cfg, HookEvent::PermissionGranted));
        // Unsubscribed category (Tool) does not.
        assert!(!should_alert(&cfg, HookEvent::PreToolUse));
    }

    #[test]
    fn memory_alert_threshold() {
        let cfg = AlertConfig {
            categories: vec![],
            memory_alerts: true,
        };
        assert!(!should_memory_alert(&cfg, MemoryPressure::Warn));
        assert!(should_memory_alert(&cfg, MemoryPressure::Alert));
        assert!(should_memory_alert(&cfg, MemoryPressure::Compact));
        // Disabled config never alerts.
        let off = AlertConfig {
            categories: vec![],
            memory_alerts: false,
        };
        assert!(!should_memory_alert(&off, MemoryPressure::Compact));
    }

    #[test]
    fn memory_alert_message_names_session() {
        let msg = format_memory_alert("sess-1", MemoryPressure::Alert, 0.86);
        assert!(msg.contains("sess-1"));
        assert!(msg.contains("86%"));
        assert!(msg.contains("Alert"));
    }

    #[test]
    fn event_alert_message_names_event() {
        let msg = format_event_alert("sess-2", HookEvent::SubagentStopFailure);
        assert!(msg.contains("sess-2"));
        assert!(msg.contains("SubagentStopFailure"));
    }

    #[test]
    fn overseer_block_alert_names_session() {
        let msg = format_overseer_block_alert("sess-7");
        assert!(msg.contains("sess-7"));
        assert!(msg.contains("overseer"));
    }

    #[test]
    fn alert_loop_does_not_panic_on_empty_sessions() {
        // The pure core of the push-alert loop must be a no-op (and never
        // panic) when there are no sessions and no events.
        let cfg = AlertConfig::recommended();
        let mut last_seen = LastSeen::new();
        let alerts = check_and_alert(&[], &std::collections::HashMap::new(), &mut last_seen, &cfg);
        assert!(alerts.is_empty());
        assert!(last_seen.is_empty());
    }

    #[test]
    fn check_and_alert_emits_for_new_subscribed_event() {
        // A new event in a subscribed category (Permission) produces exactly
        // one alert and advances the per-session last-seen cursor.
        let cfg = AlertConfig::recommended();
        let sessions = vec![serde_json::json!({ "id": "sess-1" })];
        let mut events = std::collections::HashMap::new();
        events.insert(
            "sess-1".to_string(),
            vec![serde_json::json!({
                "event": "PermissionDenied",
                "at": "2026-05-17T10:00:00Z",
            })],
        );
        let mut last_seen = LastSeen::new();
        let alerts = check_and_alert(&sessions, &events, &mut last_seen, &cfg);
        assert_eq!(alerts.len(), 1);
        assert!(alerts[0].message.contains("PermissionDenied"));
        assert_eq!(
            last_seen.get("sess-1").map(String::as_str),
            Some("2026-05-17T10:00:00Z")
        );
        // A second poll with the same event must not re-alert.
        let again = check_and_alert(&sessions, &events, &mut last_seen, &cfg);
        assert!(again.is_empty());
    }

    #[test]
    fn check_and_alert_skips_unsubscribed_category() {
        // A `PreToolUse` event (Tool category) is not in the recommended
        // subscription, so it must not produce an alert.
        let cfg = AlertConfig::recommended();
        let sessions = vec![serde_json::json!({ "id": "sess-1" })];
        let mut events = std::collections::HashMap::new();
        events.insert(
            "sess-1".to_string(),
            vec![serde_json::json!({
                "event": "PreToolUse",
                "at": "2026-05-17T10:00:00Z",
            })],
        );
        let mut last_seen = LastSeen::new();
        let alerts = check_and_alert(&sessions, &events, &mut last_seen, &cfg);
        assert!(alerts.is_empty());
    }
}
