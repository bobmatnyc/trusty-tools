//! Tests for `Assignments::repair_wave_ordering` topological repair (#162).
//!
//! Why: Pins the contract that fixable orderings (same-wave / forward deps) are
//! linearized via Kahn's algorithm while true cycles are rejected, both
//! in-memory and through the on-disk `load` path.
//! What: A `#[cfg(test)]` submodule; `super::super::*` reaches the `config`
//! module and `super::fa` is the shared `FileAssignment` builder.
//! Test: This file IS the test body.

use super::super::*;
use super::fa;

#[test]
fn wave_validator_repairs_same_wave_dep() {
    // #162: A depends on B but both are in wave 1 → repair moves B to
    // wave 1 and A to wave 2. The original validate() would reject.
    let mut asg = Assignments {
        error_convention: None,
        waves: vec![WaveDef {
            wave: 1,
            files: vec![fa("a.py", vec!["b.py"]), fa("b.py", vec![])],
        }],
    };
    // Pre-repair validation must fail with an ordering violation.
    assert!(asg.validate().is_err(), "same-wave dep must fail validate");

    let changed = asg
        .repair_wave_ordering()
        .expect("acyclic graph must repair");
    assert!(changed, "repair must report changes");

    // Post-repair: B in wave 1, A in wave 2, and validate() passes.
    assert_eq!(asg.waves.len(), 2);
    assert_eq!(asg.waves[0].wave, 1);
    assert_eq!(asg.waves[1].wave, 2);
    let wave1_paths: Vec<&str> = asg.waves[0].files.iter().map(|f| f.path.as_str()).collect();
    let wave2_paths: Vec<&str> = asg.waves[1].files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(wave1_paths, vec!["b.py"]);
    assert_eq!(wave2_paths, vec!["a.py"]);
    assert!(
        asg.validate().is_ok(),
        "repaired assignments must pass validation"
    );
}

#[test]
fn wave_validator_rejects_true_cycle() {
    // #162: A depends on B, B depends on A → no topological ordering
    // exists; repair must return Err(cycle message).
    let mut asg = Assignments {
        error_convention: None,
        waves: vec![WaveDef {
            wave: 1,
            files: vec![fa("a.py", vec!["b.py"]), fa("b.py", vec!["a.py"])],
        }],
    };
    let err = asg
        .repair_wave_ordering()
        .expect_err("cycle must not be repairable");
    assert!(
        err.contains("cycle") || err.contains("linearize"),
        "error should mention cycle, got: {err}"
    );
}

#[test]
fn wave_validator_handles_conftest_pattern() {
    // #162: Realistic case — conftest.py and several test_*.py files are
    // all in wave 1 because the plan-agent didn't recognize the import
    // relationship. After repair, conftest.py must move to an earlier
    // wave than every test file that depends on it.
    let mut asg = Assignments {
        error_convention: None,
        waves: vec![WaveDef {
            wave: 1,
            files: vec![
                fa("tests/conftest.py", vec![]),
                fa("tests/test_alpha.py", vec!["tests/conftest.py"]),
                fa("tests/test_beta.py", vec!["tests/conftest.py"]),
                fa("tests/test_gamma.py", vec!["tests/conftest.py"]),
            ],
        }],
    };

    let changed = asg
        .repair_wave_ordering()
        .expect("conftest pattern is a DAG");
    assert!(changed);
    assert_eq!(asg.waves.len(), 2);
    // conftest must be alone (or at least present) in wave 1.
    let wave1_paths: Vec<&str> = asg.waves[0].files.iter().map(|f| f.path.as_str()).collect();
    let wave2_paths: Vec<&str> = asg.waves[1].files.iter().map(|f| f.path.as_str()).collect();
    assert!(
        wave1_paths.contains(&"tests/conftest.py"),
        "conftest must be in wave 1, got {wave1_paths:?}"
    );
    for tf in [
        "tests/test_alpha.py",
        "tests/test_beta.py",
        "tests/test_gamma.py",
    ] {
        assert!(
            wave2_paths.contains(&tf),
            "{tf} must be in wave 2, got {wave2_paths:?}"
        );
    }
    assert!(asg.validate().is_ok());
}

#[test]
fn wave_validator_repair_noop_on_valid_plan() {
    // #162: An already-valid plan must report no changes (repair returns
    // Ok(false)) and leave the waves untouched.
    let mut asg = Assignments {
        error_convention: None,
        waves: vec![
            WaveDef {
                wave: 1,
                files: vec![fa("a.py", vec![])],
            },
            WaveDef {
                wave: 2,
                files: vec![fa("b.py", vec!["a.py"])],
            },
        ],
    };
    let changed = asg
        .repair_wave_ordering()
        .expect("valid plan must not error");
    assert!(!changed, "valid plan must not be modified");
    assert_eq!(asg.waves.len(), 2);
    assert_eq!(asg.waves[0].files[0].path, "a.py");
    assert_eq!(asg.waves[1].files[0].path, "b.py");
}

#[test]
fn wave_validator_load_repairs_same_wave_via_disk() {
    // #162: End-to-end — a plan with a same-wave violation written to
    // disk must load successfully (repair kicks in) instead of returning
    // None and falling back to monolithic execution.
    let tmp = tempfile::tempdir().unwrap();
    let raw = r#"{
        "waves": [
            {
                "wave": 1,
                "files": [
                    {"path": "tests/conftest.py", "stub": null, "purpose": "shared fixtures"},
                    {"path": "tests/test_api.py", "stub": null, "purpose": "api tests",
                     "depends_on": ["tests/conftest.py"]}
                ]
            }
        ]
    }"#;
    std::fs::write(tmp.path().join("assignments.json"), raw).unwrap();
    let a = Assignments::load(tmp.path()).expect("load must repair, not reject");
    assert_eq!(a.waves.len(), 2, "repair must produce 2 waves");
    assert_eq!(a.waves[0].files[0].path, "tests/conftest.py");
    assert_eq!(a.waves[1].files[0].path, "tests/test_api.py");
}

#[test]
fn wave_validator_load_rejects_true_cycle_via_disk() {
    // #162: A real cycle in assignments.json must return None (fall back
    // to monolithic) because repair cannot linearize it.
    let tmp = tempfile::tempdir().unwrap();
    let raw = r#"{
        "waves": [
            {
                "wave": 1,
                "files": [
                    {"path": "a.py", "stub": null, "purpose": "a",
                     "depends_on": ["b.py"]},
                    {"path": "b.py", "stub": null, "purpose": "b",
                     "depends_on": ["a.py"]}
                ]
            }
        ]
    }"#;
    std::fs::write(tmp.path().join("assignments.json"), raw).unwrap();
    let loaded = Assignments::load(tmp.path());
    assert!(loaded.is_none(), "true cycle must be rejected");
}
