//! Tests for #10442: `session_send` re-resolves a gone recorded pane to the
//! live pane of the record's own tmux session, and refuses everything else.
//!
//! Hermetic: [`ReresolveTmux`] is an in-memory tmux whose panes are a table
//! of `%N` → (server, session, current path). It records every pane send.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tracing_subscriber::layer::SubscriberExt;

use crate::core::process::PaneClaude;
use crate::session_manager::pane_identity::{PaneFacts, PaneIdentity};
use crate::session_manager::record::SessionRecord;
use crate::session_manager::store::SessionStore;
use crate::session_manager::tests::make_active_test_record;
use crate::session_manager::{
    ManagedError, ManagedSessionId, ManagedSessionState, ManagedTmuxDriver, SessionManager,
};

/// The one live tmux server every test runs on.
const SERVER: &str = "4242:1791300000";

/// The `$N` id of the tmux session the record launched.
const LIVE_SESSION: &str = "$7";

/// An in-memory tmux: live session names and a pane table.
#[derive(Default)]
struct ReresolveTmux {
    sessions: Vec<String>,
    /// `%N` → (session holding it, `pane_current_path`).
    panes: HashMap<String, (String, PathBuf)>,
    /// Every `send_line_to_pane` as `"<session> <pane> <text>"`.
    sends: Mutex<Vec<String>>,
    /// The `$N` id of the live session holding every pane.
    session_id: String,
    /// That session's `session_created`, epoch seconds.
    session_created: i64,
}

impl ReresolveTmux {
    fn new(sessions: &[&str], panes: &[(&str, &str, &str)]) -> Self {
        Self {
            sessions: sessions.iter().map(|s| s.to_string()).collect(),
            panes: panes
                .iter()
                .map(|(p, s, cwd)| (p.to_string(), (s.to_string(), PathBuf::from(cwd))))
                .collect(),
            session_id: LIVE_SESSION.into(),
            ..Self::default()
        }
    }

    /// The same tmux, but its live session is `$9`, created at `created`:
    /// the record's session was killed and another took its name.
    fn recreated(mut self, created: i64) -> Self {
        self.session_id = "$9".into();
        self.session_created = created;
        self
    }

    fn sends(&self) -> Vec<String> {
        self.sends.lock().expect("sends").clone()
    }
}

impl ManagedTmuxDriver for ReresolveTmux {
    fn create_session(&self, name: &str, _w: &str) -> Result<(), ManagedError> {
        Err(ManagedError::TmuxUnavailable(format!(
            "fake: no create {name}"
        )))
    }
    fn kill_session(&self, name: &str) -> Result<(), ManagedError> {
        Err(ManagedError::TmuxUnavailable(format!(
            "fake: no kill {name}"
        )))
    }
    fn send_line(&self, name: &str, _t: &str) -> Result<(), ManagedError> {
        Err(ManagedError::TmuxUnavailable(format!(
            "fake: unscoped send {name}"
        )))
    }
    fn send_line_to_pane(&self, name: &str, pane: &str, text: &str) -> Result<(), ManagedError> {
        self.sends
            .lock()
            .expect("sends")
            .push(format!("{name} {pane} {text}"));
        Ok(())
    }
    fn capture(&self, _n: &str, _l: usize) -> Result<String, ManagedError> {
        Ok(String::new())
    }
    fn capture_pane(&self, _n: &str, _p: &str, _l: usize) -> Result<String, ManagedError> {
        Ok("❯ ".into())
    }
    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(self.sessions.clone())
    }
    fn pane_facts(&self, pane_id: &str) -> Result<PaneFacts, ManagedError> {
        let (_, cwd) = self.panes.get(pane_id).ok_or_else(|| {
            ManagedError::TmuxUnavailable(format!("fake: can't find pane {pane_id}"))
        })?;
        Ok(PaneFacts {
            session_id: self.session_id.clone(),
            session_created: self.session_created,
            current_path: cwd.clone(),
        })
    }
    fn pane_exists_checked(&self, name: &str, pane_id: &str) -> Option<bool> {
        Some(self.panes.get(pane_id).is_some_and(|(s, _)| s == name))
    }
    fn session_pane_ids(&self, name: &str) -> Result<Vec<String>, ManagedError> {
        let mut ids: Vec<String> = self
            .panes
            .iter()
            .filter(|(_, (s, _))| s == name)
            .map(|(p, _)| p.clone())
            .collect();
        ids.sort();
        Ok(ids)
    }
    fn pane_identity(&self, pane_id: &str) -> Result<PaneIdentity, ManagedError> {
        let (session, _) = self.panes.get(pane_id).ok_or_else(|| {
            ManagedError::TmuxUnavailable(format!("fake: can't find pane {pane_id}"))
        })?;
        Ok(PaneIdentity {
            pane_id: pane_id.to_owned(),
            session_id: self.session_id.clone(),
            server: SERVER.into(),
            session_name: session.clone(),
        })
    }
    fn pane_harness(&self, _n: &str, _p: &str, _h: &str) -> PaneClaude {
        PaneClaude::Present
    }
}

