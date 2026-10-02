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
    assert!(err.contains(&setup_hint("octo-new", &dir)), "{err}");
    assert!(err.contains("does not fall back"), "{err}");
    assert!(!dir.exists(), "a refused login must not get a dir");
}

/// 🔴 #8914 HIGH 1: tm itself writes the token it proved into the per-account
/// dir's 0600 `hosts.yml`, as gh's insecure-storage layout. Here the dir
/// yields nothing and the operator's default config yields the token.
#[test]
fn a_proven_token_is_stored_in_the_private_hosts_yml() {
    let (_tmp, state_root, operator) = fixture();
    let dir = account_dir(&state_root, "octo-pinned");
    let probe = TableProbe::default().answer(&operator, "github.com", "octo-pinned", Ok("tok-p"));
    let check = TableCheck::default().answer(API, "tok-p", Ok("octo-pinned"));
    prepare_session_account_with(
        &state_root,
        &operator,
        "octo-pinned",
        ORIGIN,
        &probe,
        &check,
    )
    .expect("a token proven from the operator's gh is accepted");
    let text = std::fs::read_to_string(dir.join("hosts.yml")).expect("hosts.yml");
    let doc: serde_yaml::Value = serde_yaml::from_str(&text).expect("yaml");
    let host = &doc["github.com"];
    assert_eq!(
        host["users"]["octo-pinned"]["oauth_token"].as_str(),
        Some("tok-p"),
        "the dir must hold its account's own token"
    );
    assert_eq!(host["oauth_token"].as_str(), Some("tok-p"));
    assert_eq!(host["user"].as_str(), Some("octo-pinned"));
    #[cfg(unix)]
    assert_eq!(mode(&dir.join("hosts.yml")), 0o600);
}

