//! `tctl stack health` and `stack doctor` report deployment drift (#10607).
//!
//! Why: the unit tests prove the audit; only the real binary proves the rollups
//! read it. Before #10607 neither command looked at what install had placed.
//! What: runs the built binary in a sandbox (temp HOME, data dir and PATH, so no
//! real install, record or daemon is read) against a planted record and fake
//! member scripts. Nothing on the sandbox PATH makes the probe reach a daemon.
//! Test: this file.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

/// A script answering `--version` with `version`.
fn plant_binary(path: &Path, version: &str) {
    let body = format!("#!/bin/sh\necho \"member {version}\"\n");
    std::fs::write(path, body).expect("write script");
    let mut perms = std::fs::metadata(path).expect("stat").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).expect("chmod");
}

/// Plants a record with one member, whose digest is that of `digest`.
fn plant_record(root: &Path, binary: &Path, version: &str, digest: &str) {
    let dir = root.join("data").join("trusty-installer");
    std::fs::create_dir_all(&dir).expect("data dir");
    let record = serde_json::json!({
        "schema": 1,
        "members": {"tga": [{
            "name": "tga",
            "path": binary,
            "version": version,
            "sha256": digest,
        }]},
    });
    std::fs::write(dir.join("deployment.json"), record.to_string()).expect("write record");
}

/// Runs the installer in the sandbox, returning its JSON.
fn run(root: &Path, args: &[&str]) -> (Output, serde_json::Value) {
    let empty = root.join("empty-path");
    std::fs::create_dir_all(&empty).expect("empty path");
    let out = Command::new(env!("CARGO_BIN_EXE_trusty-installer"))
        .args(args)
        .env_clear()
        .env("HOME", root)
        .env("CARGO_HOME", root.join("cargo"))
        .env("TRUSTY_DATA_DIR_OVERRIDE", root.join("data"))
        .env("PATH", empty)
        .output()
        .expect("run the installer");
    let json = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "no JSON ({e}): {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    (out, json)
}

fn finding_kinds(json: &serde_json::Value) -> Vec<String> {
    json["deployment"]["findings"]
        .as_array()
        .unwrap_or_else(|| panic!("no deployment findings in {json}"))
        .iter()
        .map(|f| f["kind"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// The sha256 the audit computes, from the audit's own `build_changed` message.
fn placed_digest(root: &Path, binary: &Path, version: &str) -> String {
    plant_record(root, binary, version, "planted");
    let (_, json) = run(root, &["--json", "stack", "health"]);
    let detail = json["deployment"]["findings"][0]["detail"]
        .as_str()
        .unwrap_or_else(|| panic!("no finding in {json}"))
        .to_owned();
    let (_, rest) = detail.split_once("(sha256 ").expect("names the digest");
    rest.split(',').next().expect("digest").to_owned()
}

/// Why: #10607 — each drift kind the issue names, through the shipped command.
/// What: stale binary, missing binary, swapped build, missing and corrupt record
/// each surface in `stack health --json` and `stack doctor --json`, as drift or
/// undetermined, and no case exits 0.
/// Test: this.
#[test]
fn stack_rollups_report_each_kind_of_drift() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let binary = root.join("tga");
    plant_binary(&binary, "1.0.0");
    let digest = placed_digest(root, &binary, "1.0.0");

    // No record: nothing was compared.
    std::fs::remove_file(root.join("data/trusty-installer/deployment.json")).expect("remove");
    let (out, json) = run(root, &["--json", "stack", "health"]);
    assert_eq!(finding_kinds(&json), ["no_record"], "{json}");
    assert_eq!(json["deployment"]["state"], "undetermined");
    assert_ne!(out.status.code(), Some(0));

    // Matching: the audit is clean.
    plant_record(root, &binary, "1.0.0", &digest);
    let (_, json) = run(root, &["--json", "stack", "health"]);
    assert_eq!(json["deployment"]["state"], "clean", "{json}");

    // Wrong version.
    plant_record(root, &binary, "2.0.0", &digest);
    for args in [
        &["--json", "stack", "health"],
        &["--json", "stack", "doctor"],
    ] {
        let (out, json) = run(root, args);
        assert_eq!(finding_kinds(&json), ["wrong_version"], "{json}");
        assert_eq!(json["verdict"], "degraded");
        assert_eq!(out.status.code(), Some(2));
    }

    // Swapped build at the same version.
    plant_record(root, &binary, "1.0.0", "0000");
    let (_, json) = run(root, &["--json", "stack", "health"]);
    assert_eq!(finding_kinds(&json), ["build_changed"], "{json}");

    // Corrupt record: unreadable is not empty.
    let record = root.join("data/trusty-installer/deployment.json");
    std::fs::write(&record, "{not json").expect("corrupt");
    let (_, json) = run(root, &["--json", "stack", "health"]);
    assert_eq!(finding_kinds(&json), ["no_record"], "{json}");

    // Missing binary.
    plant_record(root, &binary, "1.0.0", &digest);
    std::fs::remove_file(&binary).expect("remove binary");
    let (out, json) = run(root, &["--json", "stack", "health"]);
    assert_eq!(finding_kinds(&json), ["missing_binary"], "{json}");
    assert_eq!(json["deployment"]["state"], "drifted");
    assert_eq!(out.status.code(), Some(2));
}
