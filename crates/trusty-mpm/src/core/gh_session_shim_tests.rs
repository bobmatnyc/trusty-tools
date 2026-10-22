//! Tests for the #9750 gh credential shim. The shim is run against a fake
//! `gh` script in a temp dir; no test runs the real gh, reads a keyring or
//! sends a request.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::{
    GH_SHIM_ENV, LaunchSeams, PinnedCredential, find_real_gh, launch_pinned_login, prefixed_path,
    shim_dir, shim_root, shim_script, write_gh_shim,
};
use crate::core::gh_account::PinnedGhIdentity;
use crate::core::gh_account_dir::AccountDirSources;
use crate::core::gh_account_dir::gh_account_dir_tests::{TableCheck, TableProbe, migrated_dir};
use crate::core::gh_login_verify::VerifiedLogin;
use crate::core::gh_shim_path::apply_gh_path;
use crate::session_manager::ssh_host_alias::SshHostAliases;

/// A planted token: no shim, env, message or log may ever contain it.
const SENTINEL: &str = "ghp_9750ShimSentinelNeverPrinted";

/// Every variable gh reads a token from.
const TOKEN_VARS: [&str; 4] = [
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
];

/// `octo` verified on `host` under `dir`.
fn verified(host: &str, dir: &Path) -> VerifiedLogin {
    VerifiedLogin {
        login: "octo".into(),
        host: host.into(),
        config_dir: dir.to_path_buf(),
    }
}

/// The value `vars` sets for `name`, if any.
fn value_of<'a>(vars: &'a [(String, String)], name: &str) -> Option<&'a str> {
    vars.iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// #9750 (ruling D7c): `vars` sets no token variable, not even the nobody
/// sentinel.
fn assert_no_token_value(vars: &[(String, String)]) {
    for var in TOKEN_VARS {
        assert_eq!(value_of(vars, var), None, "{var}: {vars:?}");
    }
}

/// Write an executable script.
///
/// A child `cp` writes the executable, so no thread of this process ever holds
/// it open for writing.
#[cfg(unix)]
fn write_script(path: &Path, body: &str) {
    let parent = path.parent().expect("parent");
    std::fs::create_dir_all(parent).expect("dir");
    let name = path.file_name().expect("file name").to_string_lossy();
    let staged = parent.join(format!(".{name}.src"));
    std::fs::write(&staged, body).expect("stage the script");
    // #10176: a sibling test thread's fork inherits any write fd this process
    // holds and keeps it until that child execs, so exec'ing a script written
    // here can fail with ETXTBSY. Same pattern as #9718.
    let installed = std::process::Command::new("/bin/sh")
        .args(["-c", "cp \"$1\" \"$2\" && chmod 755 \"$2\"", "sh"])
        .arg(&staged)
        .arg(path)
        .status()
        .expect("install the script");
    assert!(installed.success(), "install the script: {installed}");
    std::fs::remove_file(&staged).expect("remove the staged script");
}

/// `cmd.output()`, retried while the exec fails with ETXTBSY: every 20 ms, at
/// most 100 times.
///
/// Why: `write_gh_shim` writes the shim in this process. A sibling test
/// thread that forks during that write hands its child a copy of the write fd,
/// and the kernel refuses to exec the shim until that child execs or exits.
/// No test can stop that fork, so the exec is retried within a bound.
#[cfg(unix)]
fn output_past_etxtbsy(cmd: &mut std::process::Command) -> std::process::Output {
    for _ in 0..100 {
        match cmd.output() {
            // #10176: a sibling test's forked child holds the shim's write fd
            // only until it execs; ETXTBSY comes from the exec alone, so a
            // retry never re-runs a command that started.
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            other => return other.expect("the shim runs"),
        }
    }
    panic!("the shim stayed busy (ETXTBSY) for 2 s");
}

