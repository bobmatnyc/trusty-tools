//! The host-applied OS sandbox and its spawn evidence (SPEC-OKF-01 §2.16
//! "OS sandbox", owner-choices item 9 B, r7.1).
//!
//! Why: the worker parses untrusted third-party bytes. A parser flaw must
//! not reach the network or the user's files, and the host must know that
//! from its own spawn path, never from the worker's word.
//! What: [`command`] builds the spawn command. Under
//! [`SandboxPolicy::Platform`] it wraps the worker in the platform's
//! sandbox launcher at a fixed path, which applies the sandbox before the
//! worker binary runs: macOS `/usr/bin/sandbox-exec` with a deny-default
//! profile (no network; reads of `/` itself, system paths, the worker file
//! and its working directory only — dyld aborts the process without the
//! root directory read; writes to the working directory only), Linux
//! `/usr/bin/bwrap` with every namespace unshared (no network; only `/usr`,
//! the system library links, the worker file and the working directory
//! mounted). The launcher execs the worker only after the sandbox is in
//! place, so a worker that answers `hello` under it is sandboxed. The
//! returned evidence names the launcher, or `None` when the policy is `Off`
//! or the launcher is absent; a launcher that fails ends the spawn before
//! `hello`, which fails closed. `PATH` is never searched.
//! Test: `c145_sandbox_evidence_gates_connector_items_and_blocks_io`.
//!
//! # Spec References
//! - SPEC-OKF-01 §2.16 "OS sandbox", "Local-only configuration".

use std::path::Path;
use std::process::Command;

/// Whether the host applies an OS sandbox to the worker.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SandboxPolicy {
    /// Apply the platform sandbox when its launcher is installed.
    #[default]
    Platform,
    /// Never apply one; connector-store items are then skipped with
    /// `sandbox_required`.
    Off,
}

/// The host's record of the sandbox it applied: the launcher's name.
pub type SpawnEvidence = Option<&'static str>;

#[cfg(target_os = "macos")]
const LAUNCHER: &str = "/usr/bin/sandbox-exec";
#[cfg(target_os = "linux")]
const LAUNCHER: &str = "/usr/bin/bwrap";

/// A path quoted for a macOS sandbox profile string.
#[cfg(target_os = "macos")]
fn quoted(p: &Path) -> String {
    let s = p.to_string_lossy();
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(target_os = "macos")]
fn wrapped(worker: &Path, cwd: &Path) -> Command {
    let (w, d) = (quoted(worker), quoted(cwd));
    let profile = format!(
        "(version 1)\n(deny default)\n(allow process-fork)\n(allow process-exec)\n\
         (allow signal (target same-sandbox))\n(allow sysctl-read)\n\
         (allow file-read-metadata)\n\
         (allow file-read* (literal \"/\") (subpath \"/usr\") (subpath \"/System\") (subpath \"/bin\") \
         (subpath \"/private/var/db/dyld\") (subpath \"/dev\") (literal {w}) (subpath {d}))\n\
         (allow file-write* (subpath {d}) (literal \"/dev/null\"))\n"
    );
    let mut c = Command::new(LAUNCHER);
    c.arg("-p").arg(profile).arg(worker);
    c
}

#[cfg(target_os = "linux")]
fn wrapped(worker: &Path, cwd: &Path) -> Command {
    let mut c = Command::new(LAUNCHER);
    c.args(["--unshare-all", "--die-with-parent"]);
    c.args(["--ro-bind", "/usr", "/usr"]);
    for d in ["/bin", "/sbin", "/lib", "/lib32", "/lib64"] {
        let p = Path::new(d);
        match std::fs::read_link(p) {
            Ok(target) => {
                c.arg("--symlink").arg(target).arg(d);
            }
            Err(_) if p.is_dir() => {
                c.args(["--ro-bind", d, d]);
            }
            Err(_) => {}
        }
    }
    c.args(["--ro-bind-try", "/etc/ld.so.cache", "/etc/ld.so.cache"]);
    c.args(["--proc", "/proc", "--dev", "/dev"]);
    c.arg("--ro-bind").arg(worker).arg(worker);
    c.arg("--bind").arg(cwd).arg(cwd);
    c.arg("--chdir").arg(cwd);
    c.arg("--").arg(worker);
    c
}

/// The spawn command for `worker` in `cwd`, and the evidence of the sandbox
/// it applies (see the module doc). Both paths must be canonical.
pub(crate) fn command(
    policy: SandboxPolicy,
    worker: &Path,
    cwd: &Path,
) -> (Command, SpawnEvidence) {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if policy == SandboxPolicy::Platform && Path::new(LAUNCHER).is_file() {
        return (wrapped(worker, cwd), Some(LAUNCHER));
    }
    let _ = (policy, cwd);
    (Command::new(worker), None)
}
