//! Tests for #10101 slice D: the `session_names` doctor row probes each
//! record's pane on the record's own tmux server, within the probe budget.
//!
//! Hermetic: every manager runs [`ServerTmux`], an in-memory driver that
//! logs which server each pane probe reached. No tmux binary runs.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::tests::make_active_test_record;
use crate::core::doctor::CheckStatus;
use crate::core::process::PaneClaude;
use crate::daemon::doctor::doctor_session_names::check_session_names;
use crate::session_manager::{ManagedError, ManagedTmuxDriver, SessionManager};

/// Every `pane_claude` probe: (server, session). `None` is the default server.
type Calls = Arc<Mutex<Vec<(Option<String>, String)>>>;

/// A tmux that reaches named servers and logs each pane probe's server.
struct ServerTmux {
    /// The server this driver is on; `None` is the default server.
    server: Option<String>,
    /// A server whose pane probe blocks for [`HANG`].
    hang_on: Option<String>,
    calls: Calls,
}

/// How long a pane probe on the hung server blocks: past the doctor's budget.
const HANG: Duration = Duration::from_secs(6);

impl ServerTmux {
    fn new(hang_on: Option<&str>) -> (Arc<Self>, Calls) {
        let calls = Calls::default();
        let tmux = Arc::new(Self {
            server: None,
            hang_on: hang_on.map(str::to_owned),
            calls: Arc::clone(&calls),
        });
        (tmux, calls)
    }
}

impl ManagedTmuxDriver for ServerTmux {
    fn create_session(&self, _n: &str, _w: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn kill_session(&self, _n: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn send_line(&self, _n: &str, _t: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn capture(&self, _n: &str, _l: usize) -> Result<String, ManagedError> {
        Ok(String::new())
    }
    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(Vec::new())
    }
    fn on_server(&self, server: &str) -> Option<Arc<dyn ManagedTmuxDriver>> {
        Some(Arc::new(Self {
            server: Some(server.to_owned()),
            hang_on: self.hang_on.clone(),
            calls: Arc::clone(&self.calls),
        }))
    }
    /// No `claude` anywhere, so the row never reads the real registry.
    fn pane_claude(&self, name: &str) -> PaneClaude {
        self.calls
            .lock()
            .expect("calls")
            .push((self.server.clone(), name.to_owned()));
        if self.server.is_some() && self.server == self.hang_on {
            std::thread::sleep(HANG);
        }
        PaneClaude::Absent
    }
}

/// A manager over `tmux` holding one live record per `(name, socket)`.
async fn manager(
    tmux: Arc<ServerTmux>,
    records: &[(&str, Option<PathBuf>)],
) -> (tempfile::TempDir, SessionManager) {
    let dir = crate::test_support::hermetic_temp_dir();
    let mgr = SessionManager::new(dir.path(), tmux)
        .await
        .expect("manager");
    for (name, socket) in records {
        let mut record = make_active_test_record(name, "t", "/tmp");
        record.tmux_socket = socket.clone();
        mgr.store.write().await.upsert(record).await.expect("seed");
    }
    (dir, mgr)
}

/// #10101 criteria 2 and 4: a record on a workspace server has its pane
/// probed on that server; a record with no socket still on the default
/// server. Red on ed04279554, which probed both on the default server.
#[tokio::test]
async fn the_doctor_probes_each_record_on_its_own_server() {
    let (tmux, calls) = ServerTmux::new(None);
    let socket = PathBuf::from("/s/tm-ws-10101d-route");
    let (_dir, mgr) = manager(tmux, &[("tm-on-ws", Some(socket)), ("tm-on-default", None)]).await;

    let row = check_session_names(&mgr).await;

    assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
    let mut seen = calls.lock().expect("calls").clone();
    seen.sort();
    assert_eq!(
        seen,
        vec![
            (None, "tm-on-default".to_owned()),
            (Some("tm-ws-10101d-route".to_owned()), "tm-on-ws".to_owned()),
        ],
        "each pane must be probed on its record's own server"
    );
}

/// #10101 criterion 3, error arm (Fail-Open Check): a workspace server whose
/// pane probe hangs. The row finishes within the probe budget and reads
/// `Unknown` naming that server, never `Ok`; a second run while the first
/// probe is still blocked does not probe the server again. Red on
/// ed04279554, which never probed the workspace server and read `Ok`.
#[tokio::test]
async fn a_hung_workspace_server_bounds_the_doctor_row_and_reads_unknown() {
    let server = "tm-ws-10101d-hung";
    let (tmux, calls) = ServerTmux::new(Some(server));
    let socket = PathBuf::from(format!("/s/{server}"));
    let (_dir, mgr) = manager(tmux, &[("tm-hung", Some(socket))]).await;

    let started = Instant::now();
    let row = check_session_names(&mgr).await;
    let took = started.elapsed();

    assert!(took < HANG - Duration::from_secs(1), "took {took:?}");
    assert_eq!(row.status, CheckStatus::Unknown, "{}", row.message);
    assert!(
        row.message
            .contains(&format!("tmux server '{server}' did not answer")),
        "{}",
        row.message
    );

    // One probe in flight per server: the second run spawns no second probe.
    let again = check_session_names(&mgr).await;
    assert_eq!(again.status, CheckStatus::Unknown, "{}", again.message);
    let on_server = calls
        .lock()
        .expect("calls")
        .iter()
        .filter(|(s, _)| s.as_deref() == Some(server))
        .count();
    assert_eq!(on_server, 1, "the hung server was probed again");
}