/// #9750: a shim credential sets no token variable — only the shim dir — and
/// makes the shim git's credential helper.
#[test]
fn a_shim_credential_carries_no_token_and_names_the_shim_helper() {
    let cred = PinnedCredential::shim(Path::new("/state/gh-shim/octo@github.com-1"));
    assert_no_token_value(&cred.vars);
    assert_eq!(
        value_of(&cred.vars, GH_SHIM_ENV),
        Some("/state/gh-shim/octo@github.com-1")
    );
    assert_eq!(
        cred.helper,
        "!'/state/gh-shim/octo@github.com-1/gh' auth git-credential"
    );
}

/// #9750: the shim dir is first on `PATH`, ahead of every inherited entry.
#[test]
fn the_shim_dir_is_first_on_path() {
    let path = prefixed_path(OsStr::new("/shim"), Some(OsStr::new("/usr/bin:/bin")));
    assert_eq!(path, "/shim:/usr/bin:/bin");
    assert_eq!(prefixed_path(OsStr::new("/shim"), None), "/shim");
}

/// #9750: only a launch naming a shim dir gets a `PATH` override.
#[test]
fn apply_shim_path_prefixes_only_a_shimmed_launch() {
    let path_of = |cmd: &std::process::Command| {
        cmd.get_envs()
            .find(|(k, _)| *k == "PATH")
            .and_then(|(_, v)| v)
            .map(|v| v.to_string_lossy().into_owned())
    };
    let mut plain = std::process::Command::new("true");
    apply_gh_path(&mut plain, &[("GH_USER".into(), "octo".into())], &[]);
    assert_eq!(path_of(&plain), None);

    let mut shimmed = std::process::Command::new("true");
    apply_gh_path(&mut shimmed, &[(GH_SHIM_ENV.into(), "/shim".into())], &[]);
    let path = path_of(&shimmed).expect("PATH is set");
    assert!(
        path == "/shim" || path.starts_with("/shim:"),
        "the shim dir must lead PATH: {path}"
    );
}

/// #9750: the real gh is found outside the shim root, so the shim never
/// `exec`s itself.
#[cfg(unix)]
#[test]
fn find_real_gh_skips_the_shim_root() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = shim_root(tmp.path());
    let shimmed = root.join("octo@github.com-1");
    let real = tmp.path().join("bin");
    write_script(&shimmed.join("gh"), "#!/bin/sh\n");
    write_script(&real.join("gh"), "#!/bin/sh\n");
    let path = std::env::join_paths([&shimmed, &real]).expect("PATH");
    assert_eq!(find_real_gh(Some(&path), &root), Ok(real.join("gh")));
}

/// #9750: gh absent is a named refusal, never a guessed path.
#[test]
fn find_real_gh_reports_a_missing_gh() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = std::env::join_paths([tmp.path()]).expect("PATH");
    let err = find_real_gh(Some(&path), &shim_root(tmp.path())).expect_err("no gh");
    assert!(err.contains("gh is not installed"), "{err}");
    assert!(find_real_gh(None, tmp.path()).is_err());
}

/// 🔴 #9750: the shim names the login, `--user` and the verified dir, and no
/// token-shaped string.
#[test]
fn the_shim_script_names_the_login_and_never_a_token() {
    let script = shim_script(
        &verified("github.com", Path::new("/home/me/.config/gh")),
        Path::new("/opt/homebrew/bin/gh"),
        5,
    )
    .expect("script");
    assert!(script.starts_with("#!/bin/sh\n"), "{script}");
    assert!(
        script.contains("auth token --hostname 'github.com' --user 'octo'"),
        "{script}"
    );
    assert!(
        script.contains("GH_CONFIG_DIR='/home/me/.config/gh'"),
        "{script}"
    );
    assert!(script.contains("real='/opt/homebrew/bin/gh'"), "{script}");
    assert!(script.contains("export GH_TOKEN\n"), "{script}");
    for shape in ["ghp_", "gho_", "github_pat_", SENTINEL] {
        assert!(!script.contains(shape), "{shape} in {script}");
    }
}

