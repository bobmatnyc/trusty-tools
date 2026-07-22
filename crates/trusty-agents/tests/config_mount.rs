//! Integration smoke test (#2405): the universal `config` command is mounted
//! and reachable on this binary.
//!
//! Why: epic #2400 Wave 1 mounts one shared `config keys …` credential surface
//! on every primary binary; this asserts the mount actually wired up HERE by
//! driving the real built binary — `config --help` parses and `config keys
//! list` runs fully offline (no key required, no value is ever printed).
//! What: spawns the binary via the Cargo-provided `CARGO_BIN_EXE_*` path and
//! checks the two offline invocations exit success with the expected surface
//! text.
//! Test: this file IS the test.

use std::process::Command;

/// Absolute path to the freshly built binary (Cargo sets this for integration
/// tests). Using it guarantees we exercise the real mounted CLI, not a stub.
const BIN: &str = env!("CARGO_BIN_EXE_tagent");

#[test]
fn config_help_advertises_keys_feature() {
    let out = Command::new(BIN)
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

/// Why (#3406 follow-up): `tagent config profile show|set` manages
/// `~/.trusty-agents/user.toml` — this drives the REAL binary against an
/// isolated `HOME` (a fresh tempdir) so it never touches the real developer
/// machine's profile, and proves `show` reports "not set" before any
/// profile exists, `set` persists one, and a subsequent `show` reads it
/// back.
/// What: spawns three sequential invocations sharing one isolated `HOME`.
/// Test: itself.
#[test]
fn profile_show_then_set_round_trips() {
    let home = tempfile::tempdir().expect("tempdir for isolated HOME");

    let before = Command::new(BIN)
        .args(["config", "profile", "show"])
        .env("HOME", home.path())
        .output()
        .expect("spawn `config profile show` (before)");
    assert!(before.status.success());
    let before_stdout = String::from_utf8_lossy(&before.stdout);
    assert!(
        before_stdout.contains("No profile set"),
        "expected 'not set' before any profile exists; got:\n{before_stdout}"
    );

    let set = Command::new(BIN)
        .args([
            "config",
            "profile",
            "set",
            "--name",
            "Ada",
            "--email",
            "ada@example.com",
            "--timezone",
            "UTC",
            "--location",
            "New York, NY, USA",
        ])
        .env("HOME", home.path())
        .output()
        .expect("spawn `config profile set`");
    assert!(
        set.status.success(),
        "`config profile set` should exit 0; stderr:\n{}",
        String::from_utf8_lossy(&set.stderr)
    );

    let after = Command::new(BIN)
        .args(["config", "profile", "show"])
        .env("HOME", home.path())
        .output()
        .expect("spawn `config profile show` (after)");
    assert!(after.status.success());
    let after_stdout = String::from_utf8_lossy(&after.stdout);
    assert!(after_stdout.contains("Ada"), "got:\n{after_stdout}");
    assert!(
        after_stdout.contains("ada@example.com"),
        "got:\n{after_stdout}"
    );
    assert!(after_stdout.contains("UTC"), "got:\n{after_stdout}");
    assert!(
        after_stdout.contains("New York, NY, USA"),
        "got:\n{after_stdout}"
    );
}
