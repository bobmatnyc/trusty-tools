//! Tests for #9238: a record follows its own pane through a tmux rename.
//!
//! Hermetic: every manager runs [`PaneTmux`], an in-memory driver whose panes
//! are a table of `%N` → (server, session). No tmux binary runs, and the
//! fake fails any rename or kill it is asked for.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

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
    /// #9025: `pane_claude` answers `Unknown` (else `Absent`: no `claude`, so
    /// the doctor row never reads the real registry).
    claude_unreadable: bool,
    /// Every kill or rename asked for. The fix must never ask.
    mutations: Mutex<Vec<String>>,
    /// #10101: the named (`-L`) servers this driver reaches, by name.
    servers: HashMap<String, Arc<PaneTmux>>,
    /// #10101: how many times `list_sessions` was asked.
    lists: AtomicUsize,
    /// #10101: `list_sessions` blocks until the sender is dropped (a hung
    /// server).
    hang: Option<Mutex<mpsc::Receiver<()>>>,
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
        self.lists.fetch_add(1, Ordering::SeqCst);
        if let Some(hang) = &self.hang {
            let _ = hang.lock().unwrap().recv();
        }
        if self.list_fails {
            return Err(ManagedError::TmuxUnavailable("fake: no server".into()));
        }
        Ok(self.sessions.clone())
    }
    fn pane_claude(&self, _name: &str) -> crate::core::process::PaneClaude {
        if self.claude_unreadable {
            crate::core::process::PaneClaude::Unknown
        } else {
            crate::core::process::PaneClaude::Absent
        }
    }
    fn pane_exists_checked(&self, name: &str, pane_id: &str) -> Option<bool> {
        Some(self.panes.get(pane_id).is_some_and(|(_, s)| s == name))
    }
    fn on_server(&self, server: &str) -> Option<Arc<dyn ManagedTmuxDriver>> {
        self.servers
            .get(server)
            .map(|s| Arc::clone(s) as Arc<dyn ManagedTmuxDriver>)
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

    let live: std::collections::HashSet<String> =
        ["tm-architect".to_string()].into_iter().collect();
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

/// #9025 rename case: a tmux session renamed after launch leaves its `claude`
/// under the old remote name, and the doctor row warns with the `/rename`
/// that fixes it. A registry that could not be read is said, not hidden.
#[test]
fn the_doctor_row_flags_a_remote_name_that_left_its_tmux_session() {
    use crate::daemon::doctor::doctor_session_names::session_names_row;
    use crate::runtime::remote_name::RemoteNameDrift;
    let drift = RemoteNameDrift {
        session: "tm-dogfood".to_owned(),
        pid: 4242,
        remote: "trusty-tools-a7".to_owned(),
    };
    let row = session_names_row(Ok(Vec::new()), Ok(vec![drift]), &[]);
    assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
    assert!(row.message.starts_with("1 session(s)"), "{}", row.message);
    assert!(
        row.message.contains(
            "tmux session 'tm-dogfood' runs claude pid 4242 under the remote name \
             'trusty-tools-a7'; type `/rename tm-dogfood` in that session"
        ),
        "{}",
        row.message
    );

    // #9025 round 1: a check that did not run is `Unknown`, never `Ok`.
    let row = session_names_row(Ok(Vec::new()), Err("timed out".to_owned()), &[]);
    assert_eq!(row.status, CheckStatus::Unknown, "{}", row.message);
    assert!(
        row.message
            .ends_with("; remote names were not checked (timed out)"),
        "{}",
        row.message
    );
}

/// #9025 round 1: a pane the `claude` probe could not read is named in the row
/// as unchecked (`Unknown`), not skipped into an `Ok`; a drift elsewhere stays
/// a `Warn` that still carries the note. Through the real `check_session_names`
/// too, so the probe's collection is covered, not only the row text.
#[tokio::test]
async fn a_pane_the_probe_could_not_read_is_reported_unchecked() {
    use crate::daemon::doctor::doctor_session_names::session_names_row;
    let unchecked = ["tm-blind".to_owned()];
    let row = session_names_row(Ok(Vec::new()), Ok(Vec::new()), &unchecked);
    assert_eq!(row.status, CheckStatus::Unknown, "{}", row.message);
    assert!(
        row.message
            .contains("1 session(s) could not be probed for a claude")
            && row.message.contains("tm-blind"),
        "{}",
        row.message
    );

    let tmux = Arc::new(PaneTmux {
        sessions: vec!["tm-blind".to_owned()],
        claude_unreadable: true,
        ..PaneTmux::default()
    });
    let (_d, mgr) = manager(tmux).await;
    seed(&mgr, "tm-blind", ManagedSessionState::Active, "%9", None).await;
    let row = check_session_names(&mgr).await;
    assert_eq!(row.status, CheckStatus::Unknown, "{}", row.message);
    assert!(row.message.contains("tm-blind"), "{}", row.message);
}

/// The server instance a workspace server's panes were captured on.
const WS_SERVER: &str = "6161:1750000000";

/// A fake with `sessions` and `panes`, reaching `servers` by name (#10101).
fn host(
    sessions: &[&str],
    panes: &[(&str, &str, &str)],
    servers: Vec<(&str, Arc<PaneTmux>)>,
) -> Arc<PaneTmux> {
    let base = PaneTmux::new(sessions, panes);
    Arc::new(PaneTmux {
        sessions: base.sessions.clone(),
        panes: base.panes.clone(),
        servers: servers
            .into_iter()
            .map(|(n, s)| (n.to_owned(), s))
            .collect(),
        ..PaneTmux::default()
    })
}

/// [`seed`] for a record on the workspace server whose socket is `socket`.
async fn seed_on(mgr: &SessionManager, name: &str, pane: &str, socket: &Path) -> ManagedSessionId {
    let mut record = make_active_test_record(name, "t", "/tmp");
    record.pane_id = Some(pane.into());
    record.tmux_server = Some(WS_SERVER.into());
    record.tmux_socket = Some(socket.to_path_buf());
    let id = record.id;
    mgr.store.write().await.upsert(record).await.expect("seed");
    id
}

/// A socket path for server `server` in `dir`, unique per test so the
/// process-wide in-flight probe set never joins two tests.
fn socket(dir: &tempfile::TempDir, server: &str) -> PathBuf {
    dir.path().join(server)
}

/// #10101 criteria 1, 2 and 3: a PM on a workspace server renamed in tmux is
/// followed by its pane on that server; the default server's own record, whose
/// pane carries the same `%3` id, is followed on the default server. Each
/// server is listed once for two records on it.
///
/// Test: this function IS the test.
#[tokio::test]
async fn a_renamed_workspace_server_record_is_followed_on_its_own_server() {
    let ws = PaneTmux::new(
        &["tm-pm", "tm-peer"],
        &[("%3", WS_SERVER, "tm-pm"), ("%4", WS_SERVER, "tm-peer")],
    );
    let tmux = host(
        &["tm-other"],
        &[("%3", SERVER, "tm-other")],
        vec![("tm-ws-a", Arc::clone(&ws))],
    );
    let (dir, mgr) = manager(Arc::clone(&tmux)).await;
    let sock = socket(&dir, "tm-ws-a");
    let on_ws = seed_on(&mgr, "tm-pm-01", "%3", &sock).await;
    let peer = seed_on(&mgr, "tm-peer", "%4", &sock).await;
    let on_default = seed(
        &mgr,
        "tm-def-01",
        ManagedSessionState::Active,
        "%3",
        Some(SERVER),
    )
    .await;

    let followed = mgr.follow_tmux_renames().await;

    assert_eq!(followed.len(), 2, "{followed:?}");
    assert_eq!(mgr.get(&on_ws).await.expect("ws").tmux_name, "tm-pm");
    assert_eq!(
        mgr.get(&on_default).await.expect("default").tmux_name,
        "tm-other"
    );
    assert_eq!(mgr.get(&peer).await.expect("peer").tmux_name, "tm-peer");
    assert_eq!(ws.lists.load(Ordering::SeqCst), 1, "one listing per server");
    assert!(ws.mutations.lock().unwrap().is_empty());
    assert!(tmux.mutations.lock().unwrap().is_empty());
}

/// #10101 criterion 3: another server listing a session under the record's
/// old name does not read as the record being live; only its own server's
/// listing counts.
///
/// Test: this function IS the test.
#[tokio::test]
async fn a_same_named_session_on_another_server_does_not_hide_a_rename() {
    let ws_a = PaneTmux::new(&["tm-y"], &[("%3", WS_SERVER, "tm-y")]);
    let ws_b = PaneTmux::new(&["tm-x"], &[("%3", OTHER_SERVER, "tm-x")]);
    let tmux = host(
        &["tm-x"],
        &[("%3", SERVER, "tm-x")],
        vec![("tm-ws-a", ws_a), ("tm-ws-b", Arc::clone(&ws_b))],
    );
    let (dir, mgr) = manager(tmux).await;
    let id = seed_on(&mgr, "tm-x", "%3", &socket(&dir, "tm-ws-a")).await;

    mgr.follow_tmux_renames().await;

    assert_eq!(mgr.get(&id).await.expect("record").tmux_name, "tm-y");
    assert_eq!(ws_b.lists.load(Ordering::SeqCst), 0, "no record on ws-b");
}

/// #10101 criterion 2: the reaper follows a workspace-server rename before
/// its name check, so the PM stays Active under its current name.
///
/// Test: this function IS the test.
#[tokio::test]
async fn the_reaper_follows_a_workspace_server_rename_instead_of_stopping_it() {
    let ws = PaneTmux::new(&["tm-arch"], &[("%5", WS_SERVER, "tm-arch")]);
    let tmux = host(&["tm-unrelated"], &[], vec![("tm-ws-a", Arc::clone(&ws))]);
    let (dir, mgr) = manager(tmux).await;
    let id = seed_on(&mgr, "tm-sup", "%5", &socket(&dir, "tm-ws-a")).await;
    let state = DaemonState::with_session_manager(Arc::clone(&mgr));
    let live = crate::session_manager::tmux_route::LiveSessions {
        default: HashSet::from(["tm-unrelated".to_owned()]),
        named: HashMap::from([("tm-ws-a".to_owned(), HashSet::from(["tm-arch".to_owned()]))]),
        ..Default::default()
    };

    state.reap_managed_against(&live).await;

    let record = mgr.get(&id).await.expect("record");
    assert_eq!(record.state, ManagedSessionState::Active);
    assert_eq!(record.tmux_name, "tm-arch");
    assert!(
        ws.mutations.lock().unwrap().is_empty(),
        "nothing was killed"
    );
}

/// #10101 criterion 2 (Fail-Open Check): a hung workspace server bounds the
/// pass, holds one listing across passes, and leaves its records as they
/// are — not renamed, not stopped, not dropped.
///
/// Test: this function IS the test.
#[tokio::test]
async fn a_hung_workspace_server_is_skipped_and_its_records_kept() {
    let (release, rx) = mpsc::channel::<()>();
    let ws = Arc::new(PaneTmux {
        sessions: vec!["tm-new".to_owned()],
        panes: HashMap::from([("%3".to_owned(), (WS_SERVER.to_owned(), "tm-new".to_owned()))]),
        hang: Some(Mutex::new(rx)),
        ..PaneTmux::default()
    });
    let tmux = host(
        &["tm-new"],
        &[("%3", WS_SERVER, "tm-new")],
        vec![("tm-ws-hung", Arc::clone(&ws))],
    );
    let (dir, mgr) = manager(tmux).await;
    let id = seed_on(&mgr, "tm-old", "%3", &socket(&dir, "tm-ws-hung")).await;

    let started = Instant::now();
    assert!(mgr.follow_tmux_renames().await.is_empty());
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "pass not bounded"
    );
    let again = Instant::now();
    assert!(mgr.follow_tmux_renames().await.is_empty());
    assert!(
        again.elapsed() < Duration::from_secs(1),
        "in-flight server re-probed"
    );
    assert_eq!(ws.lists.load(Ordering::SeqCst), 1, "one blocked listing");

    let state = DaemonState::with_session_manager(Arc::clone(&mgr));
    let live = crate::session_manager::tmux_route::LiveSessions {
        default: HashSet::from(["tm-new".to_owned()]),
        unknown: HashSet::from(["tm-ws-hung".to_owned()]),
        ..Default::default()
    };
    state.reap_managed_against(&live).await;

    let record = mgr.get(&id).await.expect("record kept");
    assert_eq!(record.tmux_name, "tm-old");
    assert_eq!(record.state, ManagedSessionState::Active);
    drop(release);
}

