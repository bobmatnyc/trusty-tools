//! `tm doctor` row for the scheduling priority of the RUNNING tmux server (#8415).
//!
//! Why: `launchd_process_type` judges the plists, which decide the class of a
//! tmux server started from now on. A server already running keeps the class
//! it started with — fixing the plist does not lift it, and `taskpolicy -B`
//! cannot either. On the #8415 host the server sat at Darwin priority 4 while
//! every plist row would have read green after the edit. This row observes the
//! process itself, so a clamped server is reported until it is restarted.
//!
//! What: [`probe_tmux_priority`] asks tmux for its server PID
//! (`display-message -p '#{pid}'`) and `ps` for that PID's priority, through an
//! injected [`Runner`] so every branch is testable without a live tmux.
//! [`build_tmux_priority_check`] folds the result: below [`CLAMP_THRESHOLD`]
//! fails, from there up to [`INTERACTIVE_PRIORITY`] warns (launchd `Standard`
//! throttling), at or above passes, no server passes, and any read that did
//! not succeed is `Unknown` — never `Ok`. The probe runs on macOS only: procps
//! `ps -o pri` on Linux prints `39 - kernel_prio`, a different scale, so there
//! the row reports not-applicable instead of judging a number it cannot read.
//!
//! Test: `doctor_tmux_priority_tests.rs`.

use crate::core::doctor::{CheckStatus, DoctorCheck};

/// The `tm doctor` row name.
pub(crate) const CHECK_NAME: &str = "tmux_priority";

/// Lowest Darwin priority (`ps -o pri`) this row accepts for the tmux server.
///
/// Why: an interactive shell runs at 31 and a launchd job with the default
/// `Standard` class at 20; a `Background` job and everything it spawns sits at
/// 4 (#8415). Anything below 20 is in the background band.
pub(crate) const CLAMP_THRESHOLD: i32 = 20;

/// Darwin priority of an unthrottled interactive process; below it, down to
/// [`CLAMP_THRESHOLD`], the server runs under launchd `Standard` throttling.
pub(crate) const INTERACTIVE_PRIORITY: i32 = 31;

/// What one subprocess returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CmdOut {
    /// Whether the process exited 0.
    pub success: bool,
    /// Its stdout, lossily decoded.
    pub stdout: String,
    /// Its stderr, lossily decoded.
    pub stderr: String,
}

/// Runs `program args…`; `Err` when it could not be spawned at all.
///
/// Why: the seam that lets tests stand in for `tmux` and `ps`.
pub(crate) type Runner<'a> = &'a dyn Fn(&str, &[&str]) -> Result<CmdOut, String>;

/// What the probe learned about the tmux server.
///
/// Test: `probe_errors_are_unknown`, `no_server_passes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TmuxPriority {
    /// tmux answered that no server is running.
    NoServer,
    /// The server's PID and its `ps -o pri` priority.
    Observed {
        /// The tmux server PID.
        pid: u32,
        /// Its Darwin scheduling priority.
        priority: i32,
    },
    /// The priority could not be read, and why.
    Unreadable(String),
}

/// Whether tmux's stderr says there is no server on its socket.
///
/// What: tmux prints `no server running on <socket>` or `error connecting to
/// <socket> (No such file or directory)`. Any other failure — a permission
/// error included — is an unreadable probe, not an absent server.
fn is_no_server(stderr: &str) -> bool {
    stderr.contains("no server running")
        || (stderr.contains("error connecting to") && stderr.contains("No such file or directory"))
}

/// Read the tmux server's PID and priority through `run`.
///
/// What: `tmux display-message -p '#{pid}'`, then `ps -o pri= -p <pid>`. Every
/// spawn error, non-zero exit (other than tmux's no-server answer) and
/// unparsable output becomes [`TmuxPriority::Unreadable`] with the reason.
/// Test: `clamped_server_fails`, `normal_server_passes`, `no_server_passes`,
/// `probe_errors_are_unknown`.
pub(crate) fn probe_tmux_priority(tmux_bin: &str, run: Runner<'_>) -> TmuxPriority {
    let out = match run(tmux_bin, &["display-message", "-p", "#{pid}"]) {
        Ok(out) => out,
        Err(e) => return TmuxPriority::Unreadable(format!("could not run `{tmux_bin}`: {e}")),
    };
    if !out.success {
        if is_no_server(&out.stderr) {
            return TmuxPriority::NoServer;
        }
        return TmuxPriority::Unreadable(format!(
            "`tmux display-message` failed: {}",
            out.stderr.trim()
        ));
    }
    let pid = match out.stdout.trim().parse::<u32>() {
        Ok(pid) if pid > 0 => pid,
        _ => {
            return TmuxPriority::Unreadable(format!(
                "unparsable tmux server PID: {:?}",
                out.stdout.trim()
            ));
        }
    };
    let pid_arg = pid.to_string();
    let ps = match run("ps", &["-o", "pri=", "-p", &pid_arg]) {
        Ok(ps) => ps,
        Err(e) => return TmuxPriority::Unreadable(format!("could not run `ps`: {e}")),
    };
    if !ps.success {
        return TmuxPriority::Unreadable(format!(
            "`ps -o pri= -p {pid}` failed: {}",
            ps.stderr.trim()
        ));
    }
    match ps.stdout.trim().parse::<i32>() {
        Ok(priority) => TmuxPriority::Observed { pid, priority },
        Err(_) => TmuxPriority::Unreadable(format!(
            "unparsable priority for tmux server {pid}: {:?}",
            ps.stdout.trim()
        )),
    }
}