/// 🔴 #8914 HIGH 1: no refusal names `gh auth login` or `gh auth switch`;
/// both activate an account in the machine-wide keyring slot. Each names the
/// stdin setup instead.
#[test]
fn no_refusal_names_a_gh_auth_login_command() {
    let (_tmp, state_root, operator) = fixture();
    let refuse = |login: &str| {
        prepare_session_account_with(
            &state_root,
            &operator,
            login,
            ORIGIN,
            &TableProbe::default(),
            &TableCheck::default(),
        )
        .expect_err("no proven token must be refused")
    };
    for err in [refuse("octo-new"), refuse("octo-pinned")] {
        for banned in ["auth login", "auth switch", "--insecure-storage"] {
            assert!(!err.contains(banned), "names `{banned}`: {err}");
        }
        assert!(err.contains("--account-token-stdin"), "{err}");
    }
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
    let hosts = std::fs::read_to_string(dir.join("hosts.yml")).expect("hosts.yml");
    assert!(
        !hosts.contains("tok-"),
        "an unproven token is stored: {hosts}"
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
    // #8914 MEDIUM: the probe must never run in a dir gh would migrate.
    let probe = TableProbe::default().answer(&dir, "github.com", "octo-pinned", Ok("tok-p"));
    let check = TableCheck::default().answer(API, "tok-p", Ok("octo-pinned"));
    let err = prepare_session_account_with(
        &state_root,
        &operator,
        "octo-pinned",
        ORIGIN,
        &probe,
        &check,
    )
    .expect_err("a malformed config.yml must be refused");
    assert!(
        err.contains("cannot run this session as gh account")
            && err.contains("config.yml")
            && err.contains("refusing to rewrite it"),
        "{err}"
    );
    assert!(probe.calls().is_empty(), "gh ran: {:?}", probe.calls());
    assert!(check.calls().is_empty(), "GET /user ran");
    assert_eq!(
        std::fs::read_to_string(dir.join("config.yml")).expect("config.yml"),
        "version: [unclosed\n",
        "a refused config.yml must be kept byte-for-byte"
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

// ── #8914 HIGH 1: a token on stdin, proven, then stored ──────────────────────

/// 🔴 #8914 HIGH 1: a token on stdin that `GET /user` proves is stored in the
/// per-account dir, which is built private without the operator's gh knowing
/// the login.
#[test]
fn a_stdin_token_is_proven_then_stored() {
    let (_tmp, state_root, _operator) = fixture();
    let check = TableCheck::default().answer(API, "tok-in", Ok("Octo-New"));
    let dir = store_supplied_token(&state_root, "octo-new", ORIGIN, " tok-in\n", &check)
        .expect("a proven stdin token is stored");
    assert_eq!(dir, account_dir(&state_root, "octo-new"));
    assert_eq!(
        stored_token(&dir, "github.com", "octo-new").as_deref(),
        Some("tok-in")
    );
    let config = std::fs::read_to_string(dir.join("config.yml")).expect("config.yml");
    assert!(config.contains("version: \"1\""), "{config}");
    #[cfg(unix)]
    {
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join("hosts.yml")), 0o600);
    }
}

/// FAIL-OPEN CHECK: a stdin token for another account is refused, and nothing
/// is written.
#[test]
fn a_stdin_token_for_another_account_is_refused_and_not_stored() {
    let (_tmp, state_root, _operator) = fixture();
    let check = TableCheck::default().answer(API, "tok-active", Ok("octo-active"));
    let err = store_supplied_token(&state_root, "octo-new", ORIGIN, "tok-active", &check)
        .expect_err("another account's token must be refused");
    assert!(err.contains("authenticates as 'octo-active'"), "{err}");
    assert!(!err.contains("tok-"), "a refusal carries a token: {err}");
    assert!(!account_dir(&state_root, "octo-new").exists());
}

/// FAIL-OPEN CHECK: a stdin token whose `GET /user` fails is not proven, so it
/// is refused and nothing is written.
#[test]
fn a_stdin_token_whose_proof_fails_is_refused_and_not_stored() {
    let (_tmp, state_root, _operator) = fixture();
    let check = TableCheck::default().answer(API, "tok-in", Err("did not answer in time"));
    let err = store_supplied_token(&state_root, "octo-new", ORIGIN, "tok-in", &check)
        .expect_err("an unproven token must be refused");
    assert!(err.contains("did not answer in time"), "{err}");
    assert!(!account_dir(&state_root, "octo-new").exists());
}

/// FAIL-OPEN CHECK: an empty stdin token is refused before any `GET /user`.
#[test]
fn an_empty_stdin_token_is_refused() {
    let (_tmp, state_root, _operator) = fixture();
    let check = TableCheck::default();
    let err = store_supplied_token(&state_root, "octo-new", ORIGIN, " \n", &check)
        .expect_err("an empty token must be refused");
    assert!(err.contains("empty"), "{err}");
    assert!(check.calls().is_empty(), "GET /user ran");
}

/// FAIL-OPEN CHECK: a `hosts.yml` that is not a mapping is refused, never
/// replaced.
#[test]
fn a_hosts_yml_that_is_not_a_mapping_is_refused_and_kept() {
    let (_tmp, state_root, _operator) = fixture();
    let dir = account_dir(&state_root, "octo-new");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(dir.join("hosts.yml"), "- a list\n").expect("hosts.yml");
    let check = TableCheck::default().answer(API, "tok-in", Ok("octo-new"));
    let err = store_supplied_token(&state_root, "octo-new", ORIGIN, "tok-in", &check)
        .expect_err("a non-mapping hosts.yml must be refused");
    assert!(err.contains("not a gh hosts.yml mapping"), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.join("hosts.yml")).expect("hosts.yml"),
        "- a list\n"
    );
}

