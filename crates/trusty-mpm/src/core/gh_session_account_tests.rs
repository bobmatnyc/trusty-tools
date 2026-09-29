//! Tests for the `--account` session gate and spawn env (#8914).
//!
//! Why: a session started with `--account X` must run `gh` and HTTPS git as X
//! or not start; the machine's active account is never a fallback.
//! What: temp dirs, fake operator `hosts.yml`, and table fakes for `gh auth
//! token` and `GET /user`. No real `gh`, keyring, `$HOME` or network.
//! Test: this file IS the test module.

use std::path::{Path, PathBuf};

use super::*;
use crate::core::gh_account::REFUSED_GH_TOKEN;
use crate::core::gh_account_dir::gh_account_dir_tests::{TableCheck, TableProbe};

const ORIGIN: &str = "https://github.com/acme/itinerary";
const API: &str = "https://api.github.com";

/// An operator `hosts.yml` where `octo-active` is active and `octo-pinned` is
/// also logged in — the #8914 machine shape.
const OPERATOR_HOSTS: &str = "\
github.com:
    users:
        octo-active:
        octo-pinned:
    user: octo-active
";

/// A state root and an operator gh dir holding [`OPERATOR_HOSTS`].
fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_root = tmp.path().join("state");
    let operator = tmp.path().join("operator-gh");
    std::fs::create_dir_all(&operator).expect("operator dir");
    std::fs::write(operator.join("hosts.yml"), OPERATOR_HOSTS).expect("hosts.yml");
    std::fs::write(operator.join("config.yml"), "version: \"1\"\n").expect("config.yml");
    (tmp, state_root, operator)
}

fn account_dir(state_root: &Path, login: &str) -> PathBuf {
    state_root.join("gh-accounts").join(login)
}

fn value_of(vars: &[(String, String)], name: &str) -> Option<String> {
    vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777
}

/// A logged-in login gets a 0700 dir with a 0600 `hosts.yml`, proven through
/// the dir itself.
#[test]
fn a_logged_in_login_gets_a_private_proven_dir() {
    let (_tmp, state_root, operator) = fixture();
    let dir = account_dir(&state_root, "octo-pinned");
    let probe = TableProbe::default().answer(&dir, "github.com", "octo-pinned", Ok("tok-p"));
    let check = TableCheck::default().answer(API, "tok-p", Ok("octo-pinned"));
    let got = prepare_session_account_with(
        &state_root,
        &operator,
        "octo-pinned",
        ORIGIN,
        &probe,
        &check,
    )
    .expect("a logged-in, proven login is accepted");
    assert_eq!(got, dir);
    #[cfg(unix)]
    {
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join("hosts.yml")), 0o600);
        assert_eq!(mode(&dir.join("config.yml")), 0o600);
    }
}

/// 🔴 #8914: a login with no stored credential is refused with the one-time
/// setup command, and no dir is created — no fallback to `octo-active`.
#[test]
fn an_unknown_login_is_refused_with_the_setup_command() {
    let (_tmp, state_root, operator) = fixture();
    let err = prepare_session_account_with(
        &state_root,
        &operator,
        "octo-new",
        ORIGIN,
        &TableProbe::default(),
        &TableCheck::default(),
    )
    .expect_err("an unknown login must be refused");
    let dir = account_dir(&state_root, "octo-new");
    assert!(
        err.contains(&one_time_setup_command(&dir, "github.com")),
        "{err}"
    );
    assert!(err.contains("does not fall back"), "{err}");
    assert!(!dir.exists(), "a refused login must not get a dir");
}

/// 🔴 #8914: `-u octo-pinned` answers with the active account's token (the
/// #5851 keyring shape). Refused, never used.
#[test]
fn a_token_for_another_account_is_refused_not_used() {
    let (_tmp, state_root, operator) = fixture();
    let dir = account_dir(&state_root, "octo-pinned");
    let probe = TableProbe::default()
        .answer(&dir, "github.com", "octo-pinned", Ok("tok-active"))
        .answer(&operator, "github.com", "octo-pinned", Ok("tok-active"));
    let check = TableCheck::default().answer(API, "tok-active", Ok("octo-active"));
    let err = prepare_session_account_with(
        &state_root,
        &operator,
        "octo-pinned",
        ORIGIN,
        &probe,
        &check,
    )
    .expect_err("another account's token must be refused");
    assert!(err.contains("authenticates as 'octo-active'"), "{err}");
    assert!(
        !err.contains("tok-"),
        "a refusal must never carry a token: {err}"
    );
}