/// A manager over `tmux` holding one `Active` tm-architect record bound to
/// the dead pane `%17` on [`SERVER`] in session [`LIVE_SESSION`], with `cwd`
/// `/tmp`.
async fn architect(
    tmux: Arc<ReresolveTmux>,
) -> (tempfile::TempDir, SessionManager, ManagedSessionId) {
    architect_in(tmux, Some(LIVE_SESSION)).await
}

/// [`architect`] with the record storing `session` as its tmux session id;
/// `None` is a record written before #10442 stored one.
async fn architect_in(
    tmux: Arc<ReresolveTmux>,
    session: Option<&str>,
) -> (tempfile::TempDir, SessionManager, ManagedSessionId) {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let mgr = SessionManager::new(dir.path(), tmux)
        .await
        .expect("manager");
    let mut record = make_active_test_record("tm-architect", "t", "/tmp");
    record.pane_id = Some("%17".into());
    record.tmux_server = Some(SERVER.into());
    record.tmux_session_id = session.map(str::to_owned);
    let id = record.id;
    mgr.store.write().await.upsert(record).await.expect("seed");
    (dir, mgr, id)
}

/// Send `text` to `id` while capturing every log line; answers the send's
/// result and the captured lines.
async fn send_logged(
    mgr: &SessionManager,
    id: &ManagedSessionId,
) -> (Result<(), ManagedError>, Vec<String>) {
    let buffer = trusty_common::log_buffer::LogBuffer::new(64);
    let subscriber = tracing_subscriber::registry().with(
        trusty_common::log_buffer::LogBufferLayer::new(buffer.clone()),
    );
    let guard = tracing::subscriber::set_default(subscriber);
    let result = mgr.send_input_observed(id, "hello").await.map(drop);
    drop(guard);
    (result, buffer.tail(64))
}

/// Assert the send was refused with today's `PaneGone` for `%17`, that
/// nothing was typed or rebound, and that the refusal logged at ERROR.
async fn assert_refused(
    tmux: &ReresolveTmux,
    mgr: &SessionManager,
    id: &ManagedSessionId,
    result: Result<(), ManagedError>,
    lines: &[String],
) {
    assert!(
        matches!(&result, Err(ManagedError::PaneGone(sid, pid)) if sid == &id.to_string() && pid == "%17"),
        "expected PaneGone for %17, got {result:?}"
    );
    assert!(tmux.sends().is_empty(), "typed into {:?}", tmux.sends());
    let record = mgr.get(id).await.expect("record");
    assert_eq!(record.pane_id.as_deref(), Some("%17"), "record rebound");
    assert_eq!(record.state, ManagedSessionState::Active);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("ERROR") && l.contains("send refused") && l.contains("#10442")),
        "no ERROR line for the refused send: {lines:#?}"
    );
}

/// #10442 acceptance: tm-architect's record names dead `%17`; its tmux
/// session runs the agent in `%44`. The send lands in `%44` and the record
/// now names `%44`, so `session_list` reports the live pane.
///
/// Test: this function IS the test.
#[tokio::test]
async fn a_send_to_a_stale_pane_reresolves_the_live_pane_and_delivers() {
    let tmux = Arc::new(ReresolveTmux::new(
        &["tm-architect"],
        &[("%44", "tm-architect", "/tmp")],
    ));
    let (_dir, mgr, id) = architect(Arc::clone(&tmux)).await;

    let submit = mgr.send_input_observed(&id, "hello").await;

    assert!(submit.is_ok(), "send refused: {submit:?}");
    assert_eq!(tmux.sends(), vec!["tm-architect %44 hello".to_string()]);
    let record = mgr.get(&id).await.expect("record");
    assert_eq!(record.pane_id.as_deref(), Some("%44"));
    assert_eq!(record.tmux_server.as_deref(), Some(SERVER));
    assert_eq!(record.tmux_session_id.as_deref(), Some(LIVE_SESSION));
    assert_eq!(record.state, ManagedSessionState::Active);
}

/// #10442: no tmux session holds the record's name, so there is no live
/// pane; the send keeps today's refusal and logs it at ERROR.
///
/// Test: this function IS the test.
#[tokio::test]
async fn a_send_to_a_dead_session_still_refuses_and_logs_an_error() {
    let tmux = Arc::new(ReresolveTmux::new(&[], &[]));
    let (_dir, mgr, id) = architect(Arc::clone(&tmux)).await;

    let (result, lines) = send_logged(&mgr, &id).await;

    assert_refused(&tmux, &mgr, &id, result, &lines).await;
}

