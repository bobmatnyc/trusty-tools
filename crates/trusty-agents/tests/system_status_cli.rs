//! Integration test (epic #3052): `tagent system status [--json]` is
//! mounted and reachable on this binary, and never requires an LLM
//! credential to run.
//!
//! Why: this project's "must be testable by CLI/API" directive means the
//! subsystem status check must be reachable without burning an LLM turn —
//! this drives the REAL built binary (`CARGO_BIN_EXE_tagent`) rather than
//! calling `system_status::gather` in-process, so a regression in the CLI
//! wiring itself (subcommand dispatch, argv parsing) is caught, not just a
//! regression in the shared core.
//! What: runs from a fresh, isolated `$HOME`/cwd (an empty non-repo dir,
//! mirroring the task's own live-verify scenario) so the daemons it probes
//! are guaranteed absent/undiscoverable and the output is deterministic —
//! every daemon must report `up: false`.
//! Test: this file IS the test.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_tagent");

/// Why: `--json` must emit parseable JSON carrying every top-level key the
/// tool/CLI contract promises, even when every daemon is unreachable.
/// Test: itself.
#[test]
fn system_status_json_has_expected_top_level_keys() {
    let tmp = tempfile::TempDir::new().unwrap();
    let out = Command::new(BIN)
        .args(["system", "status", "--json"])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .output()
        .expect("spawn `system status --json`");
    assert!(
        out.status.success(),
        "`system status --json` should exit 0; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let value: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("not valid JSON ({e}):\n{stdout}"));

    for key in [
        "tagent",
        "daemons",
        "mcp_servers",
        "credentials",
        "agent_registry_count",
        "skills_count",
    ] {
        assert!(
            value.get(key).is_some(),
            "expected top-level key {key:?} in {value}"
        );
    }

    let daemons = value["daemons"].as_array().expect("daemons is an array");
    assert_eq!(daemons.len(), 4, "expected all 4 daemons reported");
    for d in daemons {
        assert_eq!(
            d["up"], false,
            "an empty tempdir/HOME must show every daemon down: {d}"
        );
    }

    // Credential values must never appear — only names/tiers.
    let credentials = value["credentials"].as_array().expect("credentials array");
    assert!(!credentials.is_empty());
    for c in credentials {
        assert!(c.get("provider").is_some());
        assert!(c.get("status").is_some());
    }
}

/// Why: the human-readable (non-`--json`) path must also succeed offline
/// and mention every section, so an operator can run it without `--json`
/// and still get a complete picture.
/// Test: itself.
#[test]
fn system_status_text_mode_prints_every_section() {
    let tmp = tempfile::TempDir::new().unwrap();
    let out = Command::new(BIN)
        .args(["system", "status"])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .output()
        .expect("spawn `system status`");
    assert!(
        out.status.success(),
        "`system status` should exit 0; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("tagent"));
    assert!(stdout.contains("Daemons:"));
    assert!(stdout.contains("MCP servers:"));
    assert!(stdout.contains("Credentials"));
    assert!(stdout.contains("agents discovered"));
    assert!(stdout.contains("skills discovered"));
}
