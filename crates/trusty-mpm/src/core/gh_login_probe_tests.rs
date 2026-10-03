//! Tests for [`super`] — the daemon's `gh` login probe (#9091).
//!
//! Every test stands in a fake `gh` through the [`AuthStatusRunner`] seam: no
//! `PATH` change, no subprocess, no network. The fake answers per config dir,
//! which models the launchd daemon: no `GH_CONFIG_DIR`, so gh's default dir
//! holds no account while the operator's dirs hold them.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use trusty_common::gh::GhOutput;

use super::{AuthStatusRunner, classify_auth_status, probe_gh_login_with};
use crate::core::gh_account::GhAuthProbe;
use crate::core::gh_account_dir::{AccountDirSources, tm_account_dir};

/// Real gh 2.98.0 `gh auth status` with two keyring logins. gh writes this
/// report to stdout and exits 0.
const TWO_ACCOUNTS: &str = "\
github.com
  ✓ Logged in to github.com account bobmatnyc (keyring)
  - Active account: true
  - Git operations protocol: https
  - Token: gho_************************************
  - Token scopes: 'gist', 'project', 'read:org', 'repo', 'workflow'

  ✓ Logged in to github.com account bob-duetto (keyring)
  - Active account: false
  - Git operations protocol: https
  - Token: gho_************************************
  - Token scopes: 'gist', 'project', 'read:org', 'repo', 'workflow'
";

/// Real gh 2.98.0 output when its config dir names no host. gh writes this to
/// stderr and exits 1 — what the launchd daemon got.
const NO_HOSTS: &str = "You are not logged into any GitHub hosts. To log in, run: gh auth login\n";

const BOUND: Duration = Duration::from_secs(5);

fn stdout_ok(text: &str) -> GhOutput {
    GhOutput::from_parts("auth status", Some(0), text, "")
}

fn stderr_fail(code: i32, text: &str) -> GhOutput {
    GhOutput::from_parts("auth status", Some(code), "", text)
}

/// A gh config dir gh would not migrate (#8510).
fn versioned_dir(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).expect("create gh dir");
    std::fs::write(dir.join("config.yml"), "version: \"1\"\n").expect("config.yml");
    dir.to_path_buf()
}

/// A fake `gh`: `answer(dir)` per run, every `GH_CONFIG_DIR` it saw recorded.
fn fake_gh<F>(answer: F) -> (AuthStatusRunner, Arc<Mutex<Vec<Option<PathBuf>>>>)
where
    F: Fn(Option<&Path>) -> Result<GhOutput, String> + Send + Sync + 'static,
{
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    let run: AuthStatusRunner = Arc::new(move |dir: Option<&Path>| {
        record
            .lock()
            .expect("seen")
            .push(dir.map(Path::to_path_buf));
        answer(dir)
    });
    (run, seen)
}

/// The accepted login, or a panic naming what the probe said instead.
fn accepted(probe: &GhAuthProbe, login: &str) -> String {
    match probe {
        GhAuthProbe::Answered(status) => status
            .canonical_logged_in_login(login)
            .unwrap_or_else(|| panic!("'{login}' not seen; probe answered {status:?}")),
        GhAuthProbe::Inconclusive(why) => panic!("'{login}' not seen; probe unknown: {why}"),
    }
}

/// Why (#9091 regression): the live failure. The daemon's own gh reports no
/// host, and the operator's two accounts live in the project's pinned dir.
/// Test: itself.
#[test]
fn a_pinned_config_dir_with_two_accounts_is_seen_under_launchd() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let operator = versioned_dir(&tmp.path().join("gh-bobmatnyc"));
    let pinned = operator.clone();
    let (run, _) = fake_gh(move |dir| match dir {
        Some(d) if d == pinned => Ok(stdout_ok(TWO_ACCOUNTS)),
        _ => Ok(stderr_fail(1, NO_HOSTS)),
    });
    let sources = AccountDirSources {
        static_config_dir: Some(operator),
        state_root: Some(tmp.path().join("state")),
        own_config_dir: None,
    };
    for login in ["bobmatnyc", "BOB-DUETTO"] {
        let probe = probe_gh_login_with(login, &sources, BOUND, Arc::clone(&run));
        assert_eq!(accepted(&probe, login), login.to_ascii_lowercase());
    }
}

/// Why (#9091): the `--account` clone path's own dir, from the shared
/// [`tm_account_dir`], is asked too, so a login the clone path can use is one
/// the `gh_user` check accepts.
/// Test: itself.
#[test]
fn the_clone_paths_account_dir_is_seen_under_launchd() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let account_dir = tm_account_dir(tmp.path(), "bobmatnyc").expect("safe login");
    versioned_dir(&account_dir);
    let transcript = format!(
        "github.com\n  ✓ Logged in to github.com account bobmatnyc ({}/hosts.yml)\n  \
         - Active account: true\n",
        account_dir.display()
    );
    let expected = account_dir.clone();
    let (run, seen) = fake_gh(move |dir| match dir {
        Some(d) if d == expected => Ok(stdout_ok(&transcript)),
        _ => Ok(stderr_fail(1, NO_HOSTS)),
    });
    let sources = AccountDirSources {
        state_root: Some(tmp.path().to_path_buf()),
        ..AccountDirSources::default()
    };
    let probe = probe_gh_login_with("bobmatnyc", &sources, BOUND, run);
    assert_eq!(accepted(&probe, "bobmatnyc"), "bobmatnyc");
    assert!(seen.lock().expect("seen").contains(&Some(account_dir)));
}

