//! Tests for the daemon's channel audit log (#9832).
//!
//! Why: the log must be owner-only, fenced in tests, and never carry
//! message text; each test fails a writer that drops one of those.
//! What: temp homes only; the route-change arms with real git live in
//! `snapshot_tests.rs`.
//! Test: this is the test module.

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use trusty_channels::audit::{AuditEvent, AuditRecord};
use trusty_channels::policy::{Channel, MessageKind};

use super::{ChannelAudit, append, audit_path};

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
}

/// A canonical temp home.
fn home() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = std::fs::canonicalize(tmp.path()).expect("canonical");
    (tmp, home)
}

/// The keys of every line of the log under `home`.
fn keys(home: &Path) -> Vec<BTreeSet<String>> {
    std::fs::read_to_string(audit_path(home))
        .expect("read log")
        .lines()
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).expect("JSON line");
            v.as_object().expect("object").keys().cloned().collect()
        })
        .collect()
}

fn set(keys: &[&str]) -> BTreeSet<String> {
    keys.iter().map(|k| (*k).to_string()).collect()
}

#[test]
fn audit_file_is_0600_and_dir_0700() {
    let (_tmp, home) = home();
    let path = audit_path(&home);
    let record = AuditRecord::new(AuditEvent::RouteChanged, "first_load");
    append(&path, std::slice::from_ref(&record)).expect("append");
    assert_eq!(mode(path.parent().expect("dir")), 0o700, "directory");
    assert_eq!(mode(&path), 0o600, "log");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    append(&path, &[record]).expect("append");
    assert_eq!(mode(&path), 0o600, "an existing wider log is narrowed");
}

#[test]
fn home_write_fence_is_called() {
    let root = crate::core::home_write_fence::armed_roots()
        .first()
        .expect("the lib test binary arms the fence")
        .clone();
    let dir = root.join(format!("p3-fence-{}", uuid::Uuid::new_v4()));
    let path = dir.join("audit.jsonl");
    let record = AuditRecord::new(AuditEvent::RouteChanged, "first_load");
    let caught = std::panic::catch_unwind(|| append(&path, std::slice::from_ref(&record)));
    let written = dir.exists();
    if written {
        // The fence was bypassed: undo the write before failing.
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(!written, "append wrote under the fenced root");
    let payload = caught.expect_err("append under a fenced root did not panic");
    let message = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_default();
    assert!(
        message.contains("#8545"),
        "not the fence's panic: {message}"
    );
}

#[test]
fn unknown_sender_logs_only_sender_time_length() {
    let (_tmp, home) = home();
    let audit = ChannelAudit::for_home(Some(&home));
    audit
        .inbound_dropped(Channel::Slack, "U0ABCDEF2", 42, "unknown_sender")
        .expect("logged");
    assert_eq!(
        keys(&home),
        [set(&[
            "ts", "event", "reason", "channel", "sender", "length"
        ])]
    );
    let text = std::fs::read_to_string(audit_path(&home)).expect("read");
    assert!(text.contains("\"inbound_dropped\""), "{text}");
    assert!(text.contains("\"length\":42"), "{text}");
}

/// 🔴 #9873 (AC8): a rate-window drop is a `rate_limited` line holding the
/// sender, the time and the length, never the text or the route.
#[test]
fn rate_limited_logs_sender_time_length_only() {
    let (_tmp, home) = home();
    let audit = ChannelAudit::for_home(Some(&home));
    audit
        .rate_limited(Channel::Slack, "U0ABCDEF2", 17, "rate_limited")
        .expect("logged");
    assert_eq!(
        keys(&home),
        [set(&[
            "ts", "event", "reason", "channel", "sender", "length"
        ])]
    );
    let text = std::fs::read_to_string(audit_path(&home)).expect("read");
    assert!(text.contains("\"rate_limited\",\"reason\""), "{text}");
    assert!(text.contains("\"length\":17"), "{text}");
}

#[test]
fn refused_send_is_logged() {
    let (_tmp, home) = home();
    let audit = ChannelAudit::for_home(Some(&home));
    audit
        .send_refused(
            Channel::Telegram,
            "12345",
            MessageKind::Question,
            "no_route",
        )
        .expect("logged");
    assert_eq!(
        keys(&home),
        [set(&["ts", "event", "reason", "channel", "to", "kind"])]
    );
    let text = std::fs::read_to_string(audit_path(&home)).expect("read");
    assert!(text.contains("\"send_refused\""), "{text}");
    assert!(text.contains("\"telegram\""), "{text}");
}

#[test]
fn channel_audit_with_no_home_is_an_error() {
    // #8433 D5: no home means no log; the caller still drops or refuses.
    let audit = ChannelAudit::for_home(None);
    assert!(
        audit
            .send_refused(
                Channel::Slack,
                "U0ABCDEF2",
                MessageKind::Question,
                "no_route"
            )
            .is_err()
    );
}
