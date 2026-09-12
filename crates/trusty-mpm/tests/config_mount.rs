//! Integration smoke test (#2405): the universal `config` command is mounted
//! and reachable on this binary.
//!
//! Why: epic #2400 Wave 1 mounts one shared `config keys …` credential surface
//! on every primary binary; this asserts the mount actually wired up HERE by
//! driving the real built binary — `config --help` parses and `config keys
//! list` runs fully offline (no key required, no value is ever printed).
//! What: spawns the real built binary through `common::tm_command` — which
//! confines the child to a scratch `$HOME` (#7568) — and checks the two offline
//! invocations exit success with the expected surface text.
//! Test: this file IS the test.

mod common;

#[test]
fn config_help_advertises_keys_feature() {
    let out = common::tm_command()
        .args(["config", "--help"])
        .output()
        .expect("spawn `config --help`");
    assert!(
        out.status.success(),
        "`config --help` should exit 0; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("keys"),
        "`config --help` should advertise the `keys` feature; got:\n{stdout}"
    );
}

#[test]
fn config_keys_list_runs_offline() {
    let out = common::tm_command()
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
