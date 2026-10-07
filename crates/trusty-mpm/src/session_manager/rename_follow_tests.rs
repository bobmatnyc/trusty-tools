//! Tests for #9238: a record follows its own pane through a tmux rename.
//!
//! Hermetic: every manager runs [`PaneTmux`], an in-memory driver whose panes
//! are a table of `%N` → (server, session). No tmux binary runs, and the
//! fake fails any rename or kill it is asked for.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::pane_identity::PaneIdentity;
use super::tests::make_active_test_record;
use crate::core::doctor::CheckStatus;
use crate::daemon::doctor::doctor_session_names::check_session_names;
use crate::daemon::managed_routes::cores::{attach_cmd_core, list_core};
use crate::daemon::rpc::managed::outcome::RouteBody;
use crate::daemon::state::DaemonState;
use crate::session_manager::{
    ManagedError, ManagedSessionId, ManagedSessionState, ManagedTmuxDriver, SessionManager,
};

/// The server instance every record below captured its pane on.
const SERVER: &str = "4242:1700000000";

/// A restarted server: same socket, another pid and start time.
const OTHER_SERVER: &str = "5151:1800000000";

/// An in-memory tmux: live session names and a pane table.
#[derive(Default)]
struct PaneTmux {
    sessions: Vec<String>,
    /// `%N` → (server, session holding it).
    panes: HashMap<String, (String, String)>,
    list_fails: bool,
    /// Every kill or rename asked for. The fix must never ask.
    mutations: Mutex<Vec<String>>,
}

impl PaneTmux {
    fn new(sessions: &[&str], panes: &[(&str, &str, &str)]) -> Arc<Self> {
        Arc::new(Self {
            sessions: sessions.iter().map(|s| s.to_string()).collect(),
            panes: panes
                .iter()
                .map(|(p, srv, s)| (p.to_string(), (srv.to_string(), s.to_string())))
                .collect(),
            ..Self::default()
        })
    }
}

impl ManagedTmuxDriver for PaneTmux {
    fn create_session(&self, name: &str, _w: &str) -> Result<(), ManagedError> {
        self.mutations
            .lock()
            .unwrap()
            .push(format!("create {name}"));
        Ok(())
    }
    fn kill_session(&self, name: &str) -> Result<(), ManagedError> {
        self.mutations.lock().unwrap().push(format!("kill {name}"));
        Ok(())
    }
    fn kill_session_id(&self, name: &str, id: &str) -> Result<(), ManagedError> {
        self.mutations
            .lock()
            .unwrap()
            .push(format!("kill {id} {name}"));
        Ok(())
    }
    fn send_line(&self, _n: &str, _t: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn capture(&self, _n: &str, _l: usize) -> Result<String, ManagedError> {
        Ok(String::new())
    }
    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        if self.list_fails {
            return Err(ManagedError::TmuxUnavailable("fake: no server".into()));
        }
        Ok(self.sessions.clone())
    }
    fn pane_exists_checked(&self, name: &str, pane_id: &str) -> Option<bool> {
        Some(self.panes.get(pane_id).is_some_and(|(_, s)| s == name))
    }
    fn pane_identity(&self, pane_id: &str) -> Result<PaneIdentity, ManagedError> {
        let (server, session) = self.panes.get(pane_id).ok_or_else(|| {
            ManagedError::TmuxUnavailable(format!("fake: can't find pane {pane_id}"))
        })?;
        Ok(PaneIdentity {
            pane_id: pane_id.to_owned(),
            session_id: "$7".into(),
            server: server.clone(),
            session_name: session.clone(),
        })
    }
}

/// Seed a record named `name` in `state`, bound to `pane` on `server`.
async fn seed(
    mgr: &SessionManager,
    name: &str,
    state: ManagedSessionState,
    pane: &str,
    server: Option<&str>,
) -> ManagedSessionId {
    let mut record = make_active_test_record(name, "t", "/tmp");
    record.state = state;
    record.pane_id = Some(pane.into());
    record.tmux_server = server.map(str::to_owned);
    let id = record.id;
    mgr.store.write().await.upsert(record).await.expect("seed");
    id
}

async fn manager(tmux: Arc<PaneTmux>) -> (tempfile::TempDir, Arc<SessionManager>) {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let mgr = SessionManager::new(dir.path(), tmux)
        .await
        .expect("manager");
    (dir, Arc::new(mgr))
}

