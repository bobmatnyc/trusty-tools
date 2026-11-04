//! The host-applied OS sandbox, resource limits and spawn evidence
//! (SPEC-OKF-01 §2.16 "OS sandbox", "Limits", owner-choices item 9 B, r7.1).
//!
//! Why: the worker parses untrusted third-party bytes. A parser flaw must
//! not reach the network or the user's files, must not run away with CPU or
//! memory, and the host must know what it applied from its own spawn path,
//! never from the worker's word.
//! What: [`command`] builds the spawn command in two layers, both applied
//! before the worker binary runs.
//! 1. Resource limits (#10746 S10 fix round): `/bin/sh -c 'ulimit -t <cpu>
//!    && ulimit -v <kib> && exec "$@"'` sets `RLIMIT_CPU` and `RLIMIT_AS`,
//!    then execs the next layer, which inherits both. A `ulimit` that fails
//!    ends the shell before `exec`, so the spawn fails before `hello`
//!    (fails closed). macOS does not honour `RLIMIT_AS` (its `setrlimit`
//!    refuses it with `EINVAL`), so on macOS only `RLIMIT_CPU` is set and
//!    the per-item wall time is the memory-runaway bound, as §2.16 states.
//! 2. Under [`SandboxPolicy::Platform`], the platform's sandbox launcher at
//!    a fixed path: macOS `/usr/bin/sandbox-exec` with a deny-default
//!    profile — no network, no `fork`, `exec` only of the worker file (and,
//!    for a `#!` script, its interpreter), reads of `/` itself (dyld aborts
//!    without it), system paths, the worker file and its working directory,
//!    writes to the working directory only; Linux `/usr/bin/bwrap` with every
//!    namespace unshared (no network; only `/usr`, the system library links,
//!    the worker file and the working directory mounted; a `setsid`
//!    grandchild dies with the PID namespace). The launcher execs the worker
//!    only after the sandbox is in place, so a worker that answers `hello`
//!    under it is sandboxed. The evidence names the launcher, or `None` when
//!    the policy is `Off` or the launcher is absent. `PATH` is never
//!    searched.
//!
//! Test: `c145_sandbox_evidence_gates_connector_items_and_blocks_io`,
//! `rlimits_kill_a_runaway_item_and_spare_the_rest`,
//! `sandboxed_worker_cannot_fork_a_setsid_grandchild`.
//!
//! # Spec References
//! - SPEC-OKF-01 §2.16 "OS sandbox", "Limits", "Local-only configuration".

use std::ffi::OsString;
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

/// The shell that applies the resource limits; a fixed path.
const SHELL: &str = "/bin/sh";

#[cfg(target_os = "macos")]
const LAUNCHER: &str = "/usr/bin/sandbox-exec";
#[cfg(target_os = "linux")]
const LAUNCHER: &str = "/usr/bin/bwrap";
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
const LAUNCHER: &str = "";

/// No known launcher on this OS: the evidence is always `None`.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn wrapped(worker: &Path, _cwd: &Path) -> Vec<OsString> {
    vec![worker.into()]
}

