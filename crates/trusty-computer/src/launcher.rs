//! Launching and stopping the CuaDriver.app daemon through LaunchServices.
//!
//! Why: the Accessibility grant belongs to `com.trycua.driver`, so the daemon
//! must start through LaunchServices to inherit it; the broker never execs
//! `cua-driver serve` itself (plan section 3; spike `driver.py:96-116`).
//! What: [`Launcher`] is the seam the supervisor drives; [`CuaAppLauncher`]
//! is the real one. [`open_argv`] is the exact `/usr/bin/open` command,
//! [`DAEMON_ENV`] the variables passed with `--env`. [`CuaAppLauncher::stop`]
//! asks `cua-driver stop --socket <private>`, then signals only a pid whose
//! argv names that socket.
//! Test: `open_argv_is_launchservices_bounded_and_private`,
//! `supervisor_daemon_env_turns_off_telemetry_update_and_gate`.

use std::path::Path;
use std::process::Command;

use crate::driver::DriverError;

/// The pinned cua-driver release (spike `policy.py:18`).
pub const DRIVER_VERSION: &str = "0.23.2";
/// The installed app bundle.
pub const APP_PATH: &str = "/Applications/CuaDriver.app";
/// Its binary, run only for `--version` and `stop --socket`.
pub const APP_BINARY: &str = "/Applications/CuaDriver.app/Contents/MacOS/cua-driver";
/// The user's default daemon socket, relative to HOME. Never used.
pub const DEFAULT_SOCKET_SUFFIX: &str = "Library/Caches/cua-driver/cua-driver.sock";

/// Environment for every daemon start and every `cua-driver` client run.
/// Telemetry is read env-first (cua `telemetry.rs:216`), the update check at
/// `version_check.rs:458`.
pub const DAEMON_ENV: [(&str, &str); 4] = [
    ("CUA_DRIVER_RS_TELEMETRY_ENABLED", "false"),
    ("CUA_TELEMETRY_ENABLED", "false"),
    ("CUA_DRIVER_RS_UPDATE_CHECK", "false"),
    ("CUA_DRIVER_RS_PERMISSIONS_GATE", "false"),
];

/// Start, stop and version-probe one daemon.
pub trait Launcher {
    /// The installed driver's version string.
    fn binary_version(&mut self) -> Result<String, DriverError>;
    /// Launch a daemon serving `socket` with `manifest`.
    fn start(&mut self, socket: &Path, manifest: &Path) -> Result<(), DriverError>;
    /// Stop the daemon serving `socket`, whose metadata pid is `pid`.
    fn stop(&mut self, socket: &Path, pid: Option<u32>) -> Result<(), DriverError>;
    /// True when `pid` is the daemon serving `socket` (its argv names the socket).
    fn owns(&mut self, pid: u32, socket: &Path) -> bool;
}

/// The `open` argv that launches a bounded daemon on `socket`.
pub fn open_argv(socket: &Path, manifest: &Path) -> Vec<String> {
    let mut argv: Vec<String> = ["/usr/bin/open", "-n", "-g", "-a", "CuaDriver"]
        .map(String::from)
        .to_vec();
    for (key, value) in DAEMON_ENV {
        argv.push("--env".into());
        argv.push(format!("{key}={value}"));
    }
    argv.extend(
        [
            "--args",
            "serve",
            "--socket",
            &socket.to_string_lossy(),
            "--permission-mode",
            "bounded",
            "--capability-manifest",
            &manifest.to_string_lossy(),
            "--approve-capability-manifest",
            "--no-permissions-gate",
            "--no-overlay",
        ]
        .map(String::from),
    );
    argv
}

/// True when `socket` is a user's default daemon socket (`~/Library/Caches/cua-driver/...`).
pub fn is_default_socket(socket: &Path) -> bool {
    socket.ends_with(DEFAULT_SOCKET_SUFFIX)
}

fn lifecycle(what: &str, e: impl std::fmt::Display) -> DriverError {
    DriverError::Lifecycle(format!("{what}: {e}"))
}

/// The real launcher: LaunchServices for start, `cua-driver stop --socket` for stop.
#[derive(Debug, Default)]
pub struct CuaAppLauncher;

impl CuaAppLauncher {
    fn client_cmd(args: &[&str]) -> Command {
        let mut cmd = Command::new(APP_BINARY);
        cmd.args(args).envs(DAEMON_ENV);
        cmd
    }
}

impl Launcher for CuaAppLauncher {
    fn binary_version(&mut self) -> Result<String, DriverError> {
        let out = Self::client_cmd(&["--version"])
            .output()
            .map_err(|e| lifecycle("cua-driver --version", e))?;
        let text = String::from_utf8_lossy(&out.stdout);
        text.split_whitespace()
            .last()
            .map(str::to_owned)
            .ok_or_else(|| lifecycle("cua-driver --version", "printed nothing"))
    }

    fn start(&mut self, socket: &Path, manifest: &Path) -> Result<(), DriverError> {
        let argv = open_argv(socket, manifest);
        let status = Command::new(&argv[0])
            .args(&argv[1..])
            .status()
            .map_err(|e| lifecycle("open -a CuaDriver", e))?;
        if status.success() {
            Ok(())
        } else {
            Err(lifecycle("open -a CuaDriver", status))
        }
    }

    fn stop(&mut self, socket: &Path, pid: Option<u32>) -> Result<(), DriverError> {
        let sock = socket.to_string_lossy();
        // A failed `stop` is not fatal: the supervisor waits, then falls back to the pid.
        let _ = Self::client_cmd(&["stop", "--socket", &sock]).status();
        if let Some(pid) = pid.filter(|&p| self.owns(p, socket)) {
            wait_gone(pid, 50);
            if self.owns(pid, socket) {
                // SAFETY: kill(2) on a pid whose argv was just checked to name our socket.
                unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
            }
        }
        Ok(())
    }

    fn owns(&mut self, pid: u32, socket: &Path) -> bool {
        let Ok(out) = Command::new("/bin/ps")
            .args(["-p", &pid.to_string(), "-o", "command="])
            .output()
        else {
            return false;
        };
        let argv = String::from_utf8_lossy(&out.stdout);
        let needle = format!("--socket {} ", socket.to_string_lossy());
        argv.contains(" serve ") && argv.contains(&needle)
    }
}

/// Poll up to `tenths` x 100 ms for `pid` to exit.
fn wait_gone(pid: u32, tenths: u32) {
    for _ in 0..tenths {
        // SAFETY: signal 0 only probes for existence.
        if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