/// The `tm ls` row for `id`: its `(name, state)`.
async fn listed(state: &Arc<DaemonState>, id: &ManagedSessionId) -> (String, String) {
    let outcome = list_core(state, None, true).await;
    let RouteBody::Json(body) = outcome.body else {
        panic!("list answered {}: {:?}", outcome.status, outcome.body);
    };
    let row = body["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .find(|r| r["id"] == id.to_string())
        .unwrap_or_else(|| panic!("no row for {id} in {body}"))
        .clone();
    (
        row["name"].as_str().expect("name").to_owned(),
        row["state"].as_str().expect("state").to_owned(),
    )
}

/// #9238 criteria 1 and 2: `tm sessions new` named the record
/// `tm-localizer-2`, then `tmux rename-session` made it `tm-localizer`. The
/// pane is the record's own, on its own server, so `tm ls` lists it live
/// under the current name, and the store carries that name.
///
/// Test: this function IS the test.
#[tokio::test]
async fn a_renamed_session_is_followed_by_its_pane_and_listed_live() {
    let tmux = PaneTmux::new(&["tm-localizer"], &[("%2157", SERVER, "tm-localizer")]);
    let (_dir, mgr) = manager(Arc::clone(&tmux)).await;
    let id = seed(
        &mgr,
        "tm-localizer-2",
        ManagedSessionState::Active,
        "%2157",
        Some(SERVER),
    )
    .await;
    let state = Arc::new(DaemonState::with_session_manager(Arc::clone(&mgr)));

    assert_eq!(
        listed(&state, &id).await,
        ("tm-localizer".to_string(), "active".to_string())
    );
    let record = mgr.get(&id).await.expect("record");
    assert_eq!(
        record.tmux_name, "tm-localizer",
        "the store follows the rename"
    );
    assert_eq!(
        record.state,
        ManagedSessionState::Active,
        "state is untouched"
    );
    assert!(
        tmux.mutations.lock().unwrap().is_empty(),
        "tmux was only read"
    );
}

/// #9238 criterion 2: `attach_cmd` targets the session tmux uses now, not the
/// recorded name that no longer exists.
///
/// Test: this function IS the test.
#[tokio::test]
async fn attach_cmd_targets_the_current_tmux_name() {
    let tmux = PaneTmux::new(&["tm-i8n"], &[("%31", SERVER, "tm-i8n")]);
    let (_dir, mgr) = manager(tmux).await;
    let id = seed(
        &mgr,
        "tm-i8n-01",
        ManagedSessionState::Active,
        "%31",
        Some(SERVER),
    )
    .await;
    let state = Arc::new(DaemonState::with_session_manager(mgr));

    let outcome = attach_cmd_core(&state, &id.to_string()).await;
    let RouteBody::Json(body) = outcome.body else {
        panic!("attach-cmd answered {}: {:?}", outcome.status, outcome.body);
    };
    assert_eq!(
        body["attach_cmd"].as_str(),
        Some(crate::core::tmux::shell_attach_command("tm-i8n").as_str())
    );
}

/// #9238 criterion 3: a pane tmux cannot find, and a record with no server
/// identity to prove its pane, are never read as live, so each row stays
/// stopped under its recorded name.
///
/// Test: this function IS the test.
#[tokio::test]
async fn a_dead_pane_is_never_followed_and_stays_stopped() {
    // %40 is gone; %41 is live in `tm-legacy`, but its record has no server.
    let tmux = PaneTmux::new(&["tm-legacy"], &[("%41", SERVER, "tm-legacy")]);
    let (_dir, mgr) = manager(tmux).await;
    let dead = seed(
        &mgr,
        "tm-gone-01",
        ManagedSessionState::Stopped,
        "%40",
        Some(SERVER),
    )
    .await;
    let legacy = seed(
        &mgr,
        "tm-legacy-01",
        ManagedSessionState::Stopped,
        "%41",
        None,
    )
    .await;
    let state = Arc::new(DaemonState::with_session_manager(Arc::clone(&mgr)));

    assert_eq!(
        listed(&state, &dead).await,
        ("tm-gone-01".to_string(), "stopped".to_string())
    );
    assert_eq!(
        listed(&state, &legacy).await,
        ("tm-legacy-01".to_string(), "stopped".to_string())
    );
    assert!(mgr.follow_tmux_renames().await.is_empty());
}

/// #9238 criterion 3: a restarted tmux server reuses `%N` ids. A pane with the
/// record's id on ANOTHER server is not the record's pane and never counts as
/// live, even in a live session.
///
/// Test: this function IS the test.
#[tokio::test]
async fn a_pane_id_on_another_tmux_server_is_never_followed() {
    let tmux = PaneTmux::new(&["tm-reused"], &[("%2", OTHER_SERVER, "tm-reused")]);
    let (_dir, mgr) = manager(tmux).await;
    let id = seed(
        &mgr,
        "tm-stale-01",
        ManagedSessionState::Active,
        "%2",
        Some(SERVER),
    )
    .await;
    let state = Arc::new(DaemonState::with_session_manager(Arc::clone(&mgr)));

    assert_eq!(
        listed(&state, &id).await,
        ("tm-stale-01".to_string(), "stopped".to_string())
    );
    assert_eq!(mgr.get(&id).await.expect("record").tmux_name, "tm-stale-01");
}

/// #9238: the 60-second reaper stopped a renamed session because its recorded
/// name was gone. It now follows the pane first and leaves it Active.
///
/// Test: this function IS the test.
#[tokio::test]
async fn the_reaper_follows_a_rename_instead_of_stopping_the_session() {
    let tmux = PaneTmux::new(&["tm-architect"], &[("%5", SERVER, "tm-architect")]);
    let (_dir, mgr) = manager(Arc::clone(&tmux)).await;
    let id = seed(
        &mgr,
        "tm-supervisor",
        ManagedSessionState::Active,
        "%5",
        Some(SERVER),
    )
    .await;
    let state = DaemonState::with_session_manager(Arc::clone(&mgr));

    let live = ["tm-architect".to_string()].into_iter().collect();
    state.reap_managed_against(&live).await;

    let record = mgr.get(&id).await.expect("record");
    assert_eq!(record.state, ManagedSessionState::Active);
    assert_eq!(record.tmux_name, "tm-architect");
    assert!(
        tmux.mutations.lock().unwrap().is_empty(),
        "nothing was killed"
    );
}

/// #9238 cause 2: boot reconcile marked a renamed session Stopped, and its
/// dedup then decommissioned it as a stale duplicate. It now follows the
/// pane before either decision, so the record stays Active.
///
/// Test: this function IS the test.
#[tokio::test]
async fn boot_reconcile_keeps_a_renamed_session_active() {
    let tmux = PaneTmux::new(&["tm-memory"], &[("%8", SERVER, "tm-memory")]);
    let (_dir, mgr) = manager(tmux).await;
    let id = seed(
        &mgr,
        "tm-memory-01",
        ManagedSessionState::Active,
        "%8",
        Some(SERVER),
    )
    .await;

    mgr.reconcile_on_boot(false).await.expect("reconcile");

    let record = mgr.get(&id).await.expect("record");
    assert_eq!(record.tmux_name, "tm-memory");
    assert_eq!(record.state, ManagedSessionState::Active);
}

/// #9238: when another live record already holds the pane's session name,
/// the record is left as it was; two records never share a name (#3692).
///
/// Test: this function IS the test.
#[tokio::test]
async fn a_name_another_record_holds_is_not_taken() {
    let tmux = PaneTmux::new(&["tm-a"], &[("%1", SERVER, "tm-a")]);
    let (_dir, mgr) = manager(tmux).await;
    let moved = seed(
        &mgr,
        "tm-a-02",
        ManagedSessionState::Active,
        "%1",
        Some(SERVER),
    )
    .await;
    seed(
        &mgr,
        "tm-a",
        ManagedSessionState::Stopped,
        "%9",
        Some(SERVER),
    )
    .await;

    assert!(mgr.follow_tmux_renames().await.is_empty());
    assert_eq!(mgr.get(&moved).await.expect("record").tmux_name, "tm-a-02");
}

/// #9238 criterion 5: `tm doctor` names a record whose pane is live under
/// another name, including one decommissioned while its pane runs (cause 2),
/// and points that one at the reactivate route. A clean fleet is `Ok`; an
/// unlistable tmux is `Unknown`, never `Ok`.
///
/// Test: this function IS the test.
#[tokio::test]
async fn the_doctor_row_flags_a_live_pane_under_another_name() {
    let tmux = PaneTmux::new(
        &["tm-i8n", "tm-search", "tm-ok-01"],
        &[
            ("%3", SERVER, "tm-i8n"),
            ("%4", SERVER, "tm-search"),
            ("%6", SERVER, "tm-ok-01"),
        ],
    );
    let (_dir, mgr) = manager(tmux).await;
    let renamed = seed(
        &mgr,
        "tm-i8n-01",
        ManagedSessionState::Active,
        "%3",
        Some(SERVER),
    )
    .await;
    let tomb = seed(
        &mgr,
        "tm-search-01",
        ManagedSessionState::Decommissioned,
        "%4",
        Some(SERVER),
    )
    .await;
    seed(
        &mgr,
        "tm-ok-01",
        ManagedSessionState::Active,
        "%6",
        Some(SERVER),
    )
    .await;

    let row = check_session_names(&mgr).await;
    assert_eq!(row.name, "session_names");
    assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
    assert!(row.message.starts_with("2 record(s)"), "{}", row.message);
    assert!(
        row.message.contains(&format!(
            "record {renamed} names tmux session 'tm-i8n-01', but its pane %3 is live in 'tm-i8n'"
        )),
        "{}",
        row.message
    );
    assert!(
        row.message
            .contains(&format!("/api/v1/sessions/managed/{tomb}/reactivate")),
        "{}",
        row.message
    );
    assert!(!row.message.contains("tm-ok-01"), "{}", row.message);

    let clean = PaneTmux::new(&["tm-ok-01"], &[("%6", SERVER, "tm-ok-01")]);
    let (_d, clean_mgr) = manager(clean).await;
    seed(
        &clean_mgr,
        "tm-ok-01",
        ManagedSessionState::Active,
        "%6",
        Some(SERVER),
    )
    .await;
    assert_eq!(
        check_session_names(&clean_mgr).await.status,
        CheckStatus::Ok
    );

    let unlistable = Arc::new(PaneTmux {
        list_fails: true,
        ..PaneTmux::default()
    });
    let (_d2, blind_mgr) = manager(unlistable).await;
    assert_eq!(
        check_session_names(&blind_mgr).await.status,
        CheckStatus::Unknown
    );
}
