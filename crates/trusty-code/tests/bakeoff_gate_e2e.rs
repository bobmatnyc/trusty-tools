//! Black-box proof that `tcode bakeoff-gate` is really wired (#5441).
//!
//! Why: the library unit tests in `src/bakeoff/tests.rs` prove every rule
//! fires, but they cannot prove the operator surface exists — a gate nobody can
//! invoke enforces nothing, and #5441's whole point is that a milestone must be
//! blockable by a command someone actually runs. These tests spawn the compiled
//! binary exactly as the bake-off runner or a CI step would, and assert only on
//! its stdout and exit code.
//! What: a clean bundle exits 0; mock evidence exits 1 naming the rule; a
//! missing bundle root exits 2 (could not reach a verdict) rather than 1 (the
//! gate said no); `--json` emits one parseable document; and `--baseline`
//! blocks a verifier pass-rate drop.
//! Test: this file IS the test.

mod support;

use std::path::Path;

/// A `metadata.json` that passes every preflight rule, as raw JSON so this test
/// exercises the deserializer the external Python runner will actually feed.
fn metadata_json(level: u8) -> serde_json::Value {
    serde_json::json!({
        "level": level,
        "evidence_mode": "real",
        "runner": {
            "path": "/opt/ai-coding-bake-off/scripts/run_tcode_bakeoff.py",
            "revision": "runner-abc1234",
            "dirty": false
        },
        "challenge_revision": "challenges-def5678",
        "invocation": { "model": "anthropic/claude-sonnet-4", "provider": "openrouter", "timeout_secs": 3600 },
        "build": {
            "version": "0.5.1",
            "commit": "9d9571cd1",
            "commit_date": "2026-08-11",
            "binary_sha256": "4b868f56718cf87a3dac20b7347cf51557b882a9fc137409370766a98d97c295",
            "dirty": false
        },
        "source_digests": { "instructions": "sha256:1111", "agents": "sha256:2222", "skills": "sha256:3333" },
        "run": {
            "status": "success",
            "turns": 10,
            "duration_secs": 600.0,
            "cost_usd": 1.0,
            "tokens": { "prompt": 1000, "completion": 500, "cache_read": 200, "cache_creation": 100 }
        },
        "verifier": { "checks_total": 10, "checks_passed": 10 }
    })
}

/// Write a complete L1-L3 bundle, applying `mutate` to each level's metadata.
fn write_bundle(mutate: impl Fn(&mut serde_json::Value)) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("bundle tempdir");
    for level in [1u8, 2, 3] {
        let mut meta = metadata_json(level);
        mutate(&mut meta);
        write_level(tmp.path(), level, &meta);
    }
    tmp
}

/// Write one level directory with every artifact the gate requires.
fn write_level(root: &Path, level: u8, meta: &serde_json::Value) {
    let dir = root.join(format!("L{level}"));
    std::fs::create_dir_all(&dir).expect("mkdir level");
    let report = serde_json::json!({
        "status": meta["run"]["status"],
        "build": {
            "version": meta["build"]["version"],
            "commit": meta["build"]["commit"],
            "commit_date": meta["build"]["commit_date"],
        },
    });
    std::fs::write(dir.join("metadata.json"), meta.to_string()).expect("write metadata");
    std::fs::write(dir.join("tcode_report.json"), report.to_string()).expect("write report");
    std::fs::write(dir.join("prompt.txt"), "solve it\n").expect("write prompt");
    std::fs::write(dir.join("stderr.log"), "done\n").expect("write stderr");
    std::fs::write(dir.join("solution.diff"), "--- a\n+++ b\n").expect("write solution");
    std::fs::write(dir.join("verifier.json"), "{\"passed\":true}\n").expect("write verifier");
}

/// Run `tcode bakeoff-gate` with the given extra flags.
fn run_gate(args: &[&str]) -> (i32, String, String) {
    let output = support::tcode_command()
        .arg("bakeoff-gate")
        .args(args)
        .output()
        .expect("spawn tcode bakeoff-gate");
    (
        output.status.code().expect("exit code"),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn gate_passes_a_complete_real_bundle() {
    let bundle = write_bundle(|_| {});
    let (code, stdout, stderr) = run_gate(&["--bundle", &bundle.path().display().to_string()]);

    assert_eq!(code, 0, "stdout={stdout}\nstderr={stderr}");
    assert!(stdout.contains("bakeoff-gate: PASS"), "{stdout}");
    assert!(
        stdout.contains("no baseline given"),
        "an unbaselined pass must say so rather than imply a comparison happened: {stdout}"
    );
}

#[test]
fn gate_fails_on_mock_evidence() {
    let bundle = write_bundle(|m| m["evidence_mode"] = serde_json::json!("mock"));
    let (code, stdout, stderr) = run_gate(&["--bundle", &bundle.path().display().to_string()]);

    assert_eq!(code, 1, "stdout={stdout}\nstderr={stderr}");
    assert!(stdout.contains("bakeoff-gate: FAIL"), "{stdout}");
    assert!(stdout.contains("mock_evidence"), "{stdout}");
}

#[test]
fn gate_fails_on_incomplete_l1_l3_coverage() {
    let bundle = write_bundle(|_| {});
    std::fs::remove_dir_all(bundle.path().join("L3")).expect("drop L3");
    let (code, stdout, _) = run_gate(&["--bundle", &bundle.path().display().to_string()]);

    assert_eq!(code, 1, "{stdout}");
    assert!(stdout.contains("incomplete_coverage"), "{stdout}");
}

#[test]
fn gate_fails_on_a_different_candidate_build() {
    let bundle = write_bundle(|_| {});
    let (code, stdout, _) = run_gate(&[
        "--bundle",
        &bundle.path().display().to_string(),
        "--expect-commit",
        "ffffffff",
    ]);

    assert_eq!(code, 1, "{stdout}");
    assert!(stdout.contains("build_mismatch"), "{stdout}");
}

#[test]
fn gate_json_mode_emits_one_parseable_document() {
    let bundle = write_bundle(|_| {});
    let (code, stdout, _) = run_gate(&["--bundle", &bundle.path().display().to_string(), "--json"]);

    assert_eq!(code, 0, "{stdout}");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout must be one JSON document");
    assert_eq!(parsed["passed"], serde_json::json!(true));
    assert_eq!(parsed["levels"], serde_json::json!([1, 2, 3]));
}

#[test]
fn gate_blocks_a_verifier_regression_against_the_baseline() {
    let baseline = write_bundle(|_| {});
    let candidate = write_bundle(|m| {
        if m["level"] == serde_json::json!(3) {
            m["verifier"]["checks_passed"] = serde_json::json!(7);
        }
    });

    let (code, stdout, _) = run_gate(&[
        "--bundle",
        &candidate.path().display().to_string(),
        "--baseline",
        &baseline.path().display().to_string(),
    ]);

    assert_eq!(code, 1, "{stdout}");
    assert!(stdout.contains("correctness_regression"), "{stdout}");
    assert!(
        stdout.contains("7/10"),
        "the finding must name the counts: {stdout}"
    );
}

#[test]
fn gate_reports_an_unusable_bundle_root_distinctly() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let missing = tmp.path().join("no-such-bundle");
    let (code, _, stderr) = run_gate(&["--bundle", &missing.display().to_string()]);

    assert_eq!(
        code, 2,
        "a bundle that cannot be read is not a failed gate: {stderr}"
    );
    assert!(stderr.contains("no bundle directory"), "{stderr}");
}
