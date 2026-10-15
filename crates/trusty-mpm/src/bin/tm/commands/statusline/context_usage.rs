//! Persist the tick's context-window and plan figures (#8394).
//!
//! Why: the `statusLine` payload is the only place these figures reach `tm`,
//! and a fleet poller needs them without scraping a truncated pane. The store
//! and its rules live in [`trusty_mpm::core::context_usage`]; this is the
//! statusline's call into it.
//! What: [`persist_context_usage`] runs after the status line is printed, so a
//! slow or failing write never costs the render. A failed write is logged to
//! `tracing` and to stderr — Claude Code shows only stdout in the status bar.
//! Test: `a_tick_records_its_figures`, `a_failed_persist_is_logged`,
//! `a_payload_without_figures_writes_nothing`.

use std::io::Write;
use std::path::Path;

use chrono::{DateTime, Utc};
use trusty_mpm::core::context_usage::{ContextUsage, record_context_usage};

/// Persist this tick's figures under the framework root, logging a failure.
///
/// What: [`persist_context_usage_at`] against the root the savings ledger
/// uses, `Utc::now()`, and stderr.
/// Test: through [`persist_context_usage_at`].
pub(crate) fn persist_context_usage(raw: &str) {
    let root = super::savings::savings_root();
    persist_context_usage_at(root.as_deref(), raw, Utc::now(), &mut std::io::stderr());
}

/// [`persist_context_usage`] against an explicit root, clock and log sink.
///
/// Why: this bin target may not repoint `HOME` or the framework root in its
/// tests (#5544), so the root arrives as an argument, and the log sink is
/// injected so the failure arm is assertable.
/// What: no-op when `raw` is not JSON, has no `session_id`, or carries no
/// figures. Otherwise writes the reading; on an error, or when no root
/// resolves, writes one `tm statusline: context usage not recorded …` line to
/// `log` and to `tracing::warn!`.
/// Test: `a_tick_records_its_figures`, `a_failed_persist_is_logged`,
/// `a_payload_without_figures_writes_nothing`.
pub(crate) fn persist_context_usage_at(
    root: Option<&Path>,
    raw: &str,
    now: DateTime<Utc>,
    log: &mut dyn Write,
) {
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(raw) else {
        return;
    };
    let session_id = payload
        .get("session_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if session_id.is_empty() {
        return;
    }
    let Some(usage) = ContextUsage::from_statusline_json(&payload, now) else {
        return;
    };
    let failure = match root {
        None => "no framework root resolves".to_string(),
        Some(root) => match record_context_usage(root, session_id, &usage) {
            Ok(_) => return,
            Err(e) => e.to_string(),
        },
    };
    tracing::warn!(session_id, error = %failure, "context usage not recorded");
    let _ = writeln!(
        log,
        "tm statusline: context usage not recorded for session {session_id}: {failure}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use trusty_mpm::core::context_usage::read_context_usage;

    const PAYLOAD: &str = r#"{
        "session_id": "sess-ctx",
        "context_window": {"total_input_tokens": 82000, "context_window_size": 200000, "used_percentage": 41},
        "rate_limits": {"five_hour": {"used_percentage": 24, "resets_at": 1760450400}}
    }"#;

    fn now() -> DateTime<Utc> {
        Utc.timestamp_opt(1_760_440_000, 0).single().expect("valid")
    }

    /// Why: the tick is the only writer; its figures must reach the store.
    /// Test: itself.
    #[test]
    fn a_tick_records_its_figures() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut log = Vec::new();
        persist_context_usage_at(Some(dir.path()), PAYLOAD, now(), &mut log);
        assert!(
            log.is_empty(),
            "no failure to log: {}",
            String::from_utf8_lossy(&log)
        );
        let usage = read_context_usage(dir.path(), "sess-ctx", now()).expect("recorded");
        assert_eq!(usage.total_input_tokens, Some(82_000));
        assert_eq!(usage.used_percentage, Some(41.0));
        let plan = usage.plan.expect("plan");
        assert_eq!(plan.five_hour.and_then(|w| w.used_percentage), Some(24.0));
    }

    /// Why (#8394 Fail-Open Check): a failed write must not break the status
    /// line, and must not vanish either.
    /// Test: itself.
    #[test]
    fn a_failed_persist_is_logged() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().join("a-file-not-a-dir");
        std::fs::write(&root, b"x").expect("write blocker");
        let mut log = Vec::new();
        persist_context_usage_at(Some(&root), PAYLOAD, now(), &mut log);
        let text = String::from_utf8_lossy(&log);
        assert!(
            text.contains("tm statusline: context usage not recorded") && text.contains("sess-ctx"),
            "the failure must be logged: {text:?}"
        );

        let mut log = Vec::new();
        persist_context_usage_at(None, PAYLOAD, now(), &mut log);
        assert!(
            String::from_utf8_lossy(&log).contains("context usage not recorded"),
            "an unresolvable root is logged too"
        );
    }

    /// Why: an empty, invalid or figure-free payload is not a reading.
    /// Test: itself.
    #[test]
    fn a_payload_without_figures_writes_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut log = Vec::new();
        for raw in [
            "",
            "not json",
            r#"{"session_id":"s1"}"#,
            r#"{"context_window":{}}"#,
        ] {
            persist_context_usage_at(Some(dir.path()), raw, now(), &mut log);
        }
        assert!(log.is_empty());
        assert!(!dir.path().join("usage").exists(), "nothing written");
    }
}