/// #8510 HIGH kept: gh on an Enterprise Server reads `GH_ENTERPRISE_TOKEN`.
#[test]
fn a_ghes_shim_exports_gh_enterprise_token() {
    let script = shim_script(
        &verified("ghe.corp", Path::new("/gh")),
        Path::new("/bin/gh"),
        5,
    )
    .expect("script");
    assert!(script.contains("export GH_ENTERPRISE_TOKEN\n"), "{script}");
    assert!(!script.contains("export GH_TOKEN\n"), "{script}");
}

/// A value that could end the script's quotes is refused, never written.
#[test]
fn the_shim_refuses_a_login_it_cannot_quote() {
    let mut bad = verified("github.com", Path::new("/gh"));
    bad.login = "octo'; rm -rf ~; '".into();
    assert!(shim_script(&bad, Path::new("/bin/gh"), 5).is_err());
    let dir = verified("github.com", Path::new("/it's/gh"));
    assert!(shim_script(&dir, Path::new("/bin/gh"), 5).is_err());
    let leading_dash = VerifiedLogin {
        login: "-u".into(),
        ..verified("github.com", Path::new("/gh"))
    };
    assert!(shim_script(&leading_dash, Path::new("/bin/gh"), 5).is_err());
}

/// Two pins for one login on one host but different dirs never share a shim.
#[test]
fn two_config_dirs_get_two_shims() {
    let state = Path::new("/state");
    let a = shim_dir(state, &verified("github.com", Path::new("/a")));
    let b = shim_dir(state, &verified("github.com", Path::new("/b")));
    assert_ne!(a, b);
    assert!(
        a.to_string_lossy()
            .starts_with("/state/gh-shim/octo@github.com-"),
        "{a:?}"
    );
}

/// 🔴 #9750: the script is 0755 inside a 0700 dir under a 0700 root.
#[cfg(unix)]
#[test]
fn the_shim_is_0755_inside_a_0700_dir() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = write_gh_shim(
        tmp.path(),
        &verified("github.com", Path::new("/gh")),
        Path::new("/bin/gh"),
        5,
    )
    .expect("written");
    let mode = |p: &Path| p.metadata().expect("meta").permissions().mode() & 0o777;
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&shim_root(tmp.path())), 0o700);
    assert_eq!(mode(&dir.join("gh")), 0o755);
}

/// A fake real gh: `auth token` prints a token for its `--hostname`/`--user`
/// only when no token variable is inherited and `GH_CONFIG_DIR` is `dir`;
/// every other call prints the token variables it received and its argv.
#[cfg(unix)]
fn fake_gh(path: &Path, dir: &Path, token_step: &str) {
    let body = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = auth ] && [ \"$2\" = token ]; then\n\
         \x20 if [ -n \"$GH_TOKEN$GITHUB_TOKEN$GH_ENTERPRISE_TOKEN$GITHUB_ENTERPRISE_TOKEN\" ]; then echo inherited >&2; exit 9; fi\n\
         \x20 [ \"$GH_CONFIG_DIR\" = '{}' ] || {{ echo \"wrong dir\" >&2; exit 8; }}\n\
         \x20 {token_step}\n\
         fi\n\
         printf 'GH_TOKEN=%s GITHUB_TOKEN=%s ARGS=%s\\n' \"$GH_TOKEN\" \"$GITHUB_TOKEN\" \"$*\"\n",
        dir.display()
    );
    write_script(path, &body);
}

/// Run `<shim_dir>/gh api user` with a stale inherited token.
#[cfg(unix)]
fn run_shim(shim_dir: &Path) -> std::process::Output {
    let mut cmd = std::process::Command::new(shim_dir.join("gh"));
    cmd.args(["api", "user"])
        .env("GH_TOKEN", "stale-inherited")
        .env("GITHUB_TOKEN", "stale-inherited");
    // #10176: the shim is written in this process; see `output_past_etxtbsy`.
    output_past_etxtbsy(&mut cmd)
}

