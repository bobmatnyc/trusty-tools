//! Unit tests for `commands::hook_notify` (#8392).

use super::*;

fn context() -> LineContext {
    LineContext {
        payload: serde_json::json!({
            "session_id": "7f3c0000-0000-4000-8000-000000000001",
            "hook_event_name": "Notification",
            "notification_type": "permission_prompt",
            "message": "Claude needs your permission to use Bash",
            "cwd": "/work/my-app/services"
        }),
        cwd: "/ignored".to_string(),
        project_dir: Some("/work/my-app".to_string()),
        tmux_pane: None,
    }
}

#[test]
fn the_inbox_line_carries_the_prototype_fields() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-29T12:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let line: Value = serde_json::from_str(&inbox_line(&context(), None, now)).unwrap();
    assert_eq!(line["ts"], "2026-09-29T12:00:00Z");
    assert_eq!(line["type"], "permission_prompt");
    assert_eq!(line["project"], "my-app", "project root, not the cwd leaf");
    assert_eq!(line["cwd"], "/work/my-app/services");
    assert_eq!(line["tmux_session"], Value::Null);
    assert_eq!(
        line["display"],
        "[my-app]: Claude needs your permission to use Bash"
    );

    let named: Value =
        serde_json::from_str(&inbox_line(&context(), Some("tm-app".into()), now)).unwrap();
    assert_eq!(
        named["display"],
        "[tm-app]: Claude needs your permission to use Bash"
    );
}

#[test]
fn an_unset_target_forwards_nothing() {
    assert_eq!(
        forward_to_target(&PushTarget::Unset, context(), FORWARD_TIMEOUT),
        Ok(())
    );
}

/// The inbox directory is operator-named; a typo must not mint a directory.
#[test]
fn a_missing_inbox_is_an_error_not_a_mkdir() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("no-such-inbox");
    let err =
        forward_to_target(&PushTarget::Inbox(dir.clone()), context(), FORWARD_TIMEOUT).unwrap_err();
    assert!(err.contains("events.jsonl"), "{err}");
    assert!(!dir.exists());
    assert!(
        forward_to_target(&PushTarget::NotAbsolute, context(), FORWARD_TIMEOUT)
            .unwrap_err()
            .contains("not an absolute path")
    );
}

/// A FIFO with no reader blocks `open` forever; the forward still returns at
/// the bound.
#[test]
fn a_fifo_with_no_reader_times_out() {
    let tmp = tempfile::tempdir().unwrap();
    let fifo = tmp.path().join(INBOX_EVENTS_FILE);
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(made.success());
    let start = std::time::Instant::now();
    let err = forward_to_target(
        &PushTarget::Inbox(tmp.path().to_path_buf()),
        context(),
        Duration::from_millis(200),
    )
    .unwrap_err();
    assert!(err.contains("no answer within 200 ms"), "{err}");
    assert!(start.elapsed() < Duration::from_secs(2));
    // Release the parked writer so the detached thread ends with the test.
    let mut drained = String::new();
    std::io::Read::read_to_string(&mut std::fs::File::open(&fifo).unwrap(), &mut drained).unwrap();
    assert!(drained.contains("permission_prompt"));
}

/// #8392: tmux prefix-matches a bare `-t` value, so only a `%N` pane id may
/// reach `tmux display-message -t`; every other value is dropped before tmux
/// runs.
#[test]
fn only_a_percent_n_tmux_pane_is_a_tmux_target() {
    for bad in ["main", "s:0", "=foo", "%", "%12a", ""] {
        assert_eq!(tmux_pane_id_from_env(Some(bad.to_string())), None, "{bad}");
        assert_eq!(tmux_session_of(bad), None, "{bad}");
    }
    assert_eq!(
        tmux_pane_id_from_env(Some("%12".to_string())),
        Some("%12".to_string())
    );
}
