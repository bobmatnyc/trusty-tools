//! Tests for the #9750 gh credential shim. The shim is run against a fake
//! `gh` script in a temp dir; no test runs the real gh, reads a keyring or
//! sends a request.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::{
    GH_SHIM_ENV, LaunchSeams, PinnedCredential, apply_shim_path, find_real_gh, launch_pinned_login,
    prefixed_path, shim_dir, shim_root, shim_script, write_gh_shim,
};
use crate::core::gh_account::PinnedGhIdentity;
use crate::core::gh_account_dir::AccountDirSources;
use crate::core::gh_account_dir::gh_account_dir_tests::{TableCheck, TableProbe, migrated_dir};
use crate::core::gh_login_verify::VerifiedLogin;
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

/// Write an executable script.
#[cfg(unix)]
fn write_script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
    std::fs::write(path, body).expect("script");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// #9750: a shim credential sets no token variable, only the shim dir, and
/// makes the shim git's credential helper.
#[test]
fn a_shim_credential_carries_no_token_and_names_the_shim_helper() {
    let cred = PinnedCredential::shim(Path::new("/state/gh-shim/octo@github.com-1"));
    for var in TOKEN_VARS {
        assert!(value_of(&cred.vars, var).is_none(), "{var}: {cred:?}");
    }
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
    apply_shim_path(&mut plain, &[("GH_USER".into(), "octo".into())]);
    assert_eq!(path_of(&plain), None);

    let mut shimmed = std::process::Command::new("true");
    apply_shim_path(&mut shimmed, &[(GH_SHIM_ENV.into(), "/shim".into())]);
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
    std::process::Command::new(shim_dir.join("gh"))
        .args(["api", "user"])
        .env("GH_TOKEN", "stale-inherited")
        .env("GITHUB_TOKEN", "stale-inherited")
        .output()
        .expect("the shim runs")
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
    for var in TOKEN_VARS {
        assert!(value_of(&cred.vars, var).is_none(), "{var}: {cred:?}");
    }
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

/// 🔴 #9750: a full config-dir pin spawn — verify, shim, env, the production
/// logger — under a capturing subscriber: no token reaches the log or the
/// env, in the accepted and the refused arm; the refusal is still logged.
#[test]
fn a_shimmed_spawn_logs_and_injects_no_token() {
    for who in [Ok("octo"), Ok("octo-other")] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (sources, probe, check, dir) = launch_fixture(tmp.path(), who);
        let pinned = PinnedGhIdentity {
            account: Some("octo".into()),
            config_dir: Some(dir.clone()),
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
            for var in TOKEN_VARS {
                assert!(value_of(&vars, var).is_none(), "{var}: {vars:?}");
            }
        }
    }
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
    for var in TOKEN_VARS {
        assert!(env.unset_vars().iter().any(|v| v == var), "{var}: {env:?}");
        assert!(value_of(env.vars(), var).is_none(), "{var}: {env:?}");
    }
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