/// 🔴 #9750: run for real against a fake gh, the shim asks for the pinned
/// login's token with every inherited token removed, and hands only that
/// token to the one gh process it `exec`s.
#[cfg(unix)]
#[test]
fn a_written_shim_runs_gh_with_the_pinned_token_only() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let gh_dir = tmp.path().join("gh-config");
    let real = tmp.path().join("bin").join("gh");
    fake_gh(&real, &gh_dir, "echo \"tok-for-$4-$6\"; exit 0");
    let dir =
        write_gh_shim(tmp.path(), &verified("github.com", &gh_dir), &real, 5).expect("written");
    let out = run_shim(&dir);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "GH_TOKEN=tok-for-github.com-octo GITHUB_TOKEN= ARGS=api user\n"
    );
}

/// 🔴 #9750: no keyring login — the shim exits 1 with the fix and never runs
/// the command as anyone.
#[cfg(unix)]
#[test]
fn a_shim_with_no_keyring_login_exits_1_with_the_fix() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let gh_dir = tmp.path().join("gh-config");
    let real = tmp.path().join("bin").join("gh");
    fake_gh(&real, &gh_dir, "echo 'no oauth token found' >&2; exit 1");
    let dir =
        write_gh_shim(tmp.path(), &verified("github.com", &gh_dir), &real, 5).expect("written");
    let out = run_shim(&dir);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(out.stdout.is_empty(), "the command ran: {out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("gh auth login --hostname github.com (as octo)"),
        "{stderr}"
    );
}

/// 🔴 #9750 R4: a token lookup that hangs (a locked keychain) is killed at the
/// bound and refused, instead of hanging every gh call.
#[cfg(unix)]
#[test]
fn a_hung_token_lookup_is_killed_and_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let gh_dir = tmp.path().join("gh-config");
    let real = tmp.path().join("bin").join("gh");
    fake_gh(&real, &gh_dir, "exec sleep 30");
    let dir =
        write_gh_shim(tmp.path(), &verified("github.com", &gh_dir), &real, 1).expect("written");
    let started = std::time::Instant::now();
    let out = run_shim(&dir);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(15),
        "the bound did not hold: {:?}",
        started.elapsed()
    );
}

/// A forked child holding a write fd to one file, as a sibling test thread's
/// child holds an inherited fd between its fork and its exec (#10176).
#[cfg(unix)]
struct ForkedWriter(libc::pid_t);

#[cfg(unix)]
impl ForkedWriter {
    /// Open `path` for writing, fork a child that keeps the fd for 300 ms and
    /// exits, then close this process's copy.
    fn hold(path: &Path) -> Self {
        let file = std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .expect("open for writing");
        let pause = libc::timespec {
            tv_sec: 0,
            tv_nsec: 300_000_000,
        };
        // SAFETY: the child calls only `nanosleep` and `_exit`, both
        // async-signal-safe, so forking a multi-threaded process is sound.
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            // SAFETY: as above.
            unsafe {
                libc::nanosleep(&pause, std::ptr::null_mut());
                libc::_exit(0);
            }
        }
        assert!(pid > 0, "fork: {}", std::io::Error::last_os_error());
        drop(file);
        Self(pid)
    }
}

#[cfg(unix)]
impl Drop for ForkedWriter {
    fn drop(&mut self) {
        let mut status = 0;
        // SAFETY: reaps the one child `hold` forked.
        unsafe { libc::waitpid(self.0, &mut status, 0) };
    }
}

