//! Integration smoke test (#6631): `version --json` answers the way
//! `tctl doctor trusty-analyze --self-check` actually invokes it.
//!
//! Why: `trusty-installer`'s self-check spawns `<binary> version --json` as a
//! real subprocess and parses raw stdout (`spawn_json_at_path`), never calling
//! into this crate as a library. A unit test on `commands::version::envelope`
//! proves the JSON shape but not that the CLI is wired up to emit it — this
//! drives the real built binary the way the self-check does.
//! What: spawns the binary via the Cargo-provided `CARGO_BIN_EXE_*` path,
//! parses stdout as JSON, and asserts the fields `validate_version_envelope`
//! reads plus the crate's own version. `version` (no `--json`) gets the same
//! offline-CLI-surface check the other integration tests here use.
//! Test: this file IS the test.

use std::process::Command;

/// Absolute path to the freshly built binary (Cargo sets this for integration
/// tests). Using it guarantees we exercise the real mounted CLI, not a stub.
const BIN: &str = env!("CARGO_BIN_EXE_trusty-analyze");

#[test]
fn version_json_parses_and_carries_the_crate_version() {
    let out = Command::new(BIN)
        .args(["version", "--json"])
        .output()
        .expect("spawn `version --json`");
    assert!(
        out.status.success(),
        "`version --json` should exit 0; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("`version --json` must emit valid JSON");

    // The exact fields `validate_version_envelope` reads off this stdout
    // (trusty-installer::commands::probe) — a positive `contract_version` and
    // a non-empty `verbs[]`, both at the top level.
    assert!(
        v["contract_version"].as_u64().is_some_and(|n| n >= 1),
        "contract_version must be a positive integer: {v}"
    );
    assert!(
        v["verbs"].as_array().is_some_and(|a| !a.is_empty()),
        "verbs must be a non-empty array: {v}"
    );
    assert_eq!(
        v["tool_version"],
        env!("CARGO_PKG_VERSION"),
        "tool_version must be the crate's real release version: {v}"
    );
}

#[test]
fn version_without_json_prints_the_crate_version() {
    let out = Command::new(BIN)
        .arg("version")
        .output()
        .expect("spawn `version`");
    assert!(
        out.status.success(),
        "`version` should exit 0; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "plain `version` should print the crate version; got:\n{stdout}"
    );
}
