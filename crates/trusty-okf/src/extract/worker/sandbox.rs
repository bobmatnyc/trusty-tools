//! The host-applied OS sandbox, resource limits and spawn evidence
//! (SPEC-OKF-01 §2.16 "OS sandbox", "Limits", owner-choices item 9 B, r7.1).
//!
//! Why: the worker parses untrusted third-party bytes. A parser flaw must
//! not reach the network or the user's files, must not run away with CPU or
//! memory, and the host must know what it applied from its own spawn path,
//! never from the worker's word.
//! What: [`command`] builds the spawn command in two layers, both applied
//! before the worker binary runs.
//! 1. Resource limits (#10746 S10 fix round, last change): Linux
//!    `/usr/bin/prlimit --cpu=<s> --as=<bytes> -- …`, elsewhere `/bin/dash
//!    -c 'unset PWD OLDPWD; ulimit -t <s> && exec "$@"'`, sets `RLIMIT_CPU`
//!    and `RLIMIT_AS`, never through `/bin/sh` (see `LIMITER`),
//!    then execs the next layer, which inherits both. A limit that fails
//!    ends the limiter before `exec`, so the spawn fails before `hello`
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
//!    grandchild dies with the PID namespace) and, by `--new-session`, a
//!    session of its own with no controlling terminal (#10746). The launcher execs the worker
//!    only after the sandbox is in place, so a worker that answers `hello`
//!    under it is sandboxed. The evidence names the launcher, or `None` when
//!    the policy is `Off` or the launcher is absent. `PATH` is never
//!    searched.
//!
//! Test: `c145_sandbox_evidence_gates_connector_items_and_blocks_io`,
//! `rlimits_kill_a_runaway_item_and_spare_the_rest`,
//! `worker_env_is_exact_even_where_sh_is_bash`,
//! `sandboxed_worker_cannot_fork_a_setsid_grandchild`,
//! `sandboxed_worker_has_no_controlling_tty`.
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

/// The program that applies the resource limits; a fixed path, never
/// `/bin/sh`. #10746 S10 last change: where `/bin/sh` is bash, bash exports
/// `SHLVL` into the worker's environment even after `unset`, so no shell
/// whose identity varies by host may sit in the spawn path. Linux uses
/// util-linux `prlimit`, which execs the worker with the environment it was
/// given; other systems use `/bin/dash`, which adds nothing once `PWD` is
/// unset. A missing launcher fails the spawn (fails closed).
#[cfg(target_os = "linux")]
const LIMITER: &str = "/usr/bin/prlimit";
#[cfg(not(target_os = "linux"))]
const LIMITER: &str = "/bin/dash";

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
        // #10746: a session of its own, so the worker has no controlling
        // terminal: `/dev/tty` cannot be opened and no TIOCSTI can reach the
        // operator's shell (CVE-2017-5226).
        "--new-session",
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

/// The arguments that set `RLIMIT_CPU` everywhere and `RLIMIT_AS` where
/// the OS honours it (not macOS), then exec the rest of the command line.
/// A limit that cannot be set ends the spawn before `exec`.
fn limits_prefix(cpu_seconds: u64, address_space: u64) -> Vec<OsString> {
    let cpu = cpu_seconds.max(1);
    if cfg!(target_os = "linux") {
        vec![
            LIMITER.into(),
            format!("--cpu={cpu}").into(),
            format!("--as={}", address_space.max(1)).into(),
            "--".into(),
        ]
    } else {
        // The shell's own `PWD` must not reach the worker (§2.16 allow-list).
        vec![
            LIMITER.into(),
            "-c".into(),
            format!("unset PWD OLDPWD; ulimit -t {cpu} && exec \"$@\"").into(),
            "okf-worker".into(),
        ]
    }
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
    let prefix = limits_prefix(cpu_seconds, address_space);
    let mut c = Command::new(&prefix[0]);
    c.args(&prefix[1..]).args(argv);
    (c, evidence)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the limits must be set before the worker runs, by a fixed
    /// program; `RLIMIT_AS` only where honoured.
    /// What: Linux names `prlimit` with both limits; elsewhere `dash` chains
    /// the `ulimit` with `&&` before `exec`.
    /// Test: self-contained.
    #[test]
    fn limits_prefix_names_a_fixed_limiter() {
        let p: Vec<String> = limits_prefix(7, 1 << 30)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(p[0], LIMITER);
        if cfg!(target_os = "linux") {
            assert_eq!(&p[1..], ["--cpu=7", "--as=1073741824", "--"]);
        } else {
            assert_eq!(p[2], "unset PWD OLDPWD; ulimit -t 7 && exec \"$@\"");
        }
    }

    /// Why: #10746 S10 last change — the worker's environment must equal
    /// the allow-list whatever `/bin/sh` is; bash exports `SHLVL` even after
    /// `unset`.
    /// What: the real spawn command (cleared environment plus the
    /// allow-list) runs a script that prints its environment. Where the
    /// command goes through `/bin/sh`, it runs under `/bin/bash` instead, as
    /// on a host whose `/bin/sh` is bash.
    /// Test: self-contained.
    #[test]
    fn worker_env_is_exact_even_where_sh_is_bash() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path().canonicalize().unwrap();
        let script = dir.join("print-vars");
        std::fs::write(
            &script,
            "#!/usr/bin/perl\nprint map { \"$_\\n\" } sort keys %ENV;\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (cmd, _) = command(SandboxPolicy::Off, &script, &dir, 30, 1 << 32);
        let program = if cmd.get_program() == "/bin/sh" && Path::new("/bin/bash").is_file() {
            Path::new("/bin/bash").as_os_str()
        } else {
            cmd.get_program()
        };
        let out = Command::new(program)
            .args(cmd.get_args())
            .env_clear()
            .envs(super::super::process::WORKER_ENV)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "LANG\nTZ\n");
    }
}