/// 🔴 #10176: a sibling test thread that forks while `write_gh_shim` holds the
/// shim open for writing gives its child a copy of that write fd, which the
/// child keeps until it execs or exits. Exec'ing the shim in that window
/// failed with ETXTBSY. This forces the window — a forked child holds the
/// shim's write fd for 300 ms — and the shim must still run.
///
/// The body runs in a single-threaded child process of this test binary, so
/// the fork copies no sibling test's write fd and cannot make it busy.
/// Test: itself.
#[cfg(unix)]
#[test]
fn a_shim_held_busy_by_a_forked_writer_still_runs() {
    const CHILD_RUN: &str = "TM_TEST_10176_FORKED_WRITER";
    if std::env::var_os(CHILD_RUN).is_none() {
        let module = module_path!();
        let module = module.split_once("::").map_or(module, |(_, rest)| rest);
        let name = format!("{module}::a_shim_held_busy_by_a_forked_writer_still_runs");
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                name.as_str(),
                "--exact",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env(CHILD_RUN, "1")
            .output()
            .expect("re-run this test as a child process");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "child run failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let gh_dir = tmp.path().join("gh-config");
    let real = tmp.path().join("bin").join("gh");
    fake_gh(&real, &gh_dir, "echo \"tok-for-$4-$6\"; exit 0");
    let dir =
        write_gh_shim(tmp.path(), &verified("github.com", &gh_dir), &real, 5).expect("written");
    let writer = ForkedWriter::hold(&dir.join("gh"));
    // Linux refuses the exec while the child holds the fd; the window is armed.
    #[cfg(target_os = "linux")]
    assert_eq!(
        std::process::Command::new(dir.join("gh"))
            .output()
            .err()
            .and_then(|e| e.raw_os_error()),
        Some(libc::ETXTBSY),
        "the forked writer must make the shim busy"
    );
    let out = run_shim(&dir);
    drop(writer);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "GH_TOKEN=tok-for-github.com-octo GITHUB_TOKEN= ARGS=api user\n"
    );
}

/// The launch seams over one migrated candidate dir under `root`, whose
/// `--user octo` lookup answers the sentinel and whose `GET /user` answers `who`.
fn launch_fixture(
    root: &Path,
    who: Result<&str, &str>,
) -> (AccountDirSources, TableProbe, TableCheck, PathBuf) {
    let dir = migrated_dir(&root.join("gh"));
    let probe = TableProbe::default().answer(&dir, "github.com", "octo", Ok(SENTINEL));
    let check = TableCheck::default().answer("https://api.github.com", SENTINEL, who);
    let sources = AccountDirSources {
        own_config_dir: Some(dir.clone()),
        ..Default::default()
    };
    (sources, probe, check, dir)
}

/// 🔴 #9750: a verified login launches with the shim, and neither the
/// credential nor the shim file holds the token.
#[test]
fn a_verified_login_launches_with_the_shim_and_no_token() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (sources, probe, check, dir) = launch_fixture(tmp.path(), Ok("octo"));
    let seams = LaunchSeams {
        sources: &sources,
        probe: &probe,
        check: &check,
        real_gh: Ok(PathBuf::from("/bin/gh")),
        state_root: tmp.path(),
    };
    let cred = launch_pinned_login("octo", "github.com", seams).expect("launched");
    let shim = PathBuf::from(value_of(&cred.vars, GH_SHIM_ENV).expect("shim dir"));
    assert_eq!(shim, shim_dir(tmp.path(), &verified("github.com", &dir)));
    assert_no_token_value(&cred.vars);
    let script = std::fs::read_to_string(shim.join("gh")).expect("shim");
    assert!(!script.contains(SENTINEL), "the token reached the shim");
    assert!(!format!("{cred:?}").contains(SENTINEL));
}

/// 🔴 #9750: an unverified login writes no shim and the refusal names no token.
#[test]
fn an_unverified_login_writes_no_shim() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (sources, probe, check, _dir) = launch_fixture(tmp.path(), Ok("octo-other"));
    let seams = LaunchSeams {
        sources: &sources,
        probe: &probe,
        check: &check,
        real_gh: Ok(PathBuf::from("/bin/gh")),
        state_root: tmp.path(),
    };
    let err = launch_pinned_login("octo", "github.com", seams).expect_err("refused");
    assert!(err.contains("authenticates as 'octo-other'"), "{err}");
    assert!(!err.contains(SENTINEL), "{err}");
    assert!(!shim_root(tmp.path()).exists(), "a shim was written");
}

