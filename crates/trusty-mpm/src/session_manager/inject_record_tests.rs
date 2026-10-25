//! The inject path reports each delivered text exactly once (#9167 AC5).
//!
//! Why: AC5 counts one `message` event per inject; a missed delivery or a
//! recorded refusal breaks the count match.
//! What: a recording [`InjectRecorder`] on a manager over the fake tmux
//! driver; each test drives one `inject` outcome.
//! Test: this file.

use std::sync::{Arc, Mutex};

use tempfile::TempDir;

use super::{Delivery, InjectRecorder};
use crate::core::process::PaneClaude;
use crate::core::sm::control::Submit;
use crate::session_manager::manager::SessionManager;
use crate::session_manager::record::{ManagedSessionState, SessionRecord};
use crate::session_manager::tests::{FakeTmuxDriver, make_manager};

/// What a [`Recording`] saw: `(tmux_name, claude_session_id, text)`.
type Seen = Vec<(String, Option<String>, String)>;

/// Records every delivery it is handed.
#[derive(Default)]
struct Recording(Mutex<Seen>);

impl InjectRecorder for Recording {
    fn delivered(&self, d: &Delivery<'_>) {
        self.0.lock().expect("lock").push((
            d.tmux_name.to_string(),
            d.claude_session_id.map(str::to_string),
            d.text.to_string(),
        ));
    }
}

impl Recording {
    fn seen(&self) -> Seen {
        self.0.lock().expect("lock").clone()
    }
}

/// An Active session on pane `%7` with a Claude session id, and a recorder
/// installed on its manager.
async fn active(
    dir: &TempDir,
) -> (
    SessionManager,
    Arc<FakeTmuxDriver>,
    SessionRecord,
    Arc<Recording>,
) {
    let (mgr, fake) = make_manager(dir).await;
    *fake.pane_id_override.lock().expect("lock") = Some("%7".into());
    let record = mgr
        .create(
            "task".into(),
            Some(dir.path().to_path_buf()),
            Some("inject-record".into()),
            Some(dir.path().to_path_buf()),
            None,
            None,
        )
        .await
        .expect("create");
    mgr.set_workspace(
        &record.id,
        dir.path().to_path_buf(),
        ManagedSessionState::Active,
    )
    .await
    .expect("set Active");
    mgr.set_claude_session_id(&record.id, "claude-1")
        .await
        .expect("claude id");
    let recording = Arc::new(Recording::default());
    assert!(mgr.install_inject_recorder(recording.clone()));
    let record = mgr.get(&record.id).await.expect("record");
    (mgr, fake, record, recording)
}

/// Each typed text is reported once, naming the receiving session; a
/// `NoSubmit` text counts as a delivery too.
#[tokio::test]
async fn an_inject_is_exactly_one_message_event_9167() {
    let dir = TempDir::new().expect("tempdir");
    let (mgr, _fake, record, recording) = active(&dir).await;

    mgr.inject(&record.id, "hello", Submit::Enter)
        .await
        .expect("enter");
    let one = (
        record.tmux_name.clone(),
        Some("claude-1".to_string()),
        "hello".to_string(),
    );
    assert_eq!(recording.seen(), [one.clone()]);

    mgr.inject(&record.id, "draft", Submit::NoSubmit)
        .await
        .expect("nosubmit");
    let two = (record.tmux_name.clone(), Some("claude-1".into()), "draft".into());
    assert_eq!(recording.seen(), [one, two]);
}

/// Ctrl-C carries no text, so it is no message.
#[tokio::test]
async fn an_interrupt_records_no_message_9167() {
    let dir = TempDir::new().expect("tempdir");
    let (mgr, fake, record, recording) = active(&dir).await;

    mgr.inject(&record.id, "", Submit::Interrupt)
        .await
        .expect("interrupt");

    assert_eq!(fake.pane_interrupt_calls.lock().expect("lock").len(), 1);
    assert!(recording.seen().is_empty(), "{:?}", recording.seen());
}

/// A refused inject types nothing and records nothing: a stopped session, a
/// pane whose harness exited, and a pane that is gone.
#[tokio::test]
async fn a_refused_inject_records_no_message_9167() {
    type Refuse = fn(&FakeTmuxDriver);
    let cases: [(&str, Refuse); 2] = [
        ("harness exited", |f| {
            *f.pane_claude_override.lock().expect("lock") = Some(PaneClaude::Absent);
        }),
        ("pane gone", |f| {
            *f.pane_exists_override.lock().expect("lock") = Some(false);
        }),
    ];
    for (why, refuse) in cases {
        let dir = TempDir::new().expect("tempdir");
        let (mgr, fake, record, recording) = active(&dir).await;
        refuse(&fake);
        let refused = mgr.inject(&record.id, "hello", Submit::Enter).await;
        assert!(refused.is_err(), "{why}: {refused:?}");
        assert!(recording.seen().is_empty(), "{why}: {:?}", recording.seen());
    }

    let dir = TempDir::new().expect("tempdir");
    let (mgr, _fake, record, recording) = active(&dir).await;
    mgr.stop(&record.id).await.expect("stop");
    let refused = mgr.inject(&record.id, "hello", Submit::Enter).await;
    assert!(refused.is_err(), "stopped: {refused:?}");
    assert!(recording.seen().is_empty(), "stopped: {:?}", recording.seen());
}

/// The first recorder stays; a second install is refused.
#[tokio::test]
async fn installing_an_inject_recorder_twice_keeps_the_first_9167() {
    let dir = TempDir::new().expect("tempdir");
    let (mgr, _fake, record, first) = active(&dir).await;
    let second = Arc::new(Recording::default());
    assert!(!mgr.install_inject_recorder(second.clone()));

    mgr.inject(&record.id, "hello", Submit::Enter)
        .await
        .expect("enter");

    assert_eq!(first.seen().len(), 1);
    assert!(second.seen().is_empty());
}