/// #10101 criteria 2 and 4 (Fail-Open Check): a workspace server the driver
/// cannot reach, and one whose listing fails, skip their records; neither is
/// looked up on the default server, whose `%3` sits in a live session.
///
/// Test: this function IS the test.
#[tokio::test]
async fn an_unreachable_or_failing_workspace_server_is_skipped_never_defaulted() {
    let failing = Arc::new(PaneTmux {
        sessions: vec!["tm-new".to_owned()],
        panes: HashMap::from([("%3".to_owned(), (WS_SERVER.to_owned(), "tm-new".to_owned()))]),
        list_fails: true,
        ..PaneTmux::default()
    });
    let tmux = host(
        &["tm-wrong"],
        &[("%3", WS_SERVER, "tm-wrong")],
        vec![("tm-ws-fail", failing)],
    );
    let (dir, mgr) = manager(Arc::clone(&tmux)).await;
    let gone = seed_on(&mgr, "tm-a-01", "%3", &socket(&dir, "tm-ws-gone")).await;
    let fail = seed_on(&mgr, "tm-b-01", "%3", &socket(&dir, "tm-ws-fail")).await;

    assert_eq!(mgr.name_drifts().await.expect("default listed"), Vec::new());
    assert!(mgr.follow_tmux_renames().await.is_empty());

    for (id, name) in [(gone, "tm-a-01"), (fail, "tm-b-01")] {
        let record = mgr.get(&id).await.expect("record kept");
        assert_eq!(record.tmux_name, name);
        assert_eq!(record.state, ManagedSessionState::Active);
    }
    assert!(tmux.mutations.lock().unwrap().is_empty());
}
