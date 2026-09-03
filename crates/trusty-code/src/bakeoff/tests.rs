//! Unit coverage for the L1-L3 bake-off exit gate (#5441).
//!
//! Why: the gate's whole value is that it REJECTS — so every rejection needs a
//! test that proves it fires, and the accept path needs one that proves the
//! gate is not simply always red. The fixture below writes a bundle that
//! passes, and each rejection test mutates exactly one thing.
//! What: [`bundle_at`] materialises a complete three-level bundle in a tempdir
//! from a per-level mutator, so a test's diff from "valid" is one closure.
//! Test: this file IS the test.

use std::collections::BTreeMap;
use std::path::Path;

use super::compare::{Baseline, ComparisonInputs, compare_against_baseline};
use super::metadata::*;
use super::*;

/// A metadata document that passes every preflight rule.
fn valid_metadata(level: u8) -> LevelMetadata {
    LevelMetadata {
        level,
        evidence_mode: EvidenceMode::Real,
        runner: RunnerProvenance {
            path: "/opt/ai-coding-bake-off/scripts/run_tcode_bakeoff.py".to_string(),
            revision: "runner-abc1234".to_string(),
            dirty: false,
        },
        challenge_revision: "challenges-def5678".to_string(),
        invocation: Invocation {
            model: "anthropic/claude-sonnet-4".to_string(),
            provider: "openrouter".to_string(),
            timeout_secs: 3600,
        },
        build: BuildProvenance {
            version: "0.5.1".to_string(),
            commit: "9d9571cd1".to_string(),
            commit_date: "2026-08-11".to_string(),
            binary_sha256: "4b868f56718cf87a3dac20b7347cf51557b882a9fc137409370766a98d97c295"
                .to_string(),
            dirty: false,
        },
        source_digests: SourceDigests {
            instructions: "sha256:1111".to_string(),
            agents: "sha256:2222".to_string(),
            skills: "sha256:3333".to_string(),
        },
        run: RunTelemetry {
            status: "success".to_string(),
            turns: 10,
            duration_secs: 600.0,
            cost_usd: Some(1.0),
            tokens: Tokens {
                prompt: 1000,
                completion: 500,
                cache_read: 200,
                cache_creation: 100,
            },
        },
        verifier: VerifierResult {
            checks_total: 10,
            checks_passed: 10,
        },
    }
}

/// The `tcode_report.json` a level's metadata must agree with.
fn report_for(meta: &LevelMetadata) -> serde_json::Value {
    serde_json::json!({
        "status": meta.run.status,
        "build": {
            "version": meta.build.version,
            "commit": meta.build.commit,
            "commit_date": meta.build.commit_date,
        },
        "exit_code": 0,
    })
}

/// Write one level directory with every required artifact.
fn write_level(root: &Path, meta: &LevelMetadata) {
    let dir = root.join(format!("L{}", meta.level));
    std::fs::create_dir_all(&dir).expect("mkdir level");
    std::fs::write(
        dir.join("metadata.json"),
        serde_json::to_string_pretty(meta).expect("serialize metadata"),
    )
    .expect("write metadata");
    std::fs::write(
        dir.join("tcode_report.json"),
        serde_json::to_string_pretty(&report_for(meta)).expect("serialize report"),
    )
    .expect("write report");
    std::fs::write(dir.join("prompt.txt"), "solve the challenge\n").expect("write prompt");
    std::fs::write(dir.join("stderr.log"), "run complete\n").expect("write stderr");
    std::fs::write(dir.join("solution.diff"), "--- a\n+++ b\n").expect("write solution");
    std::fs::write(dir.join("verifier.json"), "{\"passed\":true}\n").expect("write verifier");
}

/// Materialise a full L1-L3 bundle, applying `mutate` to each level first.
fn bundle_at(mutate: impl Fn(&mut LevelMetadata)) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("bundle tempdir");
    for level in LEVELS {
        let mut meta = valid_metadata(level);
        mutate(&mut meta);
        write_level(tmp.path(), &meta);
    }
    tmp
}

