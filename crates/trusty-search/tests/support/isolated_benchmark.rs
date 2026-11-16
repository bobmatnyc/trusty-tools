//! Corpus and readiness checks for benchmarks that may delete index data.
//!
//! The daemon is socket-only since #9214, so these benchmarks reach a sandbox
//! daemon (`scripts/sandbox_search_daemon.sh`) through
//! `socket_benchmark::sandbox_client`, which reads `TRUSTY_SEARCH_BENCH_SOCKET`
//! and refuses the live install's socket (#10677).
//! Set TRUSTY_SEARCH_TEST_CORPUS_ROOT to a disposable workspace copy without
//! .git, and create .trusty-search-test-corpus in it to acknowledge its purpose.
//! Register only copied roots.

#![allow(dead_code)] // Each benchmark uses a different subset of the helpers.

use std::path::{Path, PathBuf};

pub fn validate_corpus(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let root = root
        .canonicalize()
        .map_err(|e| format!("invalid corpus root: {e}"))?;
    if !root.join(".trusty-search-test-corpus").is_file() || root.join(".git").exists() {
        return Err(
            "corpus must be a disposable copy marked .trusty-search-test-corpus without .git"
                .into(),
        );
    }
    let path = root
        .join(relative)
        .canonicalize()
        .map_err(|e| format!("missing copied corpus {relative}: {e}"))?;
    if !path.starts_with(&root) || !path.is_dir() {
        return Err("benchmark corpus must be a directory inside the disposable copy".into());
    }
    Ok(path)
}

/// Refuse a stale index registration pointing outside the requested copied root.
pub fn validate_registered_root(actual: &Path, expected: &Path) -> Result<(), String> {
    let actual = actual
        .canonicalize()
        .map_err(|e| format!("invalid registered root: {e}"))?;
    let expected = expected
        .canonicalize()
        .map_err(|e| format!("invalid expected root: {e}"))?;
    if actual != expected {
        return Err(format!(
            "registered root {} differs from copied corpus {}",
            actual.display(),
            expected.display()
        ));
    }
    Ok(())
}

/// Check only enabled lanes; incomplete fixtures are a prerequisite failure.
pub fn validate_ready(status: &serde_json::Value) -> Result<(), String> {
    for (stage, disabled) in [
        ("lexical", false),
        ("semantic", status["skip_vector"].as_bool() == Some(true)),
        ("graph", status["skip_kg"].as_bool() == Some(true)),
    ] {
        if !disabled && status["stages"][stage]["status"].as_str() != Some("ready") {
            return Err(format!(
                "enabled {stage} stage is not ready: {}",
                status["stages"][stage]
            ));
        }
    }
    Ok(())
}

/// Resolve a source directory in the fixture workspace, checking symlink escapes.
pub fn corpus_root(relative: &str) -> PathBuf {
    let root = std::env::var_os("TRUSTY_SEARCH_TEST_CORPUS_ROOT")
        .expect("set TRUSTY_SEARCH_TEST_CORPUS_ROOT to a marked disposable workspace copy");
    validate_corpus(Path::new(&root), relative).unwrap_or_else(|error| panic!("{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "trusty-benchmark-guard-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn readiness_rejects_missing_in_progress_and_failed_enabled_stages() {
        assert!(validate_ready(&serde_json::json!({})).is_err());
        for stage in ["lexical", "semantic", "graph"] {
            for state in ["in_progress", "failed", "not_started"] {
                let mut status = serde_json::json!({"stages": {
                    "lexical": {"status": "ready"}, "semantic": {"status": "ready"},
                    "graph": {"status": "ready"}
                }});
                status["stages"][stage]["status"] = serde_json::json!(state);
                assert!(validate_ready(&status).is_err(), "accepted {stage}={state}");
            }
        }
    }

    #[test]
    fn readiness_accepts_ready_stages_and_explicitly_disabled_optional_lanes() {
        assert!(validate_ready(&serde_json::json!({"stages": {
            "lexical": {"status": "ready"}, "semantic": {"status": "ready"},
            "graph": {"status": "ready"}
        }}))
        .is_ok());
        assert!(validate_ready(&serde_json::json!({
            "skip_kg": true, "skip_vector": true,
            "stages": {"lexical": {"status": "ready"}}
        }))
        .is_ok());
        assert!(validate_ready(&serde_json::json!({
            "skip_kg": true, "skip_vector": true,
            "stages": {"lexical": {"status": "in_progress"}}
        }))
        .is_err());
    }

    #[test]
    fn rejects_existing_index_registered_to_another_root() {
        let expected = Fixture::new();
        let unrelated = Fixture::new();
        assert!(validate_registered_root(&unrelated.0, &expected.0).is_err());
        assert!(validate_registered_root(&expected.0, &expected.0).is_ok());
    }

    #[test]
    fn rejects_unmarked_corpus_and_real_checkout() {
        let fixture = Fixture::new();
        assert!(validate_corpus(&fixture.0, "").is_err());
        std::fs::write(fixture.0.join(".trusty-search-test-corpus"), "").unwrap();
        std::fs::write(fixture.0.join(".git"), "gitdir: checkout").unwrap();
        assert!(validate_corpus(&fixture.0, "").is_err());
    }

    #[test]
    fn accepts_existing_directory_inside_marked_copy() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.join(".trusty-search-test-corpus"), "").unwrap();
        std::fs::create_dir(fixture.0.join("crates")).unwrap();
        assert_eq!(
            validate_corpus(&fixture.0, "crates").unwrap(),
            fixture.0.join("crates").canonicalize().unwrap()
        );
        assert!(validate_corpus(&fixture.0, "missing").is_err());
    }

    #[test]
    fn rejects_parent_escape() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.join(".trusty-search-test-corpus"), "").unwrap();
        assert!(validate_corpus(&fixture.0, "..").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        let fixture = Fixture::new();
        let outside = Fixture::new();
        std::fs::write(fixture.0.join(".trusty-search-test-corpus"), "").unwrap();
        std::os::unix::fs::symlink(&outside.0, fixture.0.join("crates")).unwrap();
        assert!(validate_corpus(&fixture.0, "crates").is_err());
    }
}
