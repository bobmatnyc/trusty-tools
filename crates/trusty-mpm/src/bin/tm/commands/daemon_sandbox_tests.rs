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
        disallowed_names: Vec::new(),
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

/// Why (#9121, Architect ruling): the allowlist is closed. A credential spelled
/// without a `_TOKEN`/`_KEY` suffix (`GITHUB_PAT`, `DATABASE_URL`), a
/// lower-cased allowlisted name, and an `LC_`-prefixed name that is not a
/// locale category all refuse; every allowlisted name and locale category
/// passes.
/// Test: this test.
#[test]
fn disallowed_names_admits_only_the_closed_allowlist() {
    let allowed = [
        "HOME",
        "PATH",
        "TRUSTY_DATA_DIR_OVERRIDE",
        "TRUSTY_MPM_ADDR",
        "TMPDIR",
        "TERM",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "LC_TELEPHONE",
        "USER",
        "LOGNAME",
        "SHELL",
        "RUST_LOG",
    ];
    let refused = [
        "TELEGRAM_BOT_TOKEN",
        "OPENAI_API_KEY",
        "GITHUB_PAT",
        "DATABASE_URL",
        "LC_API_KEY",
        "home",
        "TRUSTY_BUGREPORT_TOKEN_FILE",
        "SSH_AUTH_SOCK",
        "TELEGRAM_BOT_TOKEN",
    ];
    let names = allowed.iter().chain(refused.iter()).map(OsString::from);

    assert_eq!(
        disallowed_names(names),
        vec![
            "DATABASE_URL",
            "GITHUB_PAT",
            "LC_API_KEY",
            "OPENAI_API_KEY",
            "SSH_AUTH_SOCK",
            "TELEGRAM_BOT_TOKEN",
            "TRUSTY_BUGREPORT_TOKEN_FILE",
            "home",
        ]
    );
}

/// Why: a name that is not UTF-8 cannot equal an allowlist entry, and must be
/// refused rather than skipped.
/// Test: this test.
#[test]
fn a_non_utf8_name_is_refused() {
    use std::os::unix::ffi::OsStringExt;
    let name = OsString::from_vec(vec![b'K', 0xff]);

    assert_eq!(disallowed_names(std::iter::once(name)).len(), 1);
}

/// Why: macOS sets `__CF_USER_TEXT_ENCODING` inside every process, so it must
/// pass — but only in CoreFoundation's own form, or it becomes a channel past
/// the allowlist.
/// Test: this test.
#[test]
fn only_a_well_formed_cf_text_encoding_is_allowed() {
    let cf = OsString::from("__CF_USER_TEXT_ENCODING");
    for good in ["0x1F5:0x0:0x0", "0x0:0x0:0x0", "1F5:0:0"] {
        assert!(is_cf_text_encoding(&cf, &OsString::from(good)), "{good}");
    }
    for bad in [
        "sk-fake-9121",
        "0x1F5:0x0",
        "0x1F5:0x0:0x0:0x0",
        "0xZZ:0x0:0x0",
        "",
    ] {
        assert!(!is_cf_text_encoding(&cf, &OsString::from(bad)), "{bad}");
    }
    let other = OsString::from("CF_USER_TEXT_ENCODING");
    assert!(!is_cf_text_encoding(
        &other,
        &OsString::from("0x1F5:0x0:0x0")
    ));
}

/// Why (#9121): the incident daemon inherited `TELEGRAM_BOT_TOKEN`. A sandbox
/// that sees any variable outside the allowlist must refuse, and the error
/// must name the variables so the operator can see what leaked in.
/// Test: this test.
#[test]
fn a_variable_outside_the_allowlist_refuses_and_names_only_the_variables() {
    let (home, account) = two_dirs();
    let mut env = isolated(home.path(), account.path());
    env.disallowed_names = vec!["OPENAI_API_KEY".into(), "TELEGRAM_BOT_TOKEN".into()];

    let err = enter_with(&env, || panic!("a refused sandbox must not latch"))
        .expect_err("a secret-carrying environment must refuse");

    let msg = err.to_string();
    assert!(msg.contains("TELEGRAM_BOT_TOKEN"), "{msg}");
    assert!(msg.contains("OPENAI_API_KEY"), "{msg}");
    assert!(msg.contains("values not shown"), "{msg}");
    assert!(msg.contains("#9121"), "{msg}");
    assert!(msg.contains("outside the sandbox allowlist"), "{msg}");
    assert!(
        msg.contains("sandbox launcher (`env -i` + allowlist)"),
        "{msg}"
    );
    // #7247: the binary ships to projects without this repo's script path.
    assert!(!msg.contains("scripts/"), "{msg}");
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
        disallowed_names: vec!["TELEGRAM_BOT_TOKEN".into()],
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