/// A bundle nothing was changed in.
fn valid_bundle() -> tempfile::TempDir {
    bundle_at(|_| {})
}

/// Run the preflight over a bundle root with no pins.
fn gate(root: &Path) -> GateReport {
    preflight(&load_bundle(root), &Pins::default())
}

/// Every rule fired by a report, deduplicated for readable assertions.
fn rules(report: &GateReport) -> Vec<Rule> {
    let mut seen: Vec<Rule> = Vec::new();
    for violation in &report.violations {
        if !seen.contains(&violation.rule) {
            seen.push(violation.rule);
        }
    }
    seen
}

// ---------------------------------------------------------------- metadata --

#[test]
fn metadata_round_trips_the_documented_shape() {
    let meta = valid_metadata(2);
    let json = serde_json::to_string(&meta).expect("serialize");
    let back: LevelMetadata = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(meta, back);
}

#[test]
fn provenance_gaps_names_every_missing_field() {
    let mut meta = valid_metadata(1);
    meta.runner.revision = String::new();
    meta.build.commit = "unknown".to_string();
    meta.build.commit_date = "  ".to_string();
    meta.source_digests.skills = "UNKNOWN".to_string();
    meta.invocation.timeout_secs = 0;

    let gaps = meta.provenance_gaps();
    assert_eq!(
        gaps,
        vec![
            "runner.revision",
            "build.commit",
            "build.commit_date",
            "source_digests.skills",
            "invocation.timeout_secs",
        ],
        "every unusable field must be named, including the literal \"unknown\" build_info emits"
    );
    assert!(valid_metadata(1).provenance_gaps().is_empty());
}

#[test]
fn unknown_evidence_mode_is_not_real() {
    let parsed: EvidenceMode =
        serde_json::from_str("\"replayed\"").expect("unknown mode must still deserialize");
    assert_eq!(parsed, EvidenceMode::Unknown);
    assert!(!parsed.is_real(), "an unrecognised mode must fail closed");
    assert!(!EvidenceMode::Mock.is_real());
    assert!(EvidenceMode::Real.is_real());
}

#[test]
fn rule_keys_are_distinct() {
    let all = [
        Rule::IncompleteCoverage,
        Rule::MissingArtifact,
        Rule::MalformedMetadata,
        Rule::MockEvidence,
        Rule::MissingProvenance,
        Rule::DirtyCheckout,
        Rule::StaleRunner,
        Rule::BuildMismatch,
        Rule::CorrectnessRegression,
        Rule::MissingDeliverable,
        Rule::UndispositionedChange,
    ];
    let mut keys: Vec<&str> = all.iter().map(|r| r.as_str()).collect();
    keys.sort_unstable();
    let count = keys.len();
    keys.dedup();
    assert_eq!(keys.len(), count, "each rule needs its own stable key");
}

// --------------------------------------------------------------- preflight --

#[test]
fn a_complete_bundle_passes_preflight() {
    let tmp = valid_bundle();
    let report = gate(tmp.path());
    assert!(
        report.passed(),
        "expected a clean gate, got: {}",
        report.render_human()
    );
    assert_eq!(report.levels, vec![1, 2, 3]);
}

#[test]
fn a_missing_level_is_incomplete_coverage() {
    let tmp = valid_bundle();
    std::fs::remove_dir_all(tmp.path().join("L3")).expect("drop L3");

    let report = gate(tmp.path());
    assert!(!report.passed());
    assert_eq!(rules(&report), vec![Rule::IncompleteCoverage]);
    assert_eq!(report.levels, vec![1, 2]);
}

#[test]
fn a_missing_artifact_is_rejected() {
    let tmp = valid_bundle();
    std::fs::remove_file(tmp.path().join("L2").join("stderr.log")).expect("drop stderr");

    let report = gate(tmp.path());
    assert_eq!(rules(&report), vec![Rule::MissingArtifact]);
    let violation = &report.violations[0];
    assert_eq!(violation.level, Some(2));
    assert!(
        violation.detail.contains("stderr.log"),
        "{}",
        violation.detail
    );
}