/// Fold a probe result into the row.
///
/// What: `Observed` below [`CLAMP_THRESHOLD`] → `Fail`, naming the PID, the
/// priority and the remedy; below [`INTERACTIVE_PRIORITY`] → `Warn`; at or
/// above → `Ok`; `NoServer` → `Ok`; `Unreadable` → `Unknown` with the reason.
/// Test: `clamped_server_fails`, `standard_throttled_server_warns`,
/// `normal_server_passes`, `no_server_passes`, `probe_errors_are_unknown`.
pub(crate) fn build_tmux_priority_check(probe: &TmuxPriority) -> DoctorCheck {
    match probe {
        TmuxPriority::NoServer => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            "no tmux server is running; nothing to judge",
        ),
        TmuxPriority::Observed { pid, priority } if *priority < CLAMP_THRESHOLD => {
            DoctorCheck::new(
                CHECK_NAME,
                CheckStatus::Fail,
                format!(
                    "tmux server PID {pid} runs at Darwin priority {priority}, below \
                     {CLAMP_THRESHOLD}: it is background-clamped, and every tm session and \
                     build inside it inherits the clamp (#8415). Fix the plist the \
                     `launchd_process_type` row names, then restart the tmux server \
                     (`tmux kill-server` ends every session in it; resume them with `tm`). \
                     A running server keeps its class until it exits."
                ),
            )
        }
        TmuxPriority::Observed { pid, priority } if *priority < INTERACTIVE_PRIORITY => {
            DoctorCheck::new(
                CHECK_NAME,
                CheckStatus::Warn,
                format!(
                    "tmux server PID {pid} runs at Darwin priority {priority}, below the \
                     interactive {INTERACTIVE_PRIORITY}: it runs under launchd `Standard` \
                     throttling, as the job that started it does. See the \
                     `launchd_process_type` row for the plist to set to `Interactive`, then \
                     restart the tmux server (`tmux kill-server`; resume sessions with `tm`)."
                ),
            )
        }
        TmuxPriority::Observed { pid, priority } => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            format!(
                "tmux server PID {pid} runs at Darwin priority {priority} \
                 (interactive: {INTERACTIVE_PRIORITY})"
            ),
        ),
        TmuxPriority::Unreadable(why) => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            format!("could not read the tmux server's priority: {why}"),
        ),
    }
}

/// Run a real subprocess for [`probe_tmux_priority`].
fn run_real(program: &str, args: &[&str]) -> Result<CmdOut, String> {
    let out = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    Ok(CmdOut {
        success: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// Build the row for a platform: probe on macOS, not-applicable elsewhere.
///
/// Why: `ps -o pri` is a Darwin priority only on macOS; procps prints
/// `39 - kernel_prio`, so a nice-0 Linux server would read 19 and fail with
/// launchd advice for a host that has no launchd.
/// What: `probe` runs only when `is_macos`; otherwise the row is `Ok` with
/// "not applicable on this platform".
/// Test: `other_platforms_are_not_applicable`.
pub(crate) fn tmux_priority_row(
    is_macos: bool,
    probe: impl FnOnce() -> TmuxPriority,
) -> DoctorCheck {
    if !is_macos {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            "not applicable on this platform (the priority scale is Darwin's)",
        );
    }
    build_tmux_priority_check(&probe())
}

/// Probe the live tmux server and build the row. Read-only.
///
/// Test: the pure halves are covered in `doctor_tmux_priority_tests.rs`; this
/// wiring by `run_doctor_produces_sixty_one_checks`.
pub(crate) fn check_tmux_priority() -> DoctorCheck {
    tmux_priority_row(cfg!(target_os = "macos"), || {
        let bin = crate::core::tmux::resolve_tmux_binary_or_bare();
        probe_tmux_priority(&bin, &run_real)
    })
}

#[cfg(test)]
#[path = "doctor_tmux_priority_tests.rs"]
mod tests;