/// Why (#9091 Fail-Open Check): a gh that cannot run anywhere never yields
/// acceptance, and the reason names "could not run gh", not "none".
/// Test: itself.
#[test]
fn gh_missing_everywhere_fails_closed_with_its_reason() {
    let tmp = tempfile::tempdir().expect("tempdir");
    versioned_dir(&tm_account_dir(tmp.path(), "bobmatnyc").expect("safe login"));
    let (run, _) = fake_gh(|_| Err("`gh` is not installed or not on PATH".to_string()));
    let sources = AccountDirSources {
        state_root: Some(tmp.path().to_path_buf()),
        ..AccountDirSources::default()
    };
    match probe_gh_login_with("bobmatnyc", &sources, BOUND, run) {
        GhAuthProbe::Inconclusive(why) => {
            assert!(
                why.contains("could not run gh: `gh` is not installed"),
                "{why}"
            );
        }
        other => panic!("a gh that cannot run must not answer, got {other:?}"),
    }
}

/// Why (#9091 Fail-Open Check): a run still going at the deadline is
/// unknown, never a "none" and never an acceptance.
/// Test: itself.
#[test]
fn a_run_past_the_deadline_is_unknown_not_none() {
    let (run, _) = fake_gh(|_| {
        std::thread::sleep(Duration::from_millis(400));
        Ok(stdout_ok(TWO_ACCOUNTS))
    });
    let sources = AccountDirSources::default();
    match probe_gh_login_with("bobmatnyc", &sources, Duration::from_millis(50), run) {
        GhAuthProbe::Inconclusive(why) => assert!(why.contains("did not answer"), "{why}"),
        other => panic!("expected Inconclusive, got {other:?}"),
    }
}

/// Why (#8510): a dir gh would migrate is refused before gh runs there, and
/// the refusal keeps the answer unknown rather than accepting or saying none.
/// Test: itself.
#[test]
fn a_dir_gh_would_migrate_is_never_asked() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let account_dir = tm_account_dir(tmp.path(), "bobmatnyc").expect("safe login");
    std::fs::create_dir_all(&account_dir).expect("create");
    std::fs::write(account_dir.join("config.yml"), "git_protocol: https\n").expect("write");
    let (run, seen) = fake_gh(|_| Ok(stderr_fail(1, NO_HOSTS)));
    let sources = AccountDirSources {
        state_root: Some(tmp.path().to_path_buf()),
        ..AccountDirSources::default()
    };
    let probe = probe_gh_login_with("bobmatnyc", &sources, BOUND, run);
    assert!(matches!(probe, GhAuthProbe::Inconclusive(_)), "{probe:?}");
    assert_eq!(*seen.lock().expect("seen"), vec![None]);
}

/// Why: one place that answers with the login is enough; an unknown login
/// with an unknown place stays unknown.
/// Test: itself.
#[test]
fn an_answer_elsewhere_outranks_an_unknown() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let operator = versioned_dir(&tmp.path().join("gh-op"));
    let pinned = operator.clone();
    let (run, _) = fake_gh(move |dir| match dir {
        Some(d) if d == pinned => Ok(stdout_ok(TWO_ACCOUNTS)),
        _ => Err("spawn failed".to_string()),
    });
    let sources = AccountDirSources {
        static_config_dir: Some(operator),
        ..AccountDirSources::default()
    };
    let probe = probe_gh_login_with("bob-duetto", &sources, BOUND, Arc::clone(&run));
    assert_eq!(accepted(&probe, "bob-duetto"), "bob-duetto");
    let probe = probe_gh_login_with("typo-bot", &sources, BOUND, run);
    assert!(matches!(probe, GhAuthProbe::Inconclusive(_)), "{probe:?}");
}

/// Why (#9091): gh's own "not logged into any GitHub hosts" is a definite
/// "none", even though gh exits 1.
/// Test: itself.
#[test]
fn gh_reporting_no_hosts_is_a_definite_none() {
    match classify_auth_status(Ok(stderr_fail(1, NO_HOSTS))) {
        GhAuthProbe::Answered(status) => assert!(status.logged_in.is_empty()),
        other => panic!("expected a definite none, got {other:?}"),
    }
    let (run, _) = fake_gh(|_| Ok(stderr_fail(1, NO_HOSTS)));
    let probe = probe_gh_login_with("bobmatnyc", &AccountDirSources::default(), BOUND, run);
    assert!(matches!(probe, GhAuthProbe::Answered(ref s) if s.logged_in.is_empty()));
}

/// Why (#9091): a spawn failure is "could not run gh", with its reason.
/// Test: itself.
#[test]
fn a_spawn_failure_is_could_not_run_gh() {
    match classify_auth_status(Err("permission denied".to_string())) {
        GhAuthProbe::Inconclusive(why) => assert_eq!(why, "could not run gh: permission denied"),
        other => panic!("expected Inconclusive, got {other:?}"),
    }
}

/// Why (#9091): a failure with no account line and none of gh's "not logged
/// in" words is a broken gh, not an empty host.
/// Test: itself.
#[test]
fn an_unrecognised_failure_is_not_a_none() {
    let out = stderr_fail(
        1,
        "failed to read configuration: yaml: line 3: bad indent\n",
    );
    match classify_auth_status(Ok(out)) {
        GhAuthProbe::Inconclusive(why) => {
            assert!(why.contains("exit 1"), "{why}");
            assert!(why.contains("failed to read configuration"), "{why}");
        }
        other => panic!("expected Inconclusive, got {other:?}"),
    }
}