#[test]
fn an_empty_artifact_is_rejected() {
    let tmp = valid_bundle();
    std::fs::write(tmp.path().join("L1").join("solution.diff"), "").expect("truncate solution");

    let report = gate(tmp.path());
    assert_eq!(rules(&report), vec![Rule::MissingArtifact]);
    assert!(
        report.violations[0].detail.contains("is empty"),
        "{}",
        report.violations[0].detail
    );
}

#[test]
fn malformed_metadata_is_rejected() {
    let tmp = valid_bundle();
    std::fs::write(tmp.path().join("L1").join("metadata.json"), "{ not json")
        .expect("corrupt metadata");

    let report = gate(tmp.path());
    assert_eq!(rules(&report), vec![Rule::MalformedMetadata]);
    assert_eq!(report.levels, vec![2, 3]);
}

#[test]
fn mock_evidence_is_rejected() {
    let tmp = bundle_at(|m| m.evidence_mode = EvidenceMode::Mock);
    let report = gate(tmp.path());
    assert_eq!(rules(&report), vec![Rule::MockEvidence]);
    assert_eq!(report.violations.len(), 3, "one per level");
}

#[test]
fn zero_verifier_checks_is_mock_evidence() {
    let tmp = bundle_at(|m| {
        m.verifier = VerifierResult {
            checks_total: 0,
            checks_passed: 0,
        }
    });
    let report = gate(tmp.path());
    assert_eq!(rules(&report), vec![Rule::MockEvidence]);
    assert!(
        report.violations[0].detail.contains("0 checks"),
        "{}",
        report.violations[0].detail
    );
}

#[test]
fn missing_provenance_is_rejected() {
    let tmp = bundle_at(|m| m.build.binary_sha256 = String::new());
    let report = gate(tmp.path());
    assert_eq!(rules(&report), vec![Rule::MissingProvenance]);
    assert!(
        report.violations[0].detail.contains("build.binary_sha256"),
        "{}",
        report.violations[0].detail
    );
}

#[test]
fn dirty_runner_checkout_is_rejected() {
    let tmp = bundle_at(|m| m.runner.dirty = true);
    let report = gate(tmp.path());
    assert_eq!(rules(&report), vec![Rule::DirtyCheckout]);

    let tmp = bundle_at(|m| m.build.dirty = true);
    let report = gate(tmp.path());
    assert_eq!(rules(&report), vec![Rule::DirtyCheckout]);
    assert!(
        report.violations[0].detail.contains("candidate checkout"),
        "{}",
        report.violations[0].detail
    );
}

#[test]
fn report_build_mismatch_is_rejected() {
    let tmp = valid_bundle();
    // The runner stamped one commit into metadata; the binary that actually ran
    // reported another.
    let path = tmp.path().join("L2").join("metadata.json");
    let mut meta = valid_metadata(2);
    meta.build.commit = "deadbee".to_string();
    std::fs::write(&path, serde_json::to_string(&meta).expect("serialize")).expect("rewrite");

    let report = gate(tmp.path());
    assert!(
        rules(&report).contains(&Rule::BuildMismatch),
        "{}",
        report.render_human()
    );
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.detail.contains("build.commit")),
        "{}",
        report.render_human()
    );
}

#[test]
fn metadata_status_must_match_the_run_report() {
    let tmp = valid_bundle();
    let path = tmp.path().join("L3").join("metadata.json");
    let mut meta = valid_metadata(3);
    meta.run.status = "partial".to_string();
    std::fs::write(&path, serde_json::to_string(&meta).expect("serialize")).expect("rewrite");

    let report = gate(tmp.path());
    assert!(
        rules(&report).contains(&Rule::MalformedMetadata),
        "{}",
        report.render_human()
    );
}