/// A path quoted for a macOS sandbox profile string.
#[cfg(target_os = "macos")]
fn quoted(p: &Path) -> String {
    let s = p.to_string_lossy();
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The absolute interpreter of a `#!` script, if `worker` is one.
#[cfg(target_os = "macos")]
fn interpreter(worker: &Path) -> Option<std::path::PathBuf> {
    use std::io::Read;
    let mut head = [0u8; 256];
    let n = std::fs::File::open(worker).ok()?.read(&mut head).ok()?;
    let line = head[..n].strip_prefix(b"#!")?;
    let line = line.split(|b| *b == b'\n').next()?;
    let first = std::str::from_utf8(line).ok()?.split_whitespace().next()?;
    let p = Path::new(first);
    p.is_absolute().then(|| p.to_path_buf())
}

#[cfg(target_os = "macos")]
fn wrapped(worker: &Path, cwd: &Path) -> Vec<OsString> {
    let (w, d) = (quoted(worker), quoted(cwd));
    // #10746 S10 fix: no `process-fork`, and `exec` of the worker (and its
    // interpreter) only, so no grandchild can `setsid` out of the group.
    let interp = interpreter(worker)
        .map(|i| format!(" (literal {})", quoted(&i)))
        .unwrap_or_default();
    let profile = format!(
        "(version 1)\n(deny default)\n(allow process-exec (literal {w}){interp})\n\
         (allow signal (target same-sandbox))\n(allow sysctl-read)\n\
         (allow file-read-metadata)\n\
         (allow file-read* (literal \"/\") (subpath \"/usr\") (subpath \"/System\") (subpath \"/bin\") \
         (subpath \"/private/var/db/dyld\") (subpath \"/dev\") (literal {w}) (subpath {d}))\n\
         (allow file-write* (subpath {d}) (literal \"/dev/null\"))\n"
    );
    vec![LAUNCHER.into(), "-p".into(), profile.into(), worker.into()]
}

#[cfg(target_os = "linux")]
fn wrapped(worker: &Path, cwd: &Path) -> Vec<OsString> {
    let mut c: Vec<OsString> = vec![LAUNCHER.into()];
    let mut push = |args: &[&str]| c.extend(args.iter().map(OsString::from));
    push(&[
        "--unshare-all",
        "--die-with-parent",
        "--ro-bind",
        "/usr",
        "/usr",
    ]);
    for d in ["/bin", "/sbin", "/lib", "/lib32", "/lib64"] {
        let p = Path::new(d);
        match std::fs::read_link(p) {
            Ok(target) => {
                c.push("--symlink".into());
                c.push(target.into());
                c.push(d.into());
            }
            Err(_) if p.is_dir() => c.extend(["--ro-bind", d, d].map(OsString::from)),
            Err(_) => {}
        }
    }
    c.extend(
        [
            "--ro-bind-try",
            "/etc/ld.so.cache",
            "/etc/ld.so.cache",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
        ]
        .map(OsString::from),
    );
    for (flag, p) in [("--ro-bind", worker), ("--bind", cwd)] {
        c.extend([flag.into(), p.into(), p.into()]);
    }
    c.extend(["--chdir".into(), cwd.into(), "--".into(), worker.into()]);
    c
}

/// The `ulimit` script: `RLIMIT_CPU` everywhere, `RLIMIT_AS` where the OS
/// honours it (not macOS); any failure exits before `exec`.
fn limits_script(cpu_seconds: u64, address_space: u64) -> String {
    let as_limit = if cfg!(target_os = "macos") {
        String::new()
    } else {
        format!(" && ulimit -v {}", (address_space / 1024).max(1))
    };
    format!("ulimit -t {}{as_limit} && exec \"$@\"", cpu_seconds.max(1))
}

/// The spawn command for `worker` in `cwd` under the resource limits, and
/// the evidence of the sandbox it applies (see the module doc). Both paths
/// must be canonical.
pub(crate) fn command(
    policy: SandboxPolicy,
    worker: &Path,
    cwd: &Path,
    cpu_seconds: u64,
    address_space: u64,
) -> (Command, SpawnEvidence) {
    let (argv, evidence): (Vec<OsString>, SpawnEvidence) =
        if cfg!(any(target_os = "macos", target_os = "linux"))
            && policy == SandboxPolicy::Platform
            && Path::new(LAUNCHER).is_file()
        {
            (wrapped(worker, cwd), Some(LAUNCHER))
        } else {
            (vec![worker.into()], None)
        };
    let mut c = Command::new(SHELL);
    c.arg("-c")
        .arg(limits_script(cpu_seconds, address_space))
        .arg("okf-worker")
        .args(argv);
    (c, evidence)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the limits must be set before the worker runs, and a failed
    /// `ulimit` must stop the spawn; `RLIMIT_AS` only where honoured.
    /// What: the script chains every `ulimit` with `&&` before `exec`.
    /// Test: self-contained.
    #[test]
    fn limits_script_fails_closed_and_skips_as_on_macos() {
        let s = limits_script(7, 1 << 30);
        assert!(s.starts_with("ulimit -t 7 && "), "{s}");
        assert!(s.ends_with("&& exec \"$@\""), "{s}");
        assert_eq!(s.contains("ulimit -v 1048576"), !cfg!(target_os = "macos"));
    }
}
