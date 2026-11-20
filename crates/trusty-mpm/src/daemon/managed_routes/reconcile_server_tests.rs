//! Tests for the list/get live-state reconcile on workspace tmux servers
//! (#11222, #11071).
//!
//! Why: `tm session ls` reconciled every record against the default tmux
//! server only, so a live session on its `tm-ws-*` server read `stopped`.
//! What: a fake driver whose default server and one named server hold
//! different sessions and panes; the reconcile must judge each record on its
//! own server.
//! Test: this file.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use super::summary::{reconcile_against_tmux, record_to_summary};
use super::tests::make_record;
use crate::daemon::orphan_gc::PaneInfo;
use crate::session_manager::{ManagedError, ManagedSessionState, ManagedTmuxDriver};

/// One tmux server: its session names, and each pane's foreground command.
#[derive(Clone, Default)]
struct Server {
    sessions: Vec<String>,
    panes: Vec<&'static str>,
}

/// A driver on the default server that reaches the named servers in `named`.
struct ServersTmux {
    own: Server,
    named: HashMap<String, Server>,
}

impl ServersTmux {
    fn new(own: Server, named: &[(&str, Server)]) -> Self {
        Self {
            own,
            named: named
                .iter()
                .map(|(n, s)| ((*n).to_owned(), s.clone()))
                .collect(),
        }
    }
}

impl ManagedTmuxDriver for ServersTmux {
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
        Ok(self.own.sessions.clone())
    }
    fn pane_exists_checked(&self, name: &str, pane_id: &str) -> Option<bool> {
        Some(self.own.sessions.iter().any(|s| s == name) && pane_id == "%5")
    }
    fn session_panes(&self, name: &str) -> Result<Vec<PaneInfo>, ManagedError> {
        Ok(self
            .own
            .panes
            .iter()
            .enumerate()
            .map(|(i, c)| PaneInfo {
                session_name: name.to_owned(),
                pane_current_command: (*c).to_owned(),
                pane_pid: Some(4242),
                pane_id: Some(format!("%{}", i + 5)),
                pane_current_path: None,
            })
            .collect())
    }
    fn on_server(&self, server: &str) -> Option<Arc<dyn ManagedTmuxDriver>> {
        let own = self.named.get(server)?.clone();
        Some(Arc::new(ServersTmux::new(own, &[])))
    }
    fn workspace_servers(&self) -> Result<Vec<String>, ManagedError> {
        Ok(self.named.keys().cloned().collect())
    }
}

/// A `state` record named `tm-ws-pm` with pane `%5` on server `tm-ws-x`.
fn ws_record(state: ManagedSessionState) -> crate::session_manager::SessionRecord {
    let mut rec = make_record(None);
    rec.state = state;
    rec.tmux_name = "tm-ws-pm".into();
    rec.pane_id = Some("%5".into());
    rec.tmux_socket = Some(PathBuf::from("/s/tm-ws-x"));
    rec
}

fn server(sessions: &[&str], panes: &[&'static str]) -> Server {
    Server {
        sessions: sessions.iter().map(|s| (*s).to_owned()).collect(),
        panes: panes.to_vec(),
    }
}

/// The reconciled display state of `rec` under `tmux`.
fn reconciled(tmux: &ServersTmux, rec: crate::session_manager::SessionRecord) -> String {
    let records = vec![rec];
    let mut summaries: Vec<_> = records.iter().map(record_to_summary).collect();
    reconcile_against_tmux(tmux, &mut summaries, &records);
    summaries.remove(0).state
}

/// #11222: an Active record whose session and pane live only on its
/// workspace server reads `active`, though the default server holds nothing.
#[test]
fn a_live_session_on_its_workspace_server_reads_active_11222() {
    let tmux = ServersTmux::new(
        Server::default(),
        &[("tm-ws-x", server(&["tm-ws-pm"], &["claude"]))],
    );
    assert_eq!(
        reconciled(&tmux, ws_record(ManagedSessionState::Active)),
        "active"
    );
}

/// #11222 (preserved): the same record whose workspace server lists nothing
/// reads `stopped`, even when the default server holds a same-named session.
#[test]
fn a_same_named_default_session_never_keeps_a_workspace_record_live_11222() {
    let tmux = ServersTmux::new(
        server(&["tm-ws-pm"], &["claude"]),
        &[("tm-ws-x", server(&["tm-ws-other"], &["claude"]))],
    );
    assert_eq!(
        reconciled(&tmux, ws_record(ManagedSessionState::Active)),
        "stopped"
    );
}

/// #11071: a Stopped record whose workspace-server pane runs its runtime
/// reads `active`, as `tm tui` shows it; the default server's idle shell of a
/// same-named session never decides it.
#[test]
fn a_stopped_record_running_on_its_workspace_server_reads_active_11071() {
    use crate::daemon::orphan_gc::AlwaysIdleProbe;
    use crate::session_manager::tmux_route::observe_workspace_servers;
    let tmux = ServersTmux::new(
        server(&[], &["zsh"]),
        &[("tm-ws-x", server(&["tm-ws-pm"], &["claude"]))],
    );
    let records = vec![ws_record(ManagedSessionState::Stopped)];
    let mut summaries: Vec<_> = records.iter().map(record_to_summary).collect();
    let live = observe_workspace_servers(&tmux, Default::default());
    let attached = std::collections::HashSet::new();
    super::summary::reconcile_live_state_with_probe(
        &tmux,
        &mut summaries,
        &records,
        &live,
        &attached,
        &AlwaysIdleProbe,
    );
    assert_eq!(summaries[0].state, "active");
}