#[test]
fn build_drift_across_levels_is_rejected() {
    let tmp = valid_bundle();
    let mut meta = valid_metadata(3);
    meta.build.binary_sha256 = "0000".to_string();
    write_level(tmp.path(), &meta);

    let report = gate(tmp.path());
    assert!(
        rules(&report).contains(&Rule::BuildMismatch),
        "{}",
        report.render_human()
    );
    assert!(
        report.violations.iter().any(|v| v.level.is_none()),
        "cross-level drift is a bundle-wide finding"
    );
}

#[test]
fn source_digest_drift_across_levels_is_rejected() {
    let tmp = valid_bundle();
    let mut meta = valid_metadata(2);
    meta.source_digests.skills = "sha256:9999".to_string();
    write_level(tmp.path(), &meta);

    let report = gate(tmp.path());
    assert!(
        rules(&report).contains(&Rule::StaleRunner),
        "{}",
        report.render_human()
    );
}

#[test]
fn runner_drift_across_levels_is_rejected() {
    let tmp = valid_bundle();
    let mut meta = valid_metadata(2);
    meta.runner.revision = "runner-old0000".to_string();
    write_level(tmp.path(), &meta);

    let report = gate(tmp.path());
    assert!(
        rules(&report).contains(&Rule::StaleRunner),
        "{}",
        report.render_human()
    );
}

#[test]
fn a_pinned_commit_mismatch_is_a_build_mismatch() {
    let tmp = valid_bundle();
    let pins = Pins {
        commit: Some("ffffffff".to_string()),
        ..Pins::default()
    };
    let report = preflight(&load_bundle(tmp.path()), &pins);
    assert_eq!(rules(&report), vec![Rule::BuildMismatch]);
    assert_eq!(report.violations.len(), 3, "the pin is checked per level");
}

#[test]
fn a_pinned_runner_revision_mismatch_is_a_stale_runner() {
    let tmp = valid_bundle();
    let pins = Pins {
        runner_revision: Some("runner-zzzz".to_string()),
        ..Pins::default()
    };
    let report = preflight(&load_bundle(tmp.path()), &pins);
    assert_eq!(rules(&report), vec![Rule::StaleRunner]);
}

#[test]
fn a_matching_pin_set_passes() {
    let tmp = valid_bundle();
    let reference = valid_metadata(1);
    let pins = Pins {
        commit: Some(reference.build.commit.clone()),
        binary_sha256: Some(reference.build.binary_sha256.clone()),
        runner_revision: Some(reference.runner.revision.clone()),
    };
    let report = preflight(&load_bundle(tmp.path()), &pins);
    assert!(report.passed(), "{}", report.render_human());
}

// -------------------------------------------------------------- comparison --

/// Build a candidate bundle whose L3 differs from the baseline by `mutate`.
fn candidate_vs_baseline(
    mutate: impl Fn(&mut LevelMetadata),
) -> (tempfile::TempDir, tempfile::TempDir) {
    let baseline = valid_bundle();
    let candidate = bundle_at(|m| {
        if m.level == 3 {
            mutate(m);
        }
    });
    (candidate, baseline)
}

/// Compare two bundle roots with the default tolerance and no dispositions.
fn compared(candidate: &Path, baseline: &Path) -> GateReport {
    compare_against_baseline(
        &load_bundle(candidate),
        &Baseline(load_bundle(baseline)),
        &ComparisonInputs::default(),
    )
}

#[test]
fn a_pass_rate_drop_blocks_closure() {
    let (cand, base) = candidate_vs_baseline(|m| {
        m.verifier = VerifierResult {
            checks_total: 10,
            checks_passed: 8,
        }
    });
    let report = compared(cand.path(), base.path());
    assert!(
        rules(&report).contains(&Rule::CorrectnessRegression),
        "{}",
        report.render_human()
    );
    assert!(report.violations.iter().any(|v| v.level == Some(3)));
}