/// #9750: gh absent refuses the launch after the login is verified.
#[test]
fn a_missing_gh_refuses_the_launch() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (sources, probe, check, _dir) = launch_fixture(tmp.path(), Ok("octo"));
    let seams = LaunchSeams {
        sources: &sources,
        probe: &probe,
        check: &check,
        real_gh: Err("gh is not installed on this PATH, so no gh call can run".into()),
        state_root: tmp.path(),
    };
    let err = launch_pinned_login("octo", "github.com", seams).expect_err("refused");
    assert!(err.contains("gh is not installed"), "{err}");
}

/// A `tracing` writer that appends every formatted line to a shared buffer.
#[derive(Clone, Default)]
struct LogBuffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// 🔴 #9750: a full spawn — verify, shim, env, the production logger — for a
/// config-dir pin and an account-only pin, under a capturing subscriber: no
/// token reaches the log or the env, in the accepted and the refused arm; the
/// refusal is still logged.
#[test]
fn a_shimmed_spawn_logs_and_injects_no_token() {
    let arms = [(Ok("octo"), true), (Ok("octo-other"), true)];
    let arms = arms
        .into_iter()
        .chain([(Ok("octo"), false), (Ok("octo-other"), false)]);
    for (who, with_dir) in arms {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (sources, probe, check, dir) = launch_fixture(tmp.path(), who);
        let pinned = PinnedGhIdentity {
            account: Some("octo".into()),
            config_dir: with_dir.then(|| dir.clone()),
            ..Default::default()
        };
        // #4931: a thread-local capture records nothing until the global level is raised.
        crate::test_support::enable_event_capture();
        let buffer = LogBuffer::default();
        let writer = buffer.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        let vars = tracing::subscriber::with_default(subscriber, || {
            let env = crate::core::gh_session_account::session_spawn_env(
                &pinned,
                "https://github.com/acme-9750/widget",
                tmp.path(),
                &SshHostAliases::empty(),
                |login| {
                    let seams = LaunchSeams {
                        sources: &sources,
                        probe: &probe,
                        check: &check,
                        real_gh: Ok(PathBuf::from("/bin/gh")),
                        state_root: tmp.path(),
                    };
                    launch_pinned_login(login, "github.com", seams)
                },
            );
            crate::core::gh_account::log_spawn_env(env, Path::new("/work/widget"))
        });
        let log = String::from_utf8(buffer.0.lock().expect("log").clone()).expect("utf-8");
        assert!(!log.contains(SENTINEL), "a token reached the log: {log}");
        assert!(
            !vars.iter().any(|(_, v)| v.contains(SENTINEL)),
            "a token reached the env: {vars:?}"
        );
        if who == Ok("octo-other") {
            assert!(log.contains("authenticates as 'octo-other'"), "{log}");
        } else {
            assert_no_token_value(&vars);
        }
    }
}

/// 🔴 #9750: an account-only pin (registry `gh_account`, `[accounts]`,
/// `--account`) pins git's credential helper for the origin's host to the
/// shim, cleared first so no earlier helper answers — before this, a global
/// `!gh auth git-credential` answered as the keyring's active account.
#[test]
fn a_shimmed_account_only_spawn_pins_git_to_the_shim() {
    let pinned = PinnedGhIdentity {
        account: Some("octo".into()),
        ..Default::default()
    };
    let shim = Path::new("/state/gh-shim/octo@github.com-0");
    let env = crate::core::gh_session_account::session_spawn_env(
        &pinned,
        "https://github.com/acme-9750/widget",
        Path::new("/state"),
        &SshHostAliases::empty(),
        |_| Ok(PinnedCredential::shim(shim)),
    )
    .expect("a pin")
    .expect("an env");
    let key = "credential.https://github.com.helper";
    assert_eq!(value_of(&env.vars, "GIT_CONFIG_COUNT"), Some("2"));
    assert_eq!(value_of(&env.vars, "GIT_CONFIG_KEY_0"), Some(key));
    assert_eq!(value_of(&env.vars, "GIT_CONFIG_VALUE_0"), Some(""));
    assert_eq!(value_of(&env.vars, "GIT_CONFIG_KEY_1"), Some(key));
    assert_eq!(
        value_of(&env.vars, "GIT_CONFIG_VALUE_1"),
        Some("!'/state/gh-shim/octo@github.com-0/gh' auth git-credential")
    );
    assert_no_token_value(&env.vars);
    assert!(env.warning.is_none(), "{:?}", env.warning);
}

