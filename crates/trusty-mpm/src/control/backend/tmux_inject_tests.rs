//! `TmuxBackend::send` reports each delivered text once (#9167 AC5).
//!
//! Why: the control-plane tmux backend types into a pane without passing the
//! session manager's inject path, so it records on its own.
//! What: a backend over a stub `tmux` script that exits 0 or 1, and a
//! recorder on its `InjectSlot`.
//! Test: this file.

use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use super::*;
use crate::session_manager::inject_record::{Delivery, InjectRecorder};

/// Records `(tmux_name, text)` of every delivery.
#[derive(Default)]
struct Seen(Mutex<Vec<(String, String)>>);

impl InjectRecorder for Seen {
    fn delivered(&self, d: &Delivery<'_>) {
        self.0
            .lock()
            .unwrap()
            .push((d.tmux_name.to_owned(), d.text.to_owned()));
    }
}

/// A backend whose `tmux` is a stub exiting `code`, reporting to `seen`.
fn backend(dir: &tempfile::TempDir, code: i32, seen: &Arc<Seen>) -> TmuxBackend {
    let bin = dir.path().join("tmux");
    std::fs::write(&bin, format!("#!/bin/sh\nexit {code}\n")).expect("stub");
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let slot = InjectSlot::default();
    assert!(slot.set(seen.clone()));
    TmuxBackend {
        session_id: ControlSessionId::new("proj", 0),
        tmux_name: "tm:proj:0".into(),
        driver: TmuxDriver::with_tmux_path_for_test(bin.to_string_lossy().into_owned()),
        // Never spawned, so Drop kills nothing.
        spawned: false,
        capture_lines: 100,
        last_pane_content: String::new(),
        inject: InjectSlot::default(),
    }
    .with_inject_slot(slot)
}

/// A delivered send is one event naming the pane's session and the text.
#[tokio::test]
async fn a_delivered_send_is_one_message_event_9167() {
    let dir = tempfile::tempdir().expect("tempdir");
    let seen = Arc::new(Seen::default());
    let mut b = backend(&dir, 0, &seen);

    b.send(SessionInput::new("hello")).await.expect("sent");

    assert_eq!(
        *seen.0.lock().unwrap(),
        [("tm:proj:0".to_string(), "hello".to_string())]
    );
}

/// A send tmux refused typed nothing, so records nothing.
#[tokio::test]
async fn a_failed_send_records_nothing_9167() {
    let dir = tempfile::tempdir().expect("tempdir");
    let seen = Arc::new(Seen::default());
    let mut b = backend(&dir, 1, &seen);

    assert!(b.send(SessionInput::new("hello")).await.is_err());

    assert!(seen.0.lock().unwrap().is_empty());
}
