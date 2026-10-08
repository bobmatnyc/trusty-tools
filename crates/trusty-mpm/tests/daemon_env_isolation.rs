//! A test `tm daemon` starts from a cleared environment (#9121).
//!
//! Why: a daemon spawned with the test shell's environment inherits whatever
//! secrets that shell exports; one holding `TELEGRAM_BOT_TOKEN` polled the real
//! Telegram bot. `common::daemon_command` clears the environment and re-adds an
//! allowlist, and this module proves no secret-shaped name survives it.
//! What: sets a FAKE `ZZ_TEST_TOKEN` in this process, then checks the daemon
//! command's configured variables and the environment a probe child actually
//! receives under `common::apply_daemon_env`. Only variable NAMES are read or
//! printed. Mounted in `env_serial` because it writes the process environment.
//! Test: `cargo test -p trusty-mpm --test env_serial daemon_env_isolation::`.

use crate::common;

use std::process::Command;

/// Name endings that mark a variable as a credential.
const SECRET_SUFFIXES: &[&str] = &["_TOKEN", "_KEY", "_SECRET", "_PASSWORD"];

/// The fake credential this module plants in its own process.
const FAKE_SECRET: &str = "ZZ_TEST_TOKEN";

/// Removes [`FAKE_SECRET`] from the process on drop, pass or panic.
struct FakeSecret;

impl FakeSecret {
    fn plant() -> Self {
        // SAFETY: `env_serial` runs one test at a time, so no other thread
        // reads the environment while this write happens.
        unsafe { std::env::set_var(FAKE_SECRET, "fake-value-not-a-secret") };
        Self
    }
}

impl Drop for FakeSecret {
    fn drop(&mut self) {
        // SAFETY: as in `plant`.
        unsafe { std::env::remove_var(FAKE_SECRET) };
    }
}

/// The names in `names` that end in a credential suffix.
fn secret_shaped(names: &[String]) -> Vec<&str> {
    names
        .iter()
        .map(String::as_str)
        .filter(|name| SECRET_SUFFIXES.iter().any(|s| name.ends_with(s)))
        .collect()
}

/// The daemon's environment holds only allowlisted, non-secret names, and a
/// credential set in the parent process does not reach the child.
///
/// Why (#9121): the configured-variable check alone cannot see inheritance,
/// because `Command::get_envs` lists only explicit changes; the probe child
/// sees what the kernel actually hands over.
/// Test: this function IS the test.
#[test]
fn a_test_daemon_env_carries_no_secret_shaped_variable() {
    let home = tempfile::tempdir().expect("scratch home");
    let root = home.path().join("trusty-mpm-projects");
    let _fake = FakeSecret::plant();

    let daemon = common::daemon_command(home.path(), &root);
    let mut configured: Vec<String> = daemon
        .get_envs()
        .filter(|(_, value)| value.is_some())
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect();
    configured.sort();
    // #9396: TRUSTY_CONTENT_OFFLINE keeps a test daemon off the network.
    let mut allowed: Vec<String> = [
        "HOME",
        "TRUSTY_MPM_WORKSPACE_ROOT",
        "TRUSTY_MPM_ORPHAN_GC",
        trusty_mpm::content::first_use::OFFLINE_ENV,
    ]
    .into_iter()
    .chain(
        ["PATH", "TMUX_TMPDIR"]
            .into_iter()
            .filter(|name| std::env::var_os(name).is_some()),
    )
    .map(String::from)
    .collect();
    allowed.sort();
    assert_eq!(
        configured, allowed,
        "the daemon command sets a variable outside its allowlist"
    );

    let mut probe = Command::new("awk");
    probe.arg("BEGIN { for (name in ENVIRON) print name }");
    common::apply_daemon_env(&mut probe, home.path(), &root);
    let out = probe.output().expect("run the awk probe");
    assert!(out.status.success(), "the awk probe failed");
    let received: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();

    assert!(
        !received.iter().any(|name| name == FAKE_SECRET),
        "{FAKE_SECRET} set in the test process reached the daemon's environment"
    );
    assert_eq!(
        secret_shaped(&received),
        Vec::<&str>::new(),
        "the daemon's environment carries credential-shaped variables (names only)"
    );
}
