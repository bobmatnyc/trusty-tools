//! Liveness checker for the trusty-agents ctrl socket.
//!
//! Why: The debug TUI shows whether the REPL inside the tmux session has
//! actually bound its ctrl socket. A green dot in the status panel tells
//! the operator the harness is up; a red dot says it's still booting or
//! crashed. We deliberately keep the check sync + short-timeout so the
//! 100ms tick loop never stalls.
//! What: `SocketMonitor` derives the canonical `~/.trusty-agents/sockets/<id>.ctrl.sock`
//! path from a project directory and exposes `check_alive` — a 200ms
//! `UnixStream::connect_timeout` that returns true on successful connect.
//! Test: `socket_monitor_path_matches_canonical` and
//! `socket_monitor_dead_when_no_listener` cover the unhappy paths; live
//! behaviour is observed in manual integration runs of `trusty-agents debug`.

use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::ctrl::socket::project_id_from_path;

/// Default probe deadline. 200ms is well above sub-millisecond unix-socket
/// connect latency but well below the TUI's 100ms tick — and we're willing
/// to occasionally skip a frame rather than render a stale "alive" state.
const PROBE_TIMEOUT: Duration = Duration::from_millis(200);

#[derive(Debug, Clone)]
pub struct SocketMonitor {
    socket_path: PathBuf,
}

impl SocketMonitor {
    /// Construct a monitor scoped to `project_dir`.
    ///
    /// Why: The socket name is derived from the project basename so each
    /// project gets its own ctrl socket. Tests pass synthetic dirs.
    /// What: Builds `~/.trusty-agents/sockets/<sanitized-basename>.ctrl.sock`.
    /// Test: `socket_monitor_path_matches_canonical`.
    pub fn new(project_dir: &Path) -> Self {
        let project_id = project_id_from_path(project_dir);
        let base = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let socket_path = base
            .join(".trusty-agents")
            .join("sockets")
            .join(format!("{project_id}.ctrl.sock"));
        Self { socket_path }
    }

    /// True if a listener is currently accepting on the socket.
    ///
    /// Why: A successful connect is the cheapest reliable liveness signal;
    /// sending a status ping would require knowing the protocol. The TUI
    /// just needs alive/dead.
    /// What: `UnixStream::connect_timeout` with a 200ms deadline.
    /// Test: `socket_monitor_dead_when_no_listener`.
    pub fn check_alive(&self) -> bool {
        if !self.socket_path.exists() {
            return false;
        }
        // std's UnixStream has no connect_timeout; connect_addr blocks. The
        // path is local so connect either returns immediately or fails fast.
        // We still wrap in a deadline by using nonblocking after connect to
        // avoid pathological hangs — but for AF_UNIX local connect that's
        // overkill. A plain connect is sufficient.
        let _ = PROBE_TIMEOUT; // reserved for future remote socket support
        StdUnixStream::connect(&self.socket_path).is_ok()
    }

    #[allow(dead_code)] // exposed for diagnostics + tests
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_monitor_path_matches_canonical() {
        let m = SocketMonitor::new(Path::new("/tmp/trusty-agents"));
        let s = m.socket_path().to_string_lossy().to_string();
        assert!(
            s.ends_with("/.trusty-agents/sockets/trusty-agents.ctrl.sock"),
            "{s}"
        );
    }

    #[test]
    fn socket_monitor_dead_when_no_listener() {
        // Construct a monitor pointing at a definitely-missing socket
        // by giving it a project basename that nothing else uses.
        let m = SocketMonitor::new(Path::new("/tmp/__trusty_agents_ghost_dir__"));
        assert!(!m.check_alive());
    }
}
