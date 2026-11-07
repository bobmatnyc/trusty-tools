//! The orphan-GC sweep on every live workspace tmux server (#10101).
//!
//! Why: a fresh PM now runs on its workspace's private server, so an orphan
//! there is invisible to the default server's sweep and would never be
//! reaped.
//! What: [`WorkspaceServerGc::sweep`] lists the live `tm-ws-*` servers, lists
//! each one's managed panes, and runs [`orphan_gc::run_sweep_fs`] against a
//! debounce of that server's own, killing through a driver on that server. A
//! server that cannot be listed or reached is skipped for the tick (reaps
//! nothing), the same fail-closed rule as the default server's sweep. Each
//! listing is bounded, one in flight per server ([`list_server_panes`]).
//! [`ServerPanes`] is the same per-server listing for the runtime-exit reaper.
//! Test: `orphan_gc_servers_tests.rs`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::orphan_gc::{self, ChildLivenessProbe, OrphanGc, TrackedNames};
use super::tmux::TmuxDriver;
use crate::session_manager::ManagedTmuxDriver;

/// One orphan-GC debounce per workspace server, by server name.
#[derive(Default)]
pub(crate) struct WorkspaceServerGc {
    gcs: HashMap<String, OrphanGc>,
}

impl WorkspaceServerGc {
    /// Sweep every live workspace server in this process's socket directory;
    /// returns the count reaped.
    /// Test: `an_orphan_on_a_workspace_server_is_reaped_there`.
    pub(crate) fn sweep(
        &mut self,
        tracked: &TrackedNames,
        probe: &dyn ChildLivenessProbe,
        driver: &TmuxDriver,
        mtmux: &dyn ManagedTmuxDriver,
    ) -> usize {
        let dir = crate::core::tmux_server::ambient_socket_dir();
        self.sweep_in(&dir, tracked, probe, driver, mtmux)
    }

    /// [`Self::sweep`] over the servers in `socket_dir`.
    fn sweep_in(
        &mut self,
        socket_dir: &Path,
        tracked: &TrackedNames,
        probe: &dyn ChildLivenessProbe,
        driver: &TmuxDriver,
        mtmux: &dyn ManagedTmuxDriver,
    ) -> usize {
        let servers = match crate::core::tmux_server::live_workspace_servers(socket_dir) {
            Ok(servers) => servers,
            Err(e) => {
                tracing::warn!("orphan-GC: workspace tmux servers unlistable, skipped: {e}");
                return 0;
            }
        };
        // A server that is gone takes its debounce with it.
        self.gcs.retain(|name, _| servers.contains(name));
        let mut reaped = 0;
        for server in servers {
            let Some(panes) = list_server_panes(driver, socket_dir, &server) else {
                tracing::warn!("orphan-GC: tmux server '{server}' unlistable, skipped");
                continue;
            };
            let Some(kill) = mtmux.on_server(&server) else {
                tracing::warn!("orphan-GC: tmux server '{server}' unreachable, skipped");
                continue;
            };
            let gc = self.gcs.entry(server).or_insert_with(OrphanGc::from_env);
            reaped += orphan_gc::run_sweep_fs(gc, &panes, tracked, probe, kill.as_ref());
        }
        reaped
    }
}

/// The managed panes on workspace server `server` in `socket_dir`; `None`
/// when they cannot be listed (#10101).
///
/// What: the listing runs through [`crate::core::tmux_server::bounded_probe`]
/// keyed on the server's socket: within the probe budget, and not at all
/// while an earlier listing of that server is still blocked.
/// Test: `a_hung_workspace_server_is_listed_once_across_sweeps`.
pub(crate) fn list_server_panes(
    driver: &TmuxDriver,
    socket_dir: &Path,
    server: &str,
) -> Option<Vec<orphan_gc::PaneInfo>> {
    use crate::core::tmux_server::{PROBE_BUDGET, bounded_probe, server_socket_path};
    let named = driver.on_server(server);
    let key = server_socket_path(socket_dir, server);
    match bounded_probe(&key, PROBE_BUDGET, move || named.list_managed_panes()) {
        Some(Ok(panes)) => Some(panes),
        Some(Err(e)) => {
            tracing::warn!("tmux server '{server}' could not list its panes: {e}");
            None
        }
        None => {
            tracing::warn!("tmux server '{server}' did not list its panes in time");
            None
        }
    }
}

/// The managed panes of the default server and of every live workspace
/// server, by server (#10101).
///
/// Why: a pane belongs to one server, and two servers can hold sessions with
/// one name. A reaper that matched a record against every pane by name alone
/// would read another server's pane as the record's.
/// What: [`Self::panes_of`] answers a record's own server's panes, or `None`
/// when that server's panes are unknown.
#[derive(Debug, Default)]
pub(crate) struct ServerPanes {
    /// The default server's panes.
    pub(crate) default: Vec<orphan_gc::PaneInfo>,
    /// Each listed workspace server's panes, by server name.
    pub(crate) named: HashMap<String, Vec<orphan_gc::PaneInfo>>,
    /// Live workspace servers whose panes could not be listed.
    pub(crate) unknown: HashSet<String>,
    /// The workspace servers could not be listed at all.
    pub(crate) named_unlisted: bool,
    /// Where the workspace servers were listed.
    pub(crate) socket_dir: Option<PathBuf>,
}

impl ServerPanes {
    /// The default server's `panes` alone: every workspace server unknown.
    pub(crate) fn default_only(panes: Vec<orphan_gc::PaneInfo>) -> Self {
        Self {
            default: panes,
            named_unlisted: true,
            ..Self::default()
        }
    }

    /// `default` plus the panes of every live workspace server in
    /// `socket_dir`, each listed by [`list_server_panes`].
    /// Test: `a_named_pm_whose_claude_exited_is_stopped_on_its_own_server`.
    pub(crate) fn list(
        driver: &TmuxDriver,
        socket_dir: &Path,
        default: Vec<orphan_gc::PaneInfo>,
    ) -> Self {
        let mut all = Self {
            default,
            socket_dir: Some(socket_dir.to_path_buf()),
            ..Self::default()
        };
        match crate::core::tmux_server::live_workspace_servers(socket_dir) {
            Ok(servers) => {
                for server in servers {
                    match list_server_panes(driver, socket_dir, &server) {
                        Some(panes) => {
                            all.named.insert(server, panes);
                        }
                        None => {
                            all.unknown.insert(server);
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!("workspace tmux servers unlistable: {e}");
                all.named_unlisted = true;
            }
        }
        all
    }

    /// The panes on `record`'s own server; `None` when they are unknown: an
    /// unnamable socket, one outside the listed directory, or a server that
    /// could not be listed. A server that is not live has no panes.
    /// Test: `a_default_pane_with_a_named_records_name_never_stops_it`.
    pub(crate) fn panes_of(
        &self,
        record: &crate::session_manager::SessionRecord,
    ) -> Option<&[orphan_gc::PaneInfo]> {
        use crate::session_manager::tmux_route::{in_socket_dir, record_server};
        let server = match record_server(record) {
            Ok(None) => return Some(&self.default),
            Ok(Some(server)) => server,
            Err(_) => return None,
        };
        if self.named_unlisted
            || self.unknown.contains(&server)
            || !in_socket_dir(record, self.socket_dir.as_deref())
        {
            return None;
        }
        Some(self.named.get(&server).map_or(&[], Vec::as_slice))
    }
}

#[cfg(test)]
#[path = "orphan_gc_servers_tests.rs"]
mod tests;