/// #10442: the record's session holds two panes, so tm cannot tell the
/// agent's from a sibling window's; the send refuses and names both.
///
/// Test: this function IS the test.
#[tokio::test]
async fn a_send_to_a_session_with_a_sibling_pane_still_refuses() {
    let tmux = Arc::new(ReresolveTmux::new(
        &["tm-architect"],
        &[
            ("%44", "tm-architect", "/tmp"),
            ("%50", "tm-architect", "/tmp"),
        ],
    ));
    let (_dir, mgr, id) = architect(Arc::clone(&tmux)).await;

    let (result, lines) = send_logged(&mgr, &id).await;

    assert_refused(&tmux, &mgr, &id, result, &lines).await;
    assert!(
        lines.iter().any(|l| l.contains("%44") && l.contains("%50")),
        "the refusal must name the live panes: {lines:#?}"
    );
}

/// #10442: the only live pane belongs to ANOTHER tmux session, and the
/// record's own session holds none; the send never types into it.
///
/// Test: this function IS the test.
#[tokio::test]
async fn a_send_never_reaches_a_pane_of_another_session() {
    let tmux = Arc::new(ReresolveTmux::new(
        &["tm-architect", "tm-other"],
        &[("%44", "tm-other", "/tmp")],
    ));
    let (_dir, mgr, id) = architect(Arc::clone(&tmux)).await;

    let (result, lines) = send_logged(&mgr, &id).await;

    assert_refused(&tmux, &mgr, &id, result, &lines).await;
}

/// #10442: the record's session name now holds one pane in another project
/// (the name was reused); the send refuses rather than bind it.
///
/// Test: this function IS the test.
#[tokio::test]
async fn a_send_whose_session_pane_is_another_projects_still_refuses() {
    let tmux = Arc::new(ReresolveTmux::new(
        &["tm-architect"],
        &[("%44", "tm-architect", "/elsewhere/other-project")],
    ));
    let (_dir, mgr, id) = architect(Arc::clone(&tmux)).await;

    let (result, lines) = send_logged(&mgr, &id).await;

    assert_refused(&tmux, &mgr, &id, result, &lines).await;
}

/// #10442 critic MEDIUM: the record's tmux session was killed and an
/// unmanaged one took its name, with one pane in the project. A record that
/// stored its `$N` id, and a legacy record older than the new session, both
/// refuse; nothing is typed and the record is unchanged.
///
/// Test: this function IS the test.
#[tokio::test]
async fn a_send_into_a_session_recreated_under_the_same_name_refuses() {
    let later = chrono::Utc::now().timestamp() + 60;
    for stored in [Some(LIVE_SESSION), None] {
        let tmux = Arc::new(
            ReresolveTmux::new(&["tm-architect"], &[("%44", "tm-architect", "/tmp")])
                .recreated(later),
        );
        let (_dir, mgr, id) = architect_in(Arc::clone(&tmux), stored).await;
        let before = mgr.get(&id).await.expect("record");

        let (result, lines) = send_logged(&mgr, &id).await;

        assert_refused(&tmux, &mgr, &id, result, &lines).await;
        let after = mgr.get(&id).await.expect("record");
        assert_eq!(
            serde_json::to_value(&after).expect("after"),
            serde_json::to_value(&before).expect("before"),
            "stored {stored:?}: the record changed"
        );
    }
}

/// #10442: a legacy record (no stored session id) re-resolves into a session
/// created no later than the record, and then stores that session's id.
///
/// Test: this function IS the test.
#[tokio::test]
async fn a_legacy_record_reresolves_into_a_session_older_than_itself() {
    let tmux = Arc::new(ReresolveTmux::new(
        &["tm-architect"],
        &[("%44", "tm-architect", "/tmp")],
    ));
    let (_dir, mgr, id) = architect_in(Arc::clone(&tmux), None).await;

    let submit = mgr.send_input_observed(&id, "hello").await;

    assert!(submit.is_ok(), "send refused: {submit:?}");
    assert_eq!(tmux.sends(), vec!["tm-architect %44 hello".to_string()]);
    let record = mgr.get(&id).await.expect("record");
    assert_eq!(record.tmux_session_id.as_deref(), Some(LIVE_SESSION));
}

/// #10442: `tmux_session_id` is additive. An old `sessions.json` loads with
/// `None` and re-saves without the key; a stored id survives a round trip.
///
/// Test: this function IS the test.
#[tokio::test]
async fn tmux_session_id_round_trips_and_old_records_load() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("sessions.json");
    std::fs::write(&path, include_str!("testdata/session-record-pre-d07.json")).expect("seed");
    let mut store = SessionStore::load(dir.path())
        .await
        .expect("old store loads");
    let records = store.all().await.expect("records");
    assert!(records.iter().all(|r| r.tmux_session_id.is_none()));
    store.save().await.expect("re-save");
    let written = std::fs::read_to_string(&path).expect("read back");
    assert!(!written.contains("tmux_session_id"), "{written}");

    let mut record = records.into_iter().next().expect("one record");
    record.tmux_session_id = Some(LIVE_SESSION.into());
    let json = serde_json::to_string(&record).expect("serializes");
    let back: SessionRecord = serde_json::from_str(&json).expect("reloads");
    assert_eq!(back.tmux_session_id.as_deref(), Some(LIVE_SESSION));
}