#[test]
fn a_status_regression_blocks_closure() {
    let (cand, base) = candidate_vs_baseline(|m| m.run.status = "deadline_exceeded".to_string());
    let report = compared(cand.path(), base.path());
    assert!(
        rules(&report).contains(&Rule::CorrectnessRegression),
        "{}",
        report.render_human()
    );
}

#[test]
fn a_missing_candidate_level_is_a_missing_deliverable() {
    let baseline = valid_bundle();
    let candidate = valid_bundle();
    std::fs::remove_dir_all(candidate.path().join("L2")).expect("drop L2");

    let report = compared(candidate.path(), baseline.path());
    assert_eq!(rules(&report), vec![Rule::MissingDeliverable]);
    assert_eq!(report.violations[0].level, Some(2));
}

#[test]
fn token_regression_beyond_tolerance_needs_a_disposition() {
    // 1800 total baseline tokens -> 3600: +100%, far outside the 20% default.
    let (cand, base) = candidate_vs_baseline(|m| m.run.tokens.completion = 2300);
    let report = compared(cand.path(), base.path());
    assert_eq!(rules(&report), vec![Rule::UndispositionedChange]);
    assert!(
        report.violations[0].detail.contains("tokens"),
        "{}",
        report.violations[0].detail
    );
    assert!(
        report.violations[0].detail.contains("L3.tokens"),
        "the finding must name the disposition key: {}",
        report.violations[0].detail
    );
}

#[test]
fn turn_and_duration_regressions_are_reported_per_metric() {
    let (cand, base) = candidate_vs_baseline(|m| {
        m.run.turns = 15;
        m.run.duration_secs = 900.0;
    });
    let report = compared(cand.path(), base.path());
    let named: Vec<&str> = report
        .violations
        .iter()
        .map(|v| v.detail.split(' ').next().unwrap_or(""))
        .collect();
    assert!(named.contains(&"turns"), "{named:?}");
    assert!(named.contains(&"duration_secs"), "{named:?}");
}

#[test]
fn a_documented_disposition_clears_a_performance_change() {
    let (cand, base) = candidate_vs_baseline(|m| m.run.turns = 15);
    std::fs::write(
        cand.path().join(DISPOSITIONS_FILE),
        "{\"L3.turns\": \"accepted: the extra turns are the #2265 partial-retry path\"}",
    )
    .expect("write dispositions");

    let inputs = ComparisonInputs::load(&load_bundle(cand.path()), 20.0).expect("load inputs");
    let report = compare_against_baseline(
        &load_bundle(cand.path()),
        &Baseline(load_bundle(base.path())),
        &inputs,
    );

    assert!(report.passed(), "{}", report.render_human());
    assert!(
        report.notes.iter().any(|n| n.contains("dispositioned")),
        "the accepted change must still be recorded: {:?}",
        report.notes
    );
}

#[test]
fn a_small_change_stays_inside_the_default_tolerance() {
    // 10 -> 11 turns is +10%, inside the 20% default.
    let (cand, base) = candidate_vs_baseline(|m| m.run.turns = 11);
    let report = compared(cand.path(), base.path());
    assert!(report.passed(), "{}", report.render_human());
    assert!(
        report.notes.iter().any(|n| n.contains("within tolerance")),
        "an inside-tolerance move is still worth recording: {:?}",
        report.notes
    );
}

#[test]
fn an_improvement_never_needs_a_disposition() {
    let (cand, base) = candidate_vs_baseline(|m| {
        m.run.turns = 4;
        m.run.cost_usd = Some(0.2);
    });
    let report = compared(cand.path(), base.path());
    assert!(report.passed(), "{}", report.render_human());
}

#[test]
fn an_unpriced_baseline_metric_reports_no_change() {
    let baseline = bundle_at(|m| m.run.cost_usd = None);
    let candidate = bundle_at(|m| m.run.cost_usd = Some(5.0));
    let report = compared(candidate.path(), baseline.path());
    assert!(
        report.passed(),
        "a zero baseline has no percentage change to measure: {}",
        report.render_human()
    );
}

