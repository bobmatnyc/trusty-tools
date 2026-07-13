//! Integration smoke test (#2405): the embeddable `config keys` sub-feature
//! is nested under trusty-search's PRE-EXISTING `config` command (daemon
//! memory-limit `get`/`set`) without regressing it.
//!
//! Why: trusty-search already owns a top-level `config` command in a
//! different domain (`config get/set memory-limit` — runtime daemon config),
//! so the universal inference-provider credential CLI could not be mounted as
//! a whole-grammar `Config(ConfigCommand)` top-level variant (it would
//! collide with the existing `ConfigAction` enum). Instead only the
//! embeddable `keys` feature (`trusty_common::inference::config::ConfigKeysCommand`)
//! is nested as a new `ConfigAction::Keys` variant. This test proves BOTH
//! grammars work side by side on the real built binary.
//! What: drives `CARGO_BIN_EXE_trusty-search` with `config keys --help` /
//! `config keys list` (new, offline, no key required, no value ever printed)
//! AND `config get --help` (pre-existing verb — must still parse cleanly).
//! Test: this file IS the test.

use std::process::Command;

/// Absolute path to the freshly built binary (Cargo sets this for integration
/// tests). Using it guarantees we exercise the real mounted CLI, not a stub.
const BIN: &str = env!("CARGO_BIN_EXE_trusty-search");

#[test]
fn config_keys_help_advertises_keys_feature() {
    let out = Command::new(BIN)
        .args(["config", "keys", "--help"])
        .output()
        .expect("spawn `config keys --help`");
    assert!(
        out.status.success(),
        "`config keys --help` should exit 0; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("set") && stdout.contains("list"),
        "`config keys --help` should advertise the set/list/test/unset verbs; got:\n{stdout}"
    );
}

#[test]
fn config_keys_list_runs_offline() {
    let out = Command::new(BIN)
        .args(["config", "keys", "list"])
        .output()
        .expect("spawn `config keys list`");
    assert!(
        out.status.success(),
        "`config keys list` should exit 0 offline; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Provider key status"),
        "`config keys list` should print the status header; got:\n{stdout}"
    );
}

/// Regression guard: the pre-existing daemon runtime `config get`/`config
/// set` verbs (memory-limit tuning) must still parse cleanly after nesting
/// `keys` alongside them — #2405 must not break trusty-search's own domain.
#[test]
fn preexisting_config_get_still_parses() {
    let out = Command::new(BIN)
        .args(["config", "get", "--help"])
        .output()
        .expect("spawn `config get --help`");
    assert!(
        out.status.success(),
        "`config get --help` should still exit 0 after the #2405 `keys` nesting; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.to_lowercase().contains("memory") || stdout.to_lowercase().contains("key"),
        "`config get --help` should still describe the memory-limit config key; got:\n{stdout}"
    );
}
