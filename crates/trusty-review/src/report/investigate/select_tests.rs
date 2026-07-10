//! Tests for deterministic file selection + coverage (#2357).
//!
//! Why: selection is the deterministic gate on what the LLM sees; ranking,
//! budget caps, truncation, and coverage must be exact and reproducible.
//! What: builds a temp fixture repo with planted auth/store/package/error/test
//! files and asserts ranking, the file/byte caps, per-file truncation, dimension
//! classification, keyword extraction, and coverage.  No LLM, no network.
//! Test: included as `#[cfg(test)] mod tests` from `select.rs`.

use std::fs;
use std::path::{Path, PathBuf};

use super::*;
use crate::report::scan::list_tracked_files;

/// Create `root/rel` with `content`, making parent dirs.
fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

/// Build a fixture repo covering several DD dimensions; returns (tmp, root).
fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::TempDir::new().unwrap();
    let r = tmp.path();
    write(
        r,
        "src/auth/login.rs",
        "fn login() { let token = read_secret(); }\n",
    );
    write(
        r,
        "src/store/user_store.ts",
        "export const store = createStore();\n",
    );
    write(r, "src/errors.rs", "pub enum Error { NotFound }\n");
    write(
        r,
        "package.json",
        "{\"dependencies\": {\"react\": \"^18\"}}\n",
    );
    write(
        r,
        "README.md",
        "# Project\n\nSome prose that is not code.\n",
    );
    write(r, "tests/login_test.rs", "#[test] fn t() {}\n");
    tmp
}

/// Why: an instruction-matched, dimension-matched code file must rank first.
/// What: with a brief mentioning "login", the auth file outranks the rest.
/// Test: this test itself.
#[test]
fn ranks_relevant_first() {
    let tmp = fixture();
    let files = list_tracked_files(tmp.path());
    let sel = select_files(
        tmp.path(),
        &files,
        Some("Focus on the login and authentication flow."),
        Budget::default(),
    );
    assert!(!sel.is_empty());
    assert!(
        sel.files[0].path.contains("auth/login"),
        "auth/login should rank first, got {}",
        sel.files[0].path
    );
}

/// Why: the file cap is a hard budget.
/// What: max_files = 2 selects exactly 2 and reports the rest skipped.
/// Test: this test itself.
#[test]
fn budget_caps_file_count() {
    let tmp = fixture();
    let files = list_tracked_files(tmp.path());
    let total = files.len();
    let sel = select_files(
        tmp.path(),
        &files,
        None,
        Budget {
            max_files: 2,
            max_bytes: DEFAULT_MAX_BYTES,
        },
    );
    assert_eq!(sel.files.len(), 2);
    assert_eq!(sel.total_files, total);
    assert_eq!(sel.skipped, total - 2);
}

/// Why: the byte cap is a hard budget.
/// What: a tiny max_bytes keeps bytes_sent within the cap and drops files.
/// Test: this test itself.
#[test]
fn budget_caps_total_bytes() {
    let tmp = fixture();
    let files = list_tracked_files(tmp.path());
    let sel = select_files(
        tmp.path(),
        &files,
        None,
        Budget {
            max_files: 40,
            max_bytes: 30,
        },
    );
    assert!(sel.bytes_sent <= 30, "bytes_sent {} > cap", sel.bytes_sent);
    assert!(sel.files.len() < files.len());
}

/// Why: an oversize file must be truncated with the visible marker so the byte
/// budget is respected and the reader knows content was cut.
/// What: plants a >24KB file and asserts it is truncated.
/// Test: this test itself.
#[test]
fn truncates_oversize_file() {
    let tmp = tempfile::TempDir::new().unwrap();
    let big = "x".repeat(30 * 1024);
    write(tmp.path(), "src/big.rs", &big);
    let files = list_tracked_files(tmp.path());
    let sel = select_files(tmp.path(), &files, None, Budget::default());
    let f = sel
        .files
        .iter()
        .find(|f| f.path.contains("big.rs"))
        .unwrap();
    assert!(f.truncated);
    assert!(f.content.contains(TRUNCATION_MARKER.trim()));
    assert!(f.content.len() < big.len());
}

/// Why: coverage must report which DD dimensions were reached and which were not.
/// What: the fixture covers auth/deps/state/error/tests but not scalability.
/// Test: this test itself.
#[test]
fn coverage_reports_dimensions() {
    let tmp = fixture();
    let files = list_tracked_files(tmp.path());
    let sel = select_files(tmp.path(), &files, None, Budget::default());
    let covered = &sel.dimensions_covered;
    assert!(covered.iter().any(|d| d == "authentication & secrets"));
    assert!(covered.iter().any(|d| d == "dependencies"));
    assert!(covered.iter().any(|d| d == "state management"));
    assert!(covered.iter().any(|d| d == "error handling"));
    assert!(covered.iter().any(|d| d == "test coverage"));
    assert!(
        sel.dimensions_absent.iter().any(|d| d == "scalability"),
        "scalability has no fixture file and must be absent"
    );
}

/// Why: the path heuristics are the deterministic classifier steering selection.
/// What: spot-checks each dimension's path pattern.
/// Test: this test itself.
#[test]
fn dimension_heuristics_classify() {
    assert!(
        dimensions_for("src/auth/mod.rs")
            .iter()
            .any(|d| d == "authentication & secrets")
    );
    assert!(
        dimensions_for("config/settings.rs")
            .iter()
            .any(|d| d == "authentication & secrets")
    );
    assert!(
        dimensions_for("Cargo.toml")
            .iter()
            .any(|d| d == "dependencies")
    );
    assert!(
        dimensions_for("src/state/reducer.ts")
            .iter()
            .any(|d| d == "state management")
    );
    assert!(
        dimensions_for("src/errors.rs")
            .iter()
            .any(|d| d == "error handling")
    );
    assert!(
        dimensions_for("src/queue/worker.rs")
            .iter()
            .any(|d| d == "scalability")
    );
    assert!(dimensions_for("src/main.rs").is_empty());
}

/// Why: instruction keywords steer ranking; short tokens are noise.
/// What: keeps ≥4-char tokens, lower-cases, dedupes, drops short words.
/// Test: this test itself.
#[test]
fn instruction_keywords_extracted() {
    let kw = instruction_keywords(Some("Check the AUTH and db pooling; auth auth."));
    assert!(kw.contains(&"auth".to_string()));
    assert!(kw.contains(&"pooling".to_string()));
    assert!(!kw.iter().any(|k| k == "db"), "short tokens dropped");
    assert_eq!(kw.iter().filter(|k| *k == "auth").count(), 1, "deduped");
    assert!(instruction_keywords(None).is_empty());
}

/// Why: a non-existent path yields no files, and selection is empty (not a panic).
/// What: selects over an empty file list.
/// Test: this test itself.
#[test]
fn empty_repo_selects_nothing() {
    let sel = select_files(
        Path::new("/nonexistent-xyz"),
        &[] as &[PathBuf],
        None,
        Budget::default(),
    );
    assert!(sel.is_empty());
    assert_eq!(sel.total_files, 0);
}
