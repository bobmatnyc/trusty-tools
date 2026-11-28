//! #12077: a config-apply restart never runs an opted-in session unsandboxed.

use super::*;

/// A live record named `tm-sess` with the given sandbox opt-in.
fn record(claude_sandbox: bool) -> SessionRecord {
    serde_json::from_value(serde_json::json!({
        "id": "0708af71-2b5e-5dab-8c92-739ebdb04941",
        "task": "PM",
        "tmux_name": "tm-sess",
        "cwd": "/",
        "state": "active",
        "created_at": "2026-09-30T00:00:00Z",
        "pane_id": "%3",
        "claude_sandbox": claude_sandbox,
    }))
    .expect("record")
}

/// #12077: a session that opted in is refused a config-apply restart, as a
/// 409 naming the switch, before any pane is looked up or typed into.
#[test]
fn an_opted_in_session_restart_is_refused_12077() {
    let err = refuse_a_sandboxed_restart(&record(true), "tm-sess").expect_err("refused");
    assert_eq!(err.status(), StatusCode::CONFLICT);
    let text = err.to_string();
    assert!(text.contains("TM_CLAUDE_SANDBOX"), "{text}");
    assert!(text.contains("tm-sess"), "{text}");
}

/// #12077: a default session's restart goes ahead exactly as before, and the
/// line it types is still the bare scrubbed `claude`.
#[test]
fn a_default_session_restart_is_unchanged_12077() {
    refuse_a_sandboxed_restart(&record(false), "tm-sess").expect("not refused");
    assert_eq!(
        crate::daemon::spawn_command::relaunch_command(),
        format!(
            "env{} claude",
            crate::core::claude_env_scrub::env_unset_flags()
        )
    );
}