/// 🔴 #9750 (ruling D7c): inside a pinned session, a gh that bypasses the
/// shim (run by absolute path) receives no token at all: the pane's tokens
/// are removed and no placeholder replaces them. Tested on the env the
/// relaunched session actually receives.
#[cfg(unix)]
#[test]
fn a_gh_run_outside_the_shim_gets_no_token_in_a_pinned_session() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let real = tmp.path().join("bin").join("gh");
    write_script(&real, "#!/bin/sh\nprintf 'GH_TOKEN=%s\\n' \"$GH_TOKEN\"\n");
    let pinned = PinnedGhIdentity {
        account: Some("octo".into()),
        ..Default::default()
    };
    let shim = tmp.path().join("gh-shim").join("octo@github.com-0");
    let env = super::pinned_relaunch_env_with(
        &pinned,
        "https://github.com/acme-9750/widget",
        tmp.path(),
        &SshHostAliases::empty(),
        |_| Ok(PinnedCredential::shim(&shim)),
    )
    .expect("a verified relaunch");
    let mut cmd = std::process::Command::new(&real);
    cmd.env("GH_TOKEN", "pane-token")
        .env("GITHUB_TOKEN", "pane-token");
    env.apply_to(&mut cmd);
    let out = cmd.output().expect("gh runs");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "GH_TOKEN=\n");
}

/// 🔴 #9750 (ruling D7c, #8383 global tier): inside a pinned session, a `tm`
/// gh call bound by the global `config_dir` fallback — which removes nothing,
/// so the caller's credential decides — runs through the session's shim and
/// reaches gh with the pinned account's token. At 29eb6600d8 the bare config
/// dir dropped the shim and gh sent the session's nobody sentinel: HTTP 401.
#[cfg(unix)]
#[test]
fn a_global_tier_gh_call_in_a_pinned_session_resolves_through_the_shim() {
    use crate::core::gh_shim_path::{GhPathEnv, apply_gh_path_with};
    let tmp = tempfile::tempdir().expect("tempdir");
    let gh_dir = tmp.path().join("gh-config");
    let real = tmp.path().join("bin").join("gh");
    fake_gh(&real, &gh_dir, "echo \"tok-for-$4-$6\"; exit 0");
    let shim =
        write_gh_shim(tmp.path(), &verified("github.com", &gh_dir), &real, 5).expect("written");
    // Session A's PATH: its shim, the fake real gh, then `env`/`sleep`.
    let rest = format!("{}:/usr/bin:/bin", real.parent().expect("bin").display());
    let session_path = prefixed_path(shim.as_os_str(), Some(OsStr::new(&rest)));
    let global = crate::core::trusty_tools_config::GithubConfig {
        config_dir: Some(PathBuf::from("/global/gh")),
        ..Default::default()
    };
    let tiered = crate::core::gh_identity::resolve_tiered_gh_env(None, Some(&global))
        .expect("the global tier resolves");
    let mut cmd = std::process::Command::new("gh");
    cmd.args(["api", "user"]).env("PATH", &session_path);
    for var in TOKEN_VARS {
        cmd.env_remove(var);
    }
    // The session's own environment, as its spawn env sets it.
    for (key, value) in &PinnedCredential::shim(&shim).vars {
        cmd.env(key, value);
    }
    // The `tm gh` binding on top, as `GhEnv::apply_to` applies it.
    for key in tiered.unset_vars() {
        cmd.env_remove(key);
    }
    for (key, value) in tiered.vars() {
        cmd.env(key, value);
    }
    let session = GhPathEnv {
        path: Some(session_path.clone()),
        shim: Some(shim.as_os_str().to_os_string()),
        shim_root: shim_root(tmp.path()),
    };
    apply_gh_path_with(&mut cmd, tiered.vars(), tiered.unset_vars(), &session);
    // #10176: PATH resolves `gh` to the shim this process wrote.
    let out = output_past_etxtbsy(&mut cmd);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "GH_TOKEN=tok-for-github.com-octo GITHUB_TOKEN= ARGS=api user\n"
    );
}

