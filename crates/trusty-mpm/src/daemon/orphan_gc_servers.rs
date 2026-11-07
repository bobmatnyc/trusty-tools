//! The orphan-GC sweep on every live workspace tmux server (#10101).
//!
//! Why: a fresh PM now runs on its workspace's private server, so an orphan
//! there is invisible to the default server's sweep and would never be
//! reaped.
//! What: [`WorkspaceServerGc::sweep`] lists the live `tm-ws-*` servers, lists
//! each one's managed panes, and runs [`orphan_gc::run_sweep_fs`] against a
//! debounce of that server's own, killing through a driver on that server. A
//! server that cannot be listed or reached is skipped for the tick (reaps
//! nothing), the same fail-closed rule as the default server's sweep.
//! Test: `orphan_gc_servers_tests.rs`.

use std::collections::HashMap;
use std::path::Path;

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
            let panes = match driver.on_server(&server).list_managed_panes() {
                Ok(panes) => panes,
                Err(e) => {
                    tracing::warn!("orphan-GC: tmux server '{server}' unlistable, skipped: {e}");
                    continue;
                }
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

#[cfg(test)]
#[path = "orphan_gc_servers_tests.rs"]
mod tests;