/// FAIL-OPEN CHECK: an existing dir that cannot be listed is refused, not
/// chmod-ed back into use.
#[cfg(unix)]
#[test]
fn an_unreadable_account_dir_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let (_tmp, state_root, operator) = fixture();
    let dir = account_dir(&state_root, "octo-pinned");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    if std::fs::read_dir(&dir).is_ok() {
        // Running as root: the mode cannot make the dir unreadable.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        return;
    }
    let result = prepare_session_account_with(
        &state_root,
        &operator,
        "octo-pinned",
        ORIGIN,
        &TableProbe::default(),
        &TableCheck::default(),
    );
    assert_eq!(mode(&dir), 0o000, "the dir must not be repaired");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let err = result.expect_err("an unreadable dir must be refused");
    assert!(err.contains("is unreadable"), "{err}");
}

/// FAIL-OPEN CHECK: a `hosts.yml` that names no account is refused.
#[test]
fn a_malformed_hosts_yml_is_refused() {
    let (_tmp, state_root, operator) = fixture();
    let dir = account_dir(&state_root, "octo-pinned");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(dir.join("hosts.yml"), ":: not yaml [").expect("hosts.yml");
    std::fs::write(dir.join("config.yml"), "version: \"1\"\n").expect("config.yml");
    let err = prepare_session_account_with(
        &state_root,
        &operator,
        "octo-pinned",
        ORIGIN,
        &TableProbe::default(),
        &TableCheck::default(),
    )
    .expect_err("a malformed hosts.yml must be refused");
    assert!(err.contains("names no github.com account"), "{err}");
}

/// FAIL-OPEN CHECK: an existing dir with no `hosts.yml`, for a login the
/// operator's gh does not know, is refused.
#[test]
fn a_missing_hosts_yml_for_an_unknown_login_is_refused() {
    let (_tmp, state_root, operator) = fixture();
    let dir = account_dir(&state_root, "octo-new");
    std::fs::create_dir_all(&dir).expect("dir");
    let err = prepare_session_account_with(
        &state_root,
        &operator,
        "octo-new",
        ORIGIN,
        &TableProbe::default(),
        &TableCheck::default(),
    )
    .expect_err("a dir with no hosts.yml and no login must be refused");
    assert!(err.contains("is not logged into gh"), "{err}");
}

/// FAIL-OPEN CHECK: a `config.yml` gh would migrate is refused.
#[test]
fn a_malformed_config_yml_is_refused() {
    let (_tmp, state_root, operator) = fixture();
    let dir = account_dir(&state_root, "octo-pinned");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(
        dir.join("hosts.yml"),
        "github.com:\n    user: octo-pinned\n    users:\n        octo-pinned:\n",
    )
    .expect("hosts.yml");
    std::fs::write(dir.join("config.yml"), "version: [unclosed\n").expect("config.yml");
    let err = prepare_session_account_with(
        &state_root,
        &operator,
        "octo-pinned",
        ORIGIN,
        &TableProbe::default(),
        &TableCheck::default(),
    )
    .expect_err("a malformed config.yml must be refused");
    assert!(
        err.contains("cannot run this session as gh account"),
        "{err}"
    );
}

/// A dir the operator's own `gh auth login` created at gh's modes is made
/// private on reuse.
#[cfg(unix)]
#[test]
fn a_reused_dir_is_made_private() {
    use std::os::unix::fs::PermissionsExt;
    let (_tmp, state_root, operator) = fixture();
    let dir = account_dir(&state_root, "octo-pinned");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(
        dir.join("hosts.yml"),
        "github.com:\n    user: octo-pinned\n    users:\n        octo-pinned:\n",
    )
    .expect("hosts.yml");
    std::fs::write(dir.join("config.yml"), "version: \"1\"\n").expect("config.yml");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    std::fs::set_permissions(
        dir.join("hosts.yml"),
        std::fs::Permissions::from_mode(0o644),
    )
    .expect("chmod");
    let probe = TableProbe::default().answer(&dir, "github.com", "octo-pinned", Ok("tok-p"));
    let check = TableCheck::default().answer(API, "tok-p", Ok("octo-pinned"));
    prepare_session_account_with(
        &state_root,
        &operator,
        "octo-pinned",
        ORIGIN,
        &probe,
        &check,
    )
    .expect("a valid reused dir is accepted");
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&dir.join("hosts.yml")), 0o600);
}

