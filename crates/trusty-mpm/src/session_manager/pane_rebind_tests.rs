//! Tests for #9313: re-bind a record to its live pane after a tmux server
//! replacement, only on exactly one match, and never touch the session.
//!
//! Hermetic: every manager runs [`RebindTmux`], an in-memory driver whose
//! panes are a table of `%N` → (server, session). It records every call that
//! could create, kill, rename, type into or signal a session, and the tests
//! assert that list stays empty.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::daemon::managed_routes::cores::list_core;
use crate::daemon::managed_routes::rebind::{rebind_all_core, rebind_core};
use crate::daemon::rpc::managed::outcome::RouteBody;
use crate::daemon::state::DaemonState;
use crate::session_manager::pane_identity::PaneIdentity;
use crate::session_manager::pane_rebind::RebindOutcome;
use crate::session_manager::tests::make_active_test_record;
use crate::session_manager::{
    ManagedError, ManagedSessionId, ManagedSessionState, ManagedTmuxDriver, SessionManager,
};

/// The server every record below captured its pane on.
const OLD: &str = "99330:1791270266";

/// The replacement server the live sessions run on now.
const NEW: &str = "66166:1791300000";

/// An in-memory tmux: live session names and a pane table.
#[derive(Default)]
struct RebindTmux {
    sessions: Vec<String>,
    /// `%N` → (server, session holding it).
    panes: HashMap<String, (String, String)>,
    /// Sessions whose `list-panes` fails.
    pane_list_fails: HashSet<String>,
    /// Every pane-identity read fails.
    identity_fails: bool,
    /// Every call that could launch, kill, rename, type into or signal.
    mutations: Mutex<Vec<String>>,
}

impl RebindTmux {
    fn new(sessions: &[&str], panes: &[(&str, &str, &str)]) -> Self {
        Self {
            sessions: sessions.iter().map(|s| s.to_string()).collect(),
            panes: panes
                .iter()
                .map(|(p, srv, s)| (p.to_string(), (srv.to_string(), s.to_string())))
                .collect(),
            ..Self::default()
        }
    }

    fn touched(&self, call: String) -> Result<(), ManagedError> {
        self.mutations.lock().unwrap().push(call);
        Ok(())
    }

    fn mutations(&self) -> Vec<String> {
        self.mutations.lock().unwrap().clone()
    }
}

