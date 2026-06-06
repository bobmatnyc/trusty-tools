//! Tests for `Assignments` parsing, path safety, structural validation, and
//! the wave-ordering topological repair (#88, #114, #152, #162).
//!
//! Why: Pins the wave-decomposition contract the engine's wave loop depends on
//! — safe relative paths, sequential ordinals, no duplicates, earlier-wave deps,
//! and best-effort topological repair of fixable orderings.
//! What: A `#[cfg(test)]` submodule; `super::super::*` reaches the `config`
//! module and `super::fa` is the shared `FileAssignment` builder.
//! Test: This file IS the test body.

use super::super::*;
use super::fa;

#[test]
fn assignments_load_returns_none_for_missing_file() {
    // #88: Absent assignments.json → None so the engine falls back to
    // monolithic code phase.
    let tmp = tempfile::tempdir().unwrap();
    let loaded = Assignments::load(tmp.path());
    assert!(loaded.is_none());
}

#[test]
fn assignments_load_parses_valid_json() {
    // #88: A two-wave document round-trips through Assignments::load.
    let tmp = tempfile::tempdir().unwrap();
    let raw = r#"{
        "error_convention": "exceptions",
        "waves": [
            {
                "wave": 1,
                "files": [
                    {
                        "path": "src/util.py",
                        "stub": "util.py",
                        "purpose": "helpers",
                        "max_lines": 120
                    }
                ]
            },
            {
                "wave": 2,
                "files": [
                    {
                        "path": "src/main.py",
                        "stub": "main.py",
                        "purpose": "entrypoint",
                        "depends_on": ["src/util.py"]
                    }
                ]
            }
        ]
    }"#;
    std::fs::write(tmp.path().join("assignments.json"), raw).unwrap();
    let a = Assignments::load(tmp.path()).expect("parses");
    assert_eq!(a.error_convention.as_deref(), Some("exceptions"));
    assert_eq!(a.waves.len(), 2);
    assert_eq!(a.waves[0].wave, 1);
    assert_eq!(a.waves[0].files.len(), 1);
    assert_eq!(a.waves[0].files[0].path, "src/util.py");
    assert_eq!(a.waves[0].files[0].max_lines, Some(120));
    assert!(a.waves[0].files[0].depends_on.is_empty());
    assert_eq!(a.waves[1].files[0].depends_on, vec!["src/util.py"]);
}

#[test]
fn assignments_load_returns_none_for_invalid_json() {
    // #88: Malformed JSON must not crash the engine — just return None.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("assignments.json"), "{ not json").unwrap();
    let loaded = Assignments::load(tmp.path());
    assert!(loaded.is_none());
}

#[test]
fn file_assignment_stub_can_be_null() {
    // Plan-agent emits `"stub": null` for files with no scaffold (e.g.
    // `__init__.py`). Serde must accept null without rejecting the document.
    let raw = r#"{
        "waves": [
            {
                "wave": 1,
                "files": [
                    {
                        "path": "src/__init__.py",
                        "stub": null,
                        "purpose": "package marker"
                    }
                ]
            }
        ]
    }"#;
    let a: Assignments = serde_json::from_str(raw).expect("null stub must parse");
    assert_eq!(a.waves[0].files[0].stub, None);
}

// ---- #114: Assignments validation tests ----

#[test]
fn validate_file_path_accepts_safe_relative() {
    assert!(Assignments::validate_file_path("src/foo.rs").is_ok());
    assert!(Assignments::validate_file_path("a/b/c/d.py").is_ok());
    assert!(Assignments::validate_file_path("file.txt").is_ok());
}

#[test]
fn validate_file_path_rejects_absolute() {
    // #114: absolute paths must be rejected to prevent out_dir escape.
    let err = Assignments::validate_file_path("/etc/passwd").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("absolute"), "got: {msg}");
    assert!(msg.contains("/etc/passwd"), "got: {msg}");
}

#[test]
fn validate_file_path_rejects_parent_traversal() {
    let err = Assignments::validate_file_path("../escape.py").unwrap_err();
    assert!(err.to_string().contains(".."), "got: {err}");

    let err2 = Assignments::validate_file_path("src/../../etc/passwd").unwrap_err();
    assert!(err2.to_string().contains(".."), "got: {err2}");
}

#[test]
fn validate_file_path_rejects_empty() {
    let err = Assignments::validate_file_path("").unwrap_err();
    assert!(err.to_string().contains("empty"), "got: {err}");
}

#[test]
fn validate_file_path_rejects_bare_dotdot() {
    let err = Assignments::validate_file_path("..").unwrap_err();
    assert!(err.to_string().contains(".."), "got: {err}");
}