/// A stored token answers before gh is asked, so a stale keyring slot cannot
/// shadow it; a dir with none still asks gh.
#[test]
fn a_stored_token_is_read_before_gh_is_asked() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    std::fs::write(
        dir.join("hosts.yml"),
        "github.com:\n    users:\n        Octo-Pinned:\n            oauth_token: tok-file\n",
    )
    .expect("hosts.yml");
    let inner = TableProbe::default().answer(dir, "github.com", "octo-other", Ok("tok-gh"));
    let probe = StoredTokenFirst(&inner);
    assert_eq!(
        probe.token(dir, "github.com", "octo-pinned").as_deref(),
        Ok("tok-file")
    );
    assert_eq!(
        probe.token(dir, "github.com", "octo-other").as_deref(),
        Ok("tok-gh")
    );
    assert_eq!(inner.calls().len(), 1, "gh was asked for the stored login");
}

// ── #8914 MEDIUM: an SSH host alias is proven on its real host ──────────────

/// An `~/.ssh/config` alias resolves to the host GitHub serves, so the proof
/// never goes to `https://<alias>/api/v3`.
#[test]
fn an_ssh_alias_origin_is_proven_on_its_real_host() {
    let aliases = SshHostAliases::parse("Host gh-work\n    HostName github.com\n");
    let origin = session_origin("octo-pinned", "git@gh-work:acme/itinerary.git", &aliases)
        .expect("a known alias resolves");
    assert_eq!(
        origin_host(&origin).as_deref(),
        Ok("github.com"),
        "{origin}"
    );
}

/// FAIL-OPEN CHECK: an alias no `~/.ssh/config` entry renames is refused.
#[test]
fn an_unresolved_ssh_alias_is_refused() {
    let err = session_origin(
        "octo-pinned",
        "git@gh-work:acme/itinerary.git",
        &SshHostAliases::empty(),
    )
    .expect_err("an unknown alias must be refused");
    assert!(
        err.contains("gh-work") && err.contains("does not fall back"),
        "{err}"
    );
}

// ── spawn env ───────────────────────────────────────────────────────────────

/// A registry pin to tm's own account dir.
fn tm_dir_pin(state_root: &Path) -> PinnedGhIdentity {
    PinnedGhIdentity {
        account: Some("octo-pinned".into()),
        config_dir: Some(account_dir(state_root, "octo-pinned")),
        ..Default::default()
    }
}

/// `session_spawn_env` over no SSH aliases.
fn spawn(
    pinned: &PinnedGhIdentity,
    origin: &str,
    prove: impl FnOnce(&str) -> Result<ProvenToken, String>,
) -> GhSpawnEnv {
    session_spawn_env(
        pinned,
        origin,
        Path::new("/state"),
        &SshHostAliases::empty(),
        prove,
    )
    .expect("a pin yields an env")
    .expect("the env never errs")
}

/// Assert `env` is the nobody-token in both token variables, with a warning.
fn assert_fails_closed(env: &GhSpawnEnv) -> String {
    for var in ["GH_TOKEN", "GH_ENTERPRISE_TOKEN"] {
        assert_eq!(
            value_of(&env.vars, var).as_deref(),
            Some(REFUSED_GH_TOKEN),
            "{var}: {:?}",
            env.vars
        );
    }
    let warning = env.warning.clone().expect("the refusal is logged");
    assert!(warning.contains("--account-token-stdin"), "{warning}");
    assert!(!warning.contains("auth login"), "{warning}");
    warning
}

