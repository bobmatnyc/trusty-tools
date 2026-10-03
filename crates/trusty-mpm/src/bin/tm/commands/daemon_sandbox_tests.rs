//! Fail-open check for `tm daemon --sandbox` (#9121).
//!
//! Every refusal arm has a test that reaches it, and the accepting case has
//! one that proves the checks can pass. No test reads or writes the process
//! environment or sets the process-wide credential latch: the environment is a
//! literal [`SandboxEnv`] and the latch is a closure.

use std::cell::Cell;

use super::*;

/// An isolated environment rooted at two fresh directories.
///
/// What: `home` and `account` are distinct, existing directories, the data-dir
/// override is set, and no secret-shaped variable is present.
fn isolated(home: &Path, account: &Path) -> SandboxEnv {
    SandboxEnv {
        secret_names: Vec::new(),
        data_dir_override: Some(home.join("data").into_os_string()),
        home: Some(home.to_path_buf()),
        account_home: Some(account.to_path_buf()),
    }
}

/// Two fresh directories: (sandbox home, account home).
fn two_dirs() -> (tempfile::TempDir, tempfile::TempDir) {
    (
        tempfile::tempdir().expect("home tempdir"),
        tempfile::tempdir().expect("account tempdir"),
    )
}

/// Why: the contract is "ends in `_TOKEN` or `_KEY`", compared without regard
/// to case, plus the file-pointer and registry forms the daemon reads. A name
/// that merely contains `TOKEN` or `KEY` elsewhere is not a secret.
/// Test: this test.
#[test]
fn secret_shaped_names_matches_the_contract_suffixes() {
    let names = [
        "TELEGRAM_BOT_TOKEN",
        "OPENAI_API_KEY",
        "slack_bot_token",
        "TRUSTY_BUGREPORT_TOKEN_FILE",
        "TRUSTY_BUGREPORT_GH_APP_KEY_FILE",
        "CLIENT_SECRET",
        "BITBUCKET_APP_PASSWORD",
        "HOME",
        "PATH",
        "TRUSTY_MPM_ADDR",
        "TOKENIZER_PATH",
        "KEYBOARD_LAYOUT",
        "TELEGRAM_BOT_TOKEN",
    ]
    .into_iter()
    .map(OsString::from);

    assert_eq!(
        secret_shaped_names(names),
        vec![
            "BITBUCKET_APP_PASSWORD",
            "CLIENT_SECRET",
            "OPENAI_API_KEY",
            "TELEGRAM_BOT_TOKEN",
            "TRUSTY_BUGREPORT_GH_APP_KEY_FILE",
            "TRUSTY_BUGREPORT_TOKEN_FILE",
            "slack_bot_token",
        ]
    );
}

/// Why (#9121): the incident daemon inherited `TELEGRAM_BOT_TOKEN`. A sandbox
/// that sees any `*_TOKEN` or `*_KEY` must refuse, and the error must name the
/// variables so the operator can see what leaked in.
/// Test: this test.
#[test]
fn a_token_or_key_env_refuses_and_names_only_the_variables() {
    let (home, account) = two_dirs();
    let mut env = isolated(home.path(), account.path());
    env.secret_names = vec!["OPENAI_API_KEY".into(), "TELEGRAM_BOT_TOKEN".into()];

    let err = enter_with(&env, || panic!("a refused sandbox must not latch"))
        .expect_err("a secret-carrying environment must refuse");

    let msg = err.to_string();
    assert!(msg.contains("TELEGRAM_BOT_TOKEN"), "{msg}");
    assert!(msg.contains("OPENAI_API_KEY"), "{msg}");
    assert!(msg.contains("values not shown"), "{msg}");
    assert!(msg.contains("#9121"), "{msg}");
    assert!(msg.contains("scripts/sandbox_daemon.sh"), "{msg}");
}

/// Why: without the override the daemon's socket and data land in the real
/// OS data directory, which `$HOME` does not redirect on macOS. Unset and
/// empty both refuse.
/// Test: this test.
#[test]
fn refuses_without_a_data_dir_override() {
    let (home, account) = two_dirs();
    for value in [None, Some(OsString::new())] {
        let mut env = isolated(home.path(), account.path());
        env.data_dir_override = value.clone();
        assert_eq!(
            refusals(&env),
            vec![Refusal::NoDataDirOverride],
            "override {value:?}"
        );
    }
}