#[test]
fn validate_assignments_accepts_valid_two_wave() {
    let asg = Assignments {
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
    assert!(asg.validate().is_ok());
}

#[test]
fn validate_assignments_rejects_non_sequential_ordinals() {
    let asg = Assignments {
        error_convention: None,
        waves: vec![
            WaveDef {
                wave: 1,
                files: vec![fa("a.py", vec![])],
            },
            WaveDef {
                wave: 3,
                files: vec![fa("b.py", vec!["a.py"])],
            },
        ],
    };
    let err = asg.validate().unwrap_err().to_string();
    assert!(err.contains("expected ordinal 2"), "got: {err}");
}

#[test]
fn validate_assignments_rejects_unsafe_path() {
    let asg = Assignments {
        error_convention: None,
        waves: vec![WaveDef {
            wave: 1,
            files: vec![fa("../../etc/passwd", vec![])],
        }],
    };
    let err = asg.validate().unwrap_err().to_string();
    assert!(err.contains(".."), "got: {err}");
}

#[test]
fn validate_assignments_rejects_duplicate_paths() {
    let asg = Assignments {
        error_convention: None,
        waves: vec![
            WaveDef {
                wave: 1,
                files: vec![fa("a.py", vec![])],
            },
            WaveDef {
                wave: 2,
                files: vec![fa("a.py", vec![])],
            },
        ],
    };
    let err = asg.validate().unwrap_err().to_string();
    assert!(err.contains("duplicate"), "got: {err}");
}

#[test]
fn validate_assignments_rejects_forward_or_same_wave_dep() {
    let asg_same = Assignments {
        error_convention: None,
        waves: vec![WaveDef {
            wave: 1,
            files: vec![fa("a.py", vec![]), fa("b.py", vec!["a.py"])],
        }],
    };
    let err = asg_same.validate().unwrap_err().to_string();
    assert!(err.contains("not in an earlier wave"), "got: {err}");

    let asg_fwd = Assignments {
        error_convention: None,
        waves: vec![
            WaveDef {
                wave: 1,
                files: vec![fa("a.py", vec!["b.py"])],
            },
            WaveDef {
                wave: 2,
                files: vec![fa("b.py", vec![])],
            },
        ],
    };
    let err = asg_fwd.validate().unwrap_err().to_string();
    assert!(err.contains("not in an earlier wave"), "got: {err}");
}

#[test]
fn validate_assignments_rejects_empty_waves() {
    let asg = Assignments {
        error_convention: None,
        waves: vec![],
    };
    let err = asg.validate().unwrap_err().to_string();
    assert!(err.contains("no waves"), "got: {err}");
}

#[test]
fn assignments_load_rejects_path_traversal_via_disk() {
    // #114: A plan-agent emitting `../../etc/passwd` must NOT cause the
    // wave loop to run. `load()` returns None (with an ERROR log) and
    // the engine falls back to the monolithic code phase.
    let tmp = tempfile::tempdir().unwrap();
    let raw = r#"{
        "waves": [
            {
                "wave": 1,
                "files": [
                    {
                        "path": "../../etc/passwd",
                        "stub": null,
                        "purpose": "attack"
                    }
                ]
            }
        ]
    }"#;
    std::fs::write(tmp.path().join("assignments.json"), raw).unwrap();
    let loaded = Assignments::load(tmp.path());
    assert!(
        loaded.is_none(),
        "path-traversal assignments must be rejected"
    );
}

#[test]
fn assignments_load_rejects_absolute_path_via_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let raw = r#"{
        "waves": [
            {
                "wave": 1,
                "files": [
                    {
                        "path": "/etc/passwd",
                        "stub": null,
                        "purpose": "attack"
                    }
                ]
            }
        ]
    }"#;
    std::fs::write(tmp.path().join("assignments.json"), raw).unwrap();
    let loaded = Assignments::load(tmp.path());
    assert!(
        loaded.is_none(),
        "absolute-path assignment must be rejected"
    );
}

// ---- #152: Flat-path heuristic warning tests ----

#[test]
fn validate_file_path_accepts_proper_directory_path() {
    // #152: Properly structured paths with slashes must pass without error.
    assert!(
        Assignments::validate_file_path("src/doc_pipeline/stages/extraction.py").is_ok(),
        "directory-structured path must be accepted"
    );
    assert!(
        Assignments::validate_file_path("tests/test_api.py").is_ok(),
        "test path with one directory level must be accepted"
    );
    assert!(
        Assignments::validate_file_path("app/routers/users.py").is_ok(),
        "nested path must be accepted"
    );
}

#[test]
fn validate_file_path_accepts_legitimate_flat_file() {
    // #152: Short flat filenames like conftest.py or __init__.py must pass
    // without triggering the heuristic warning (they are not encoding
    // directory structure — they are genuinely flat files).
    assert!(
        Assignments::validate_file_path("conftest.py").is_ok(),
        "conftest.py is a legitimate flat file"
    );
    assert!(
        Assignments::validate_file_path("__init__.py").is_ok(),
        "__init__.py is a legitimate flat file"
    );
    assert!(
        Assignments::validate_file_path("my_module.py").is_ok(),
        "short flat file with one underscore must be accepted"
    );
}

#[test]
fn validate_file_path_accepts_suspiciously_flat_but_non_fatal() {
    // #152: A long flat filename that looks like encoded directory structure
    // (e.g. stages_extraction_helpers.py) must still PASS validate_file_path
    // (the check is non-fatal — it only emits a warning). The result is Ok.
    assert!(
        Assignments::validate_file_path("stages_extraction_helpers.py").is_ok(),
        "heuristic warning for flat path must be non-fatal"
    );
    assert!(
        Assignments::validate_file_path("src_doc_pipeline_stages_extraction.py").is_ok(),
        "very long flat path must be non-fatal (warning only)"
    );
}

// #162 wave-validator topological repair tests live in `tests/wave_repair.rs`
// to keep this file under the 500-line cap.