/// A registry pin to tm's own account dir.
fn tm_dir_pin(state_root: &Path) -> PinnedGhIdentity {
    PinnedGhIdentity {
        account: Some("octo-pinned".into()),
        config_dir: Some(account_dir(state_root, "octo-pinned")),
    }
}

/// 🔴 #8914: the session env carries the proven token, the per-account dir
/// (never the operator's `~/.config/gh`), and `gh` as git's only helper.
#[test]
fn a_proven_tm_account_dir_pin_isolates_gh_and_git() {
    let state_root = Path::new("/state");
    let env = session_spawn_env(&tm_dir_pin(state_root), ORIGIN, state_root, |login| {
        assert_eq!(login, "octo-pinned");
        Ok(ProvenToken::for_test("github.com", "tok-p"))
    })
    .expect("a pin yields an env")
    .expect("the env never errs");
    let vars = &env.vars;
    assert_eq!(value_of(vars, "GH_TOKEN").as_deref(), Some("tok-p"));
    assert_eq!(value_of(vars, "GH_USER").as_deref(), Some("octo-pinned"));
    assert_eq!(
        value_of(vars, "GH_CONFIG_DIR").as_deref(),
        Some("/state/gh-accounts/octo-pinned")
    );
    assert_eq!(value_of(vars, "GIT_CONFIG_COUNT").as_deref(), Some("2"));
    assert_eq!(
        value_of(vars, "GIT_CONFIG_KEY_1").as_deref(),
        Some("credential.https://github.com.helper")
    );
    assert_eq!(value_of(vars, "GIT_CONFIG_VALUE_0").as_deref(), Some(""));
    assert_eq!(
        value_of(vars, "GIT_CONFIG_VALUE_1").as_deref(),
        Some(GH_CREDENTIAL_HELPER)
    );
    assert!(env.warning.is_none(), "{:?}", env.warning);
}

/// 🔴 #8914: no proven token → the nobody-token and the setup command, never
/// a bare `GH_CONFIG_DIR` that gh resolves to the active account.
#[test]
fn an_unproven_tm_account_dir_pin_fails_closed() {
    let state_root = Path::new("/state");
    let env = session_spawn_env(&tm_dir_pin(state_root), ORIGIN, state_root, |_| {
        Err("no candidate answered".into())
    })
    .expect("a pin yields an env")
    .expect("the refusal rides the env");
    for var in ["GH_TOKEN", "GH_ENTERPRISE_TOKEN"] {
        assert_eq!(
            value_of(&env.vars, var).as_deref(),
            Some(REFUSED_GH_TOKEN),
            "{var}"
        );
    }
    let warning = env.warning.expect("the refusal is logged");
    assert!(warning.contains("--insecure-storage"), "{warning}");
}

/// An operator-chosen config dir (#5851) is outside tm's account dirs and
/// keeps its pre-#8914 env: `GH_CONFIG_DIR` only, never a proof.
#[test]
fn an_operator_config_dir_pin_is_unchanged() {
    let pinned = PinnedGhIdentity {
        account: Some("octo-pinned".into()),
        config_dir: Some(PathBuf::from("/home/me/.config/gh-duetto")),
    };
    let env = session_spawn_env(&pinned, ORIGIN, Path::new("/state"), |_| {
        panic!("an operator config dir is never proven")
    })
    .expect("a pin yields an env")
    .expect("the env never errs");
    assert!(value_of(&env.vars, "GH_TOKEN").is_none(), "{:?}", env.vars);
    assert!(
        value_of(&env.vars, "GIT_CONFIG_COUNT").is_none(),
        "{:?}",
        env.vars
    );
}