/// 🔴 #8914: the session env carries the proven token, the per-account dir
/// (never the operator's `~/.config/gh`), and `gh` as git's only helper.
#[test]
fn a_proven_tm_account_dir_pin_isolates_gh_and_git() {
    let env = spawn(&tm_dir_pin(Path::new("/state")), ORIGIN, |login| {
        assert_eq!(login, "octo-pinned");
        Ok(ProvenToken::for_test("github.com", "tok-p"))
    });
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

/// 🔴 #8914: no proven token → the nobody-token and the setup hint, never a
/// bare `GH_CONFIG_DIR` that gh resolves to the active account.
#[test]
fn an_unproven_tm_account_dir_pin_fails_closed() {
    let env = spawn(&tm_dir_pin(Path::new("/state")), ORIGIN, |_| {
        Err("no candidate answered".into())
    });
    assert_fails_closed(&env);
}

/// An operator-chosen config dir pin (#5851).
fn operator_dir_pin() -> PinnedGhIdentity {
    PinnedGhIdentity {
        account: Some("octo-pinned".into()),
        config_dir: Some(PathBuf::from("/home/me/.config/gh-duetto")),
        ..Default::default()
    }
}

/// 🔴 #8914 HIGH 3: an operator's config dir holds no token of its own, so it
/// is proven like tm's own dir and the proven token is injected.
#[test]
fn an_operator_config_dir_pin_is_proven_and_injected() {
    let env = spawn(&operator_dir_pin(), ORIGIN, |login| {
        assert_eq!(login, "octo-pinned");
        Ok(ProvenToken::for_test("github.com", "tok-p"))
    });
    assert_eq!(value_of(&env.vars, "GH_TOKEN").as_deref(), Some("tok-p"));
    assert_eq!(
        value_of(&env.vars, "GH_CONFIG_DIR").as_deref(),
        Some("/home/me/.config/gh-duetto")
    );
    assert_eq!(
        value_of(&env.vars, "GIT_CONFIG_VALUE_1").as_deref(),
        Some(GH_CREDENTIAL_HELPER)
    );
}

/// 🔴 #8914 HIGH 3 FAIL-OPEN CHECK: an operator config dir with no proven
/// token gets the nobody-token, never `GH_CONFIG_DIR` alone.
#[test]
fn an_operator_config_dir_pin_without_a_proven_token_fails_closed() {
    let env = spawn(&operator_dir_pin(), ORIGIN, |_| Err("not proven".into()));
    assert_fails_closed(&env);
}

/// FAIL-OPEN CHECK: a config dir pin with no account, whose `hosts.yml` names
/// none, cannot be proven, so it fails closed without asking.
#[test]
fn a_config_dir_pin_naming_no_login_fails_closed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pinned = PinnedGhIdentity {
        account: None,
        config_dir: Some(tmp.path().to_path_buf()),
        ..Default::default()
    };
    let env = spawn(&pinned, ORIGIN, |_| panic!("no login is ever proven"));
    let warning = assert_fails_closed(&env);
    assert!(warning.contains("names no account"), "{warning}");
    assert!(value_of(&env.vars, "GH_USER").is_none(), "{:?}", env.vars);
}

/// A config dir pin with no account proves the login its `hosts.yml` names.
#[test]
fn a_config_dir_pin_proves_the_login_its_hosts_yml_names() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("hosts.yml"),
        "github.com:\n    user: octo-pinned\n",
    )
    .expect("hosts.yml");
    let pinned = PinnedGhIdentity {
        account: None,
        config_dir: Some(tmp.path().to_path_buf()),
        ..Default::default()
    };
    let env = spawn(&pinned, ORIGIN, |login| {
        assert_eq!(login, "octo-pinned");
        Ok(ProvenToken::for_test("github.com", "tok-p"))
    });
    assert_eq!(value_of(&env.vars, "GH_TOKEN").as_deref(), Some("tok-p"));
}

/// 🔴 #8914 LOW FAIL-OPEN CHECK: an origin that names no gh host fails closed
/// and gets no guessed `github.com` credential helper.
#[test]
fn a_config_dir_pin_whose_origin_names_no_host_fails_closed() {
    let env = spawn(&tm_dir_pin(Path::new("/state")), "not a remote", |_| {
        panic!("an origin with no host is never proven")
    });
    assert_fails_closed(&env);
    assert!(
        value_of(&env.vars, "GIT_CONFIG_COUNT").is_none(),
        "{:?}",
        env.vars
    );
}