#[test]
fn a_malformed_dispositions_file_is_an_error() {
    let tmp = valid_bundle();
    std::fs::write(tmp.path().join(DISPOSITIONS_FILE), "{ nope").expect("write dispositions");
    let err = ComparisonInputs::load(&load_bundle(tmp.path()), 20.0)
        .expect_err("a typoed disposition must not read as no disposition");
    assert!(err.contains(DISPOSITIONS_FILE), "{err}");
}

#[test]
fn an_absent_dispositions_file_loads_as_none() {
    let tmp = valid_bundle();
    let inputs = ComparisonInputs::load(&load_bundle(tmp.path()), 12.5).expect("load inputs");
    assert!(inputs.dispositions.is_empty());
    assert_eq!(inputs.tolerance_pct, 12.5);
}

// ------------------------------------------------------------------ report --

#[test]
fn comparison_findings_join_preflight_findings() {
    let tmp = bundle_at(|m| m.evidence_mode = EvidenceMode::Mock);
    let baseline = valid_bundle();

    let mut report = gate(tmp.path());
    let preflight_count = report.violations.len();
    report.absorb(compare_against_baseline(
        &load_bundle(tmp.path()),
        &Baseline(load_bundle(baseline.path())),
        &ComparisonInputs::default(),
    ));

    assert!(report.violations.len() >= preflight_count);
    assert!(rules(&report).contains(&Rule::MockEvidence));
    assert!(
        report
            .notes
            .iter()
            .any(|n| n.contains("compared against baseline"))
    );
}

#[test]
fn human_render_names_every_violation() {
    let tmp = valid_bundle();
    std::fs::remove_dir_all(tmp.path().join("L1")).expect("drop L1");
    let rendered = gate(tmp.path()).render_human();

    assert!(rendered.starts_with("bakeoff-gate: FAIL"), "{rendered}");
    assert!(rendered.contains("incomplete_coverage"), "{rendered}");
    assert!(rendered.contains("L1"), "{rendered}");
}

#[test]
fn json_render_carries_the_verdict_and_rules() {
    let tmp = bundle_at(|m| m.evidence_mode = EvidenceMode::Mock);
    let parsed: serde_json::Value =
        serde_json::from_str(&gate(tmp.path()).render_json()).expect("valid JSON");

    assert_eq!(parsed["passed"], serde_json::json!(false));
    assert_eq!(parsed["levels"], serde_json::json!([1, 2, 3]));
    assert_eq!(
        parsed["violations"][0]["rule"],
        serde_json::json!("mock_evidence")
    );
    assert_eq!(parsed["violations"][0]["level"], serde_json::json!(1));
}

#[test]
fn a_clean_pass_renders_as_pass_in_both_forms() {
    let tmp = valid_bundle();
    let report = gate(tmp.path());
    assert!(report.render_human().starts_with("bakeoff-gate: PASS"));

    let parsed: serde_json::Value =
        serde_json::from_str(&report.render_json()).expect("valid JSON");
    assert_eq!(parsed["passed"], serde_json::json!(true));
    assert_eq!(parsed["violations"], serde_json::json!([]));
}

#[test]
fn dispositions_are_keyed_by_level_and_metric() {
    // Guards the key format the CLI documents and the violation text prints.
    let mut map = BTreeMap::new();
    map.insert("L3.cost_usd".to_string(), "accepted".to_string());
    let inputs = ComparisonInputs {
        tolerance_pct: 20.0,
        dispositions: map,
    };

    let (cand, base) = candidate_vs_baseline(|m| m.run.cost_usd = Some(2.0));
    let report = compare_against_baseline(
        &load_bundle(cand.path()),
        &Baseline(load_bundle(base.path())),
        &inputs,
    );
    assert!(report.passed(), "{}", report.render_human());
}