/// 🔴 #9750: a relaunch in a checkout whose origin git cannot read (an
/// unreadable `.git`) is refused — never relaunched as unpinned.
#[cfg(unix)]
#[test]
fn a_relaunch_whose_origin_git_cannot_read_is_refused() {
    let checkout = unreadable_checkout();
    let result = super::pinned_relaunch_env(checkout.path());
    restore_checkout(checkout.path());
    let err = result.expect_err("refused");
    assert!(err.contains("git cannot read the origin"), "{err}");
    assert!(err.ends_with("No token was injected."), "{err}");
}

/// A git checkout with an `origin`, whose `.git` is then made unreadable.
#[cfg(unix)]
pub(crate) fn unreadable_checkout() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    for args in [
        &["init", "-q"][..],
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme-9750/widget",
        ][..],
    ] {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(args)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?}");
    }
    let git_dir = dir.path().join(".git");
    std::fs::set_permissions(&git_dir, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    dir
}

/// Undo [`unreadable_checkout`]'s chmod so the temp dir can be removed.
#[cfg(unix)]
pub(crate) fn restore_checkout(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let git_dir = dir.join(".git");
    std::fs::set_permissions(&git_dir, std::fs::Permissions::from_mode(0o755)).expect("restore");
}

/// 🔴 #9750 Q1: a pinned in-place relaunch clears every inherited token
/// variable and hands `claude` the shim, first on `PATH`.
#[test]
fn a_pinned_relaunch_env_clears_inherited_tokens_and_adds_the_shim() {
    let pinned = PinnedGhIdentity {
        account: Some("octo".into()),
        ..Default::default()
    };
    let shim = Path::new("/state/gh-shim/octo@github.com-0");
    let env = super::pinned_relaunch_env_with(
        &pinned,
        "https://github.com/acme-9750/widget",
        Path::new("/state"),
        &SshHostAliases::empty(),
        |_| Ok(PinnedCredential::shim(shim)),
    )
    .expect("a verified relaunch");
    // #9750 (ruling D7c): each inherited token is removed, none replaced.
    for var in TOKEN_VARS {
        assert!(env.unset_vars().iter().any(|v| v == var), "{var}: {env:?}");
    }
    assert_no_token_value(env.vars());
    let mut cmd = std::process::Command::new("true");
    env.apply_to(&mut cmd);
    let path = cmd
        .get_envs()
        .find(|(k, _)| *k == "PATH")
        .and_then(|(_, v)| v)
        .map(|v| v.to_string_lossy().into_owned())
        .expect("PATH is set");
    assert!(
        path.starts_with("/state/gh-shim/octo@github.com-0"),
        "{path}"
    );
}

/// 🔴 #9750 Q1: a relaunch whose login is not proven is refused with the
/// plan's message — never relaunched with the nobody-token.
#[test]
fn a_refused_pinned_relaunch_is_the_launch_refusal() {
    let pinned = PinnedGhIdentity {
        account: Some("octo".into()),
        ..Default::default()
    };
    let err = super::pinned_relaunch_env_with(
        &pinned,
        "https://github.com/acme-9750/widget",
        Path::new("/state"),
        &SshHostAliases::empty(),
        |_| Err("printed no token".into()),
    )
    .expect_err("refused");
    assert_eq!(
        err,
        crate::core::gh_login_verify::launch_refusal("octo", "github.com", "printed no token")
    );
}