/// Test: this test.
#[test]
fn refuses_when_home_is_unset() {
    let (home, account) = two_dirs();
    for value in [None, Some(PathBuf::new())] {
        let mut env = isolated(home.path(), account.path());
        env.home = value.clone();
        assert_eq!(refusals(&env), vec![Refusal::NoHome], "home {value:?}");
    }
}

/// Why: a daemon whose `$HOME` is the real home writes the operator's lock
/// file and framework root — it is not a sandbox at all.
/// Test: this test.
#[test]
fn refuses_when_home_is_the_account_home() {
    let (home, _) = two_dirs();
    let env = isolated(home.path(), home.path());
    assert_eq!(
        refusals(&env),
        vec![Refusal::HomeIsAccountHome(home.path().to_path_buf())]
    );
}

/// Why: a symlink to the real home is the real home; comparing the strings
/// would let it through.
/// Test: this test.
#[test]
fn refuses_when_home_symlinks_to_the_account_home() {
    let (account, scratch) = two_dirs();
    let link = scratch.path().join("home-link");
    std::os::unix::fs::symlink(account.path(), &link).expect("symlink");

    let env = isolated(&link, account.path());

    assert_eq!(refusals(&env), vec![Refusal::HomeIsAccountHome(link)]);
}

/// Why: with no password-database home there is nothing to prove `$HOME`
/// differs from, and an unknown answer must refuse.
/// Test: this test.
#[test]
fn refuses_when_the_account_home_is_unknown() {
    let (home, account) = two_dirs();
    for value in [None, Some(PathBuf::new())] {
        let mut env = isolated(home.path(), account.path());
        env.account_home = value.clone();
        assert_eq!(
            refusals(&env),
            vec![Refusal::AccountHomeUnknown],
            "account {value:?}"
        );
    }
}

/// Why: a `$HOME` that does not resolve cannot be compared after resolving
/// symlinks, so it refuses rather than falling back to a string compare.
/// Test: this test.
#[test]
fn refuses_when_home_does_not_exist() {
    let (home, account) = two_dirs();
    let missing = home.path().join("does-not-exist");
    let mut env = isolated(home.path(), account.path());
    env.home = Some(missing.clone());

    assert_eq!(refusals(&env), vec![Refusal::HomeUnresolvable(missing)]);
}

/// Why: every refusal is reported at once, so an operator fixes the
/// environment in one pass.
/// Test: this test.
#[test]
fn every_refusal_is_reported_together() {
    let env = SandboxEnv {
        secret_names: vec!["TELEGRAM_BOT_TOKEN".into()],
        ..SandboxEnv::default()
    };

    let msg = enter_with(&env, || panic!("must not latch"))
        .expect_err("an empty environment must refuse")
        .to_string();

    assert!(msg.contains("TELEGRAM_BOT_TOKEN"), "{msg}");
    assert!(msg.contains(DATA_DIR_OVERRIDE_ENV), "{msg}");
    assert!(msg.contains("$HOME is not set"), "{msg}");
}

/// Why: the refusal tests pass trivially if the checks always refused. An
/// isolated, clean environment enters, and enters exactly once.
/// Test: this test.
#[test]
fn an_isolated_clean_environment_enters_and_latches() {
    let (home, account) = two_dirs();
    let latched = Cell::new(0);

    enter_with(&isolated(home.path(), account.path()), || {
        latched.set(latched.get() + 1);
    })
    .expect("an isolated environment enters sandbox mode");

    assert_eq!(latched.get(), 1);
}

/// Test: this test.
#[test]
fn a_refused_sandbox_never_latches() {
    let (home, account) = two_dirs();
    let mut env = isolated(home.path(), account.path());
    env.data_dir_override = None;
    let latched = Cell::new(false);

    let _ = enter_with(&env, || latched.set(true));

    assert!(!latched.get(), "a refused sandbox set the credential latch");
}

/// Why (#9121): the incident was the bot spawning. In sandbox mode the spawn
/// must not run at all.
/// Test: this test.
#[test]
fn sandbox_never_spawns_the_telegram_bot() {
    let spawned = Cell::new(false);

    let handle = gate_channel_pollers(true, || {
        spawned.set(true);
        Some(())
    });

    assert!(handle.is_none());
    assert!(!spawned.get(), "the bot spawn ran in sandbox mode");
}

/// Test: this test.
#[test]
fn outside_sandbox_the_bot_spawn_runs() {
    let spawned = Cell::new(false);

    let handle = gate_channel_pollers(false, || {
        spawned.set(true);
        Some(())
    });

    assert!(handle.is_some());
    assert!(spawned.get());
}