impl ManagedTmuxDriver for RebindTmux {
    fn create_session(&self, name: &str, _w: &str) -> Result<(), ManagedError> {
        self.touched(format!("create {name}"))
    }
    fn create_session_exclusive(&self, name: &str, _w: &str) -> Result<(), ManagedError> {
        self.touched(format!("create-exclusive {name}"))
    }
    fn kill_session(&self, name: &str) -> Result<(), ManagedError> {
        self.touched(format!("kill {name}"))
    }
    fn kill_session_id(&self, name: &str, id: &str) -> Result<(), ManagedError> {
        self.touched(format!("kill {id} {name}"))
    }
    fn rename_session(&self, old: &str, new: &str) -> Result<(), ManagedError> {
        self.touched(format!("rename {old} {new}"))
    }
    fn rename_session_id(&self, name: &str, id: &str, new: &str) -> Result<(), ManagedError> {
        self.touched(format!("rename {id} {name} {new}"))
    }
    fn send_line(&self, name: &str, _t: &str) -> Result<(), ManagedError> {
        self.touched(format!("send {name}"))
    }
    fn send_line_to_pane(&self, name: &str, pane: &str, _t: &str) -> Result<(), ManagedError> {
        self.touched(format!("send {name} {pane}"))
    }
    fn send_interrupt(&self, name: &str) -> Result<(), ManagedError> {
        self.touched(format!("interrupt {name}"))
    }
    fn graceful_stop(&self, name: &str, _pid: Option<u32>) -> Result<(), ManagedError> {
        self.touched(format!("graceful-stop {name}"))
    }
    fn signal_terminate(&self, name: &str, _pid: Option<u32>) {
        let _ = self.touched(format!("terminate {name}"));
    }
    fn signal_terminate_pane(&self, name: &str, pane: &str, _pid: Option<u32>) {
        let _ = self.touched(format!("terminate {name} {pane}"));
    }
    fn capture(&self, _n: &str, _l: usize) -> Result<String, ManagedError> {
        Ok(String::new())
    }
    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(self.sessions.clone())
    }
    fn pane_exists_checked(&self, name: &str, pane_id: &str) -> Option<bool> {
        Some(self.panes.get(pane_id).is_some_and(|(_, s)| s == name))
    }
    fn session_pane_ids(&self, name: &str) -> Result<Vec<String>, ManagedError> {
        if self.pane_list_fails.contains(name) {
            return Err(ManagedError::TmuxUnavailable(format!(
                "fake: list-panes failed for {name}"
            )));
        }
        let mut ids: Vec<String> = self
            .panes
            .iter()
            .filter(|(_, (_, s))| s == name)
            .map(|(p, _)| p.clone())
            .collect();
        ids.sort();
        Ok(ids)
    }
    fn pane_identity(&self, pane_id: &str) -> Result<PaneIdentity, ManagedError> {
        if self.identity_fails {
            return Err(ManagedError::TmuxUnavailable("fake: no server".into()));
        }
        let (server, session) = self.panes.get(pane_id).ok_or_else(|| {
            ManagedError::TmuxUnavailable(format!("fake: can't find pane {pane_id}"))
        })?;
        Ok(PaneIdentity {
            pane_id: pane_id.to_owned(),
            session_id: "$3".into(),
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
    server: &str,
) -> ManagedSessionId {
    let mut record = make_active_test_record(name, "t", "/tmp");
    record.state = state;
    record.pane_id = Some(pane.into());
    record.tmux_server = Some(server.into());
    let id = record.id;
    mgr.store.write().await.upsert(record).await.expect("seed");
    id
}

async fn manager(tmux: Arc<RebindTmux>) -> (tempfile::TempDir, Arc<SessionManager>) {
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

/// The record's `(tmux_name, pane_id, tmux_server, state)`.
async fn binding(
    mgr: &SessionManager,
    id: &ManagedSessionId,
) -> (String, Option<String>, Option<String>, ManagedSessionState) {
    let r = mgr.get(id).await.expect("record");
    (r.tmux_name, r.pane_id, r.tmux_server, r.state)
}

fn bound(name: &str, pane: &str, server: &str) -> (String, Option<String>, Option<String>) {
    (name.into(), Some(pane.into()), Some(server.into()))
}

/// #9313 acceptance: tm-dogfood's record names `%12` on the old server; the
/// live session runs on the new server in `%1`. `tm ls` lists it active, and
/// the record now names `%1` on the new server. tmux was only read.
///
/// Test: this function IS the test.
#[tokio::test]
async fn exactly_one_pane_under_the_name_rebinds_a_stale_server_record() {
    let tmux = Arc::new(RebindTmux::new(
        &["tm-dogfood"],
        &[("%1", NEW, "tm-dogfood")],
    ));
    let (_dir, mgr) = manager(Arc::clone(&tmux)).await;
    let id = seed(&mgr, "tm-dogfood", ManagedSessionState::Active, "%12", OLD).await;
    let state = Arc::new(DaemonState::with_session_manager(Arc::clone(&mgr)));

    assert_eq!(
        listed(&state, &id).await,
        ("tm-dogfood".to_string(), "active".to_string())
    );
    let (name, pane, server, st) = binding(&mgr, &id).await;
    assert_eq!((name, pane, server), bound("tm-dogfood", "%1", NEW));
    assert_eq!(st, ManagedSessionState::Active);
    assert!(tmux.mutations().is_empty(), "tmux was only read");
}

/// #9313: the session is live but lists no pane, so nothing matches; the
/// record keeps its old binding.
///
/// Test: this function IS the test.
#[tokio::test]
async fn no_pane_under_the_name_leaves_the_record_unchanged() {
    let tmux = Arc::new(RebindTmux::new(&["tm-iris"], &[]));
    let (_dir, mgr) = manager(Arc::clone(&tmux)).await;
    let id = seed(&mgr, "tm-iris", ManagedSessionState::Stopped, "%4", OLD).await;

    let results = mgr.rebind_stale_panes().await;

    assert!(
        matches!(results.as_slice(), [(rid, RebindOutcome::NoMatch(_))] if *rid == id),
        "{results:?}"
    );
    let (name, pane, server, st) = binding(&mgr, &id).await;
    assert_eq!((name, pane, server), bound("tm-iris", "%4", OLD));
    assert_eq!(st, ManagedSessionState::Stopped);
    assert!(tmux.mutations().is_empty());
}

/// #9313 acceptance: two candidate panes refuse, in the automatic pass and
/// in the manual verb. Neither picks one; the record is unchanged.
///
/// Test: this function IS the test.
#[tokio::test]
async fn two_panes_in_the_session_refuse_the_rebind() {
    let tmux = Arc::new(RebindTmux::new(
        &["tm-iris"],
        &[("%5", NEW, "tm-iris"), ("%6", NEW, "tm-iris")],
    ));
    let (_dir, mgr) = manager(Arc::clone(&tmux)).await;
    let id = seed(&mgr, "tm-iris", ManagedSessionState::Active, "%4", OLD).await;

    let auto = mgr.rebind_stale_panes().await;
    let manual = mgr.rebind_session(&id, None).await.expect("verb");

    assert!(
        matches!(auto.as_slice(), [(_, RebindOutcome::Ambiguous(why))] if why.contains("%5, %6")),
        "{auto:?}"
    );
    assert!(matches!(manual, RebindOutcome::Ambiguous(_)), "{manual:?}");
    let (name, pane, server, _) = binding(&mgr, &id).await;
    assert_eq!((name, pane, server), bound("tm-iris", "%4", OLD));
    assert!(tmux.mutations().is_empty());
}

/// #9313: two records name the same live session. The one pane matches
/// both, so neither is rebound.
///
/// Test: this function IS the test.
#[tokio::test]
async fn a_second_record_naming_the_session_makes_the_rebind_ambiguous() {
    let tmux = Arc::new(RebindTmux::new(&["tm-a"], &[("%1", NEW, "tm-a")]));
    let (_dir, mgr) = manager(Arc::clone(&tmux)).await;
    let first = seed(&mgr, "tm-a", ManagedSessionState::Active, "%7", OLD).await;
    let second = seed(&mgr, "tm-a", ManagedSessionState::Stopped, "%8", OLD).await;

    let results = mgr.rebind_stale_panes().await;

    assert!(
        results
            .iter()
            .all(|(_, o)| matches!(o, RebindOutcome::Ambiguous(_))),
        "{results:?}"
    );
    assert_eq!(binding(&mgr, &first).await.1.as_deref(), Some("%7"));
    assert_eq!(binding(&mgr, &second).await.1.as_deref(), Some("%8"));
}

/// #9313 Fail-Open Check: `list-panes` fails for tm-a, so tm-a keeps its
/// binding; the failure does not stop tm-b, which has exactly one pane, from
/// being rebound. Pre-fix nothing rebinds tm-b, and an error arm that fell
/// through to a rebind would move tm-a.
///
/// Test: this function IS the test.
#[tokio::test]
async fn a_pane_list_error_leaves_that_record_unchanged_and_rebinds_the_rest() {
    let mut fake = RebindTmux::new(
        &["tm-a", "tm-b"],
        &[("%1", NEW, "tm-a"), ("%2", NEW, "tm-b")],
    );
    fake.pane_list_fails.insert("tm-a".into());
    let tmux = Arc::new(fake);
    let (_dir, mgr) = manager(Arc::clone(&tmux)).await;
    let a = seed(&mgr, "tm-a", ManagedSessionState::Stopped, "%9", OLD).await;
    let b = seed(&mgr, "tm-b", ManagedSessionState::Stopped, "%8", OLD).await;
    let state = Arc::new(DaemonState::with_session_manager(Arc::clone(&mgr)));

    list_core(&state, None, true).await;

    let (name, pane, server, st) = binding(&mgr, &a).await;
    assert_eq!((name, pane, server), bound("tm-a", "%9", OLD));
    assert_eq!(
        st,
        ManagedSessionState::Stopped,
        "an error never advances state"
    );
    let (name, pane, server, st) = binding(&mgr, &b).await;
    assert_eq!((name, pane, server), bound("tm-b", "%2", NEW));
    assert_eq!(st, ManagedSessionState::Active);
    assert!(tmux.mutations().is_empty());
}

/// #9313 Fail-Open Check: the pane identity cannot be read, so the manual
/// verb answers an error and the route 503; the record is unchanged.
///
/// Test: this function IS the test.
#[tokio::test]
async fn a_tmux_error_in_the_manual_verb_changes_nothing() {
    let mut fake = RebindTmux::new(&["tm-a"], &[("%1", NEW, "tm-a")]);
    fake.identity_fails = true;
    let (_dir, mgr) = manager(Arc::new(fake)).await;
    let id = seed(&mgr, "tm-a", ManagedSessionState::Stopped, "%9", OLD).await;
    let state = Arc::new(DaemonState::with_session_manager(Arc::clone(&mgr)));

    let err = mgr.rebind_session(&id, None).await;
    let route = rebind_core(&state, &id.to_string(), None).await;

    assert!(
        matches!(err, Err(ManagedError::TmuxUnavailable(_))),
        "{err:?}"
    );
    assert_eq!(route.status, 503);
    let (name, pane, server, st) = binding(&mgr, &id).await;
    assert_eq!((name, pane, server), bound("tm-a", "%9", OLD));
    assert_eq!(st, ManagedSessionState::Stopped);
}

/// #9313 acceptance: tm-supervisor was renamed to tm-architect in tmux on
/// the old server, and the server was then replaced. The automatic pass
/// leaves it (its name is not live); `tm sessions rebind tm-supervisor
/// --tmux tm-architect` binds it to `%0` under the live name.
///
/// Test: this function IS the test.
#[tokio::test]
async fn the_manual_verb_rebinds_a_renamed_session_to_the_named_tmux_session() {
    let tmux = Arc::new(RebindTmux::new(
        &["tm-architect"],
        &[("%0", NEW, "tm-architect")],
    ));
    let (_dir, mgr) = manager(Arc::clone(&tmux)).await;
    let id = seed(
        &mgr,
        "tm-supervisor",
        ManagedSessionState::Active,
        "%2140",
        OLD,
    )
    .await;

    assert!(mgr.rebind_stale_panes().await.is_empty());
    assert_eq!(binding(&mgr, &id).await.0, "tm-supervisor");

    let outcome = mgr
        .rebind_session(&id, Some("tm-architect"))
        .await
        .expect("verb");

    assert!(
        matches!(outcome, RebindOutcome::Rebound { .. }),
        "{outcome:?}"
    );
    let (name, pane, server, st) = binding(&mgr, &id).await;
    assert_eq!((name, pane, server), bound("tm-architect", "%0", NEW));
    assert_eq!(st, ManagedSessionState::Active);
    assert_eq!(
        mgr.rebind_session(&id, None).await.expect("again"),
        RebindOutcome::Current
    );
    assert!(tmux.mutations().is_empty());
}

/// #9313: on the SAME server a different pane under the name is a relaunch,
/// not a replacement. The automatic pass leaves it; the verb rebinds it.
///
/// Test: this function IS the test.
#[tokio::test]
async fn a_same_server_pane_change_is_left_to_the_manual_verb() {
    let tmux = Arc::new(RebindTmux::new(&["tm-a"], &[("%3", NEW, "tm-a")]));
    let (_dir, mgr) = manager(Arc::clone(&tmux)).await;
    let id = seed(&mgr, "tm-a", ManagedSessionState::Active, "%2", NEW).await;

    assert!(mgr.rebind_stale_panes().await.is_empty());
    assert_eq!(binding(&mgr, &id).await.1.as_deref(), Some("%2"));

    let outcome = mgr.rebind_session(&id, None).await.expect("verb");

    assert!(
        matches!(outcome, RebindOutcome::Rebound { .. }),
        "{outcome:?}"
    );
    assert_eq!(binding(&mgr, &id).await.1.as_deref(), Some("%3"));
}

/// #9313: boot reconcile with auto-resume on rebinds a stopped record whose
/// session runs on the replaced server, marks it active, and launches, kills
/// or signals nothing.
///
/// Test: this function IS the test.
#[tokio::test]
async fn boot_reconcile_rebinds_and_lists_the_session_active_without_a_launch_or_kill() {
    let tmux = Arc::new(RebindTmux::new(
        &["tm-dogfood"],
        &[("%1", NEW, "tm-dogfood")],
    ));
    let (_dir, mgr) = manager(Arc::clone(&tmux)).await;
    let id = seed(&mgr, "tm-dogfood", ManagedSessionState::Stopped, "%12", OLD).await;

    mgr.reconcile_on_boot(true).await.expect("reconcile");

    let (name, pane, server, st) = binding(&mgr, &id).await;
    assert_eq!((name, pane, server), bound("tm-dogfood", "%1", NEW));
    assert_eq!(st, ManagedSessionState::Active);
    assert!(
        tmux.mutations().is_empty(),
        "no launch, kill or signal: {:?}",
        tmux.mutations()
    );
}

/// #9313: the routes report each outcome — `rebound` with the new pane,
/// `ambiguous` with the reason, 404 for an unknown id, 409 for a terminal
/// record — and `--all` covers only active and stopped records.
///
/// Test: this function IS the test.
#[tokio::test]
async fn the_rebind_routes_report_each_outcome() {
    let tmux = Arc::new(RebindTmux::new(
        &["tm-a", "tm-b"],
        &[
            ("%1", NEW, "tm-a"),
            ("%2", NEW, "tm-b"),
            ("%3", NEW, "tm-b"),
        ],
    ));
    let (_dir, mgr) = manager(Arc::clone(&tmux)).await;
    let a = seed(&mgr, "tm-a", ManagedSessionState::Active, "%9", OLD).await;
    let b = seed(&mgr, "tm-b", ManagedSessionState::Stopped, "%8", OLD).await;
    let gone = seed(&mgr, "tm-c", ManagedSessionState::Deleted, "%7", OLD).await;
    let state = Arc::new(DaemonState::with_session_manager(Arc::clone(&mgr)));

    let all = rebind_all_core(&state).await;
    let RouteBody::Json(body) = all.body else {
        panic!("rebind --all answered {}: {:?}", all.status, all.body);
    };
    let rows = body["results"].as_array().expect("results");
    let row = |id: &ManagedSessionId| {
        rows.iter()
            .find(|r| r["id"] == id.to_string())
            .unwrap_or_else(|| panic!("no row for {id} in {body}"))
    };
    assert_eq!(rows.len(), 2, "terminal records are not rebound: {body}");
    assert_eq!(row(&a)["outcome"], "rebound");
    assert_eq!(row(&a)["pane_id"], "%1");
    assert_eq!(row(&b)["outcome"], "ambiguous");
    assert!(row(&b)["detail"].as_str().unwrap().contains("2 panes"));

    let unknown = rebind_core(&state, &ManagedSessionId::new().to_string(), None).await;
    assert_eq!(unknown.status, 404);
    let terminal = rebind_core(&state, &gone.to_string(), None).await;
    assert_eq!(terminal.status, 409);
    assert!(tmux.mutations().is_empty());
}

/// #9313: the real driver against a private `-L` tmux server lists every
/// pane of a session across its windows, and a missing session is an `Err`,
/// never an empty list.
///
/// Test: this function IS the test.
#[serial_test::serial]
#[test]
fn the_real_driver_lists_every_pane_of_a_live_session() {
    use crate::test_support::tmux_session::{
        PrivateTmuxServer, ScratchTmuxSession, reserved_session_name,
    };
    if !ScratchTmuxSession::tmux_available("tmux") {
        eprintln!("tmux not available; skipping");
        return;
    }
    let server = PrivateTmuxServer::new("tmux", "9313-panes");
    let name = reserved_session_name("9313-panes");
    let _session =
        ScratchTmuxSession::spawn_on_socket("tmux", Some(server.name()), &name, "sleep 600");
    let driver = crate::session_manager::RealTmuxDriver::from_driver_for_test(
        crate::daemon::tmux::TmuxDriver::with_tmux_path_for_test(server.shim_bin()),
    );

    let one = driver.session_pane_ids(&name).expect("list-panes");
    assert_eq!(one.len(), 1, "{one:?}");
    assert_eq!(Some(one[0].clone()), driver.get_pane_id(&name));

    let target = trusty_common::tmux::exact_window_target(&name);
    server
        .query(&["new-window", "-d", "-t", &target, "sleep 600"])
        .expect("new-window");
    let two = driver.session_pane_ids(&name).expect("list-panes");
    assert_eq!(two.len(), 2, "{two:?}");
    assert!(two.iter().all(|p| p.starts_with('%')), "{two:?}");

    assert!(driver.session_pane_ids("tm-9313-no-such-session").is_err());
}
