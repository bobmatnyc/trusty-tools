//! Unit tests for `bootstrap::scan`.
//!
//! Why: Separated from `scan.rs` to keep the production module under the
//! 500-SLOC cap while retaining full test coverage of all per-format
//! scanners.
//! What: exercises `scan_project` against fixture directories covering
//! Cargo, npm, Python, Go, CLAUDE.md, .git/config, and the fallback path.
//! Test: this file is the test suite.

use super::scan_project;
use std::fs;
use std::path::Path;

fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(&p, content).expect("write");
}

/// Why: Pin the Cargo.toml scanner against a realistic single-crate
/// manifest. Covers name/version/edition/rust-version extraction.
#[test]
fn scan_project_extracts_cargo_facts() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write(
        tmp.path(),
        "Cargo.toml",
        r#"
[package]
name = "demo-crate"
version = "1.2.3"
edition = "2021"
rust-version = "1.88"
"#,
    );
    let (triples, summary, subject) = scan_project(tmp.path(), "fallback").expect("scan_project");
    assert_eq!(subject, "demo-crate");
    assert!(summary.iter().any(|s| s.file == "Cargo.toml"));

    let has = |p: &str, o: &str| {
        triples
            .iter()
            .any(|t| t.subject == "demo-crate" && t.predicate == p && t.object == o)
    };
    assert!(has("has_language", "Rust"));
    assert!(has("has_version", "1.2.3"));
    assert!(has("has_edition", "2021"));
    assert!(has("has_rust_version", "1.88"));
}

/// Why: Workspace manifests have no `[package]` section but a
/// `[workspace]` table with members; the scanner must still produce
/// workspace-member triples and fall back to the directory name for
/// the subject.
#[test]
fn scan_project_extracts_workspace_members() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("trusty-tools");
    fs::create_dir_all(&root).expect("mkdir");
    write(
        &root,
        "Cargo.toml",
        r#"
[workspace]
members = ["crates/foo", "crates/bar"]
resolver = "2"
"#,
    );
    let (triples, _summary, subject) = scan_project(&root, "fallback").expect("scan_project");
    assert_eq!(subject, "trusty-tools");
    assert!(triples
        .iter()
        .any(|t| t.predicate == "has_workspace_member" && t.object == "crates/foo"));
    assert!(triples
        .iter()
        .any(|t| t.predicate == "has_workspace_member" && t.object == "crates/bar"));
}

/// Why: package.json is the JS/TS entry point; pin name/version + a
/// `has_dependency` triple per top-level dep key.
#[test]
fn scan_project_extracts_package_json() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write(
        tmp.path(),
        "package.json",
        r#"{
  "name": "my-app",
  "version": "0.5.0",
  "dependencies": {
    "react": "^18.0.0",
    "lodash": "^4.0.0"
  }
}"#,
    );
    let (triples, _summary, subject) = scan_project(tmp.path(), "fb").expect("scan");
    assert_eq!(subject, "my-app");
    assert!(triples
        .iter()
        .any(|t| t.predicate == "has_language" && t.object == "JavaScript"));
    assert!(triples
        .iter()
        .any(|t| t.predicate == "has_version" && t.object == "0.5.0"));
    assert!(triples
        .iter()
        .any(|t| t.predicate == "has_dependency" && t.object == "react"));
    assert!(triples
        .iter()
        .any(|t| t.predicate == "has_dependency" && t.object == "lodash"));
}

/// Why: pyproject.toml uses PEP-621 `[project]` table; confirm
/// language/version/requires-python triples land.
#[test]
fn scan_project_extracts_pyproject() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write(
        tmp.path(),
        "pyproject.toml",
        r#"
[project]
name = "pydemo"
version = "2.0.1"
requires-python = ">=3.10"
"#,
    );
    let (triples, _summary, subject) = scan_project(tmp.path(), "fb").expect("scan");
    assert_eq!(subject, "pydemo");
    assert!(triples
        .iter()
        .any(|t| t.predicate == "has_language" && t.object == "Python"));
    assert!(triples
        .iter()
        .any(|t| t.predicate == "has_version" && t.object == "2.0.1"));
    assert!(triples
        .iter()
        .any(|t| t.predicate == "requires_python" && t.object == ">=3.10"));
}

/// Why: Go modules name themselves in `go.mod`; confirm module-name
/// extraction + language tag.
#[test]
fn scan_project_extracts_go_mod() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write(
        tmp.path(),
        "go.mod",
        "module github.com/example/widget\n\ngo 1.22\n",
    );
    let (triples, _summary, subject) = scan_project(tmp.path(), "fb").expect("scan");
    assert_eq!(subject, "github.com/example/widget");
    assert!(triples
        .iter()
        .any(|t| t.predicate == "has_language" && t.object == "Go"));
}

/// Why: CLAUDE.md's first H1 becomes the project description; pin the
/// extractor against a typical heading + leading frontmatter.
#[test]
fn scan_project_extracts_claude_md_h1() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write(
        tmp.path(),
        "Cargo.toml",
        r#"
[package]
name = "demo"
version = "0.1.0"
"#,
    );
    write(
        tmp.path(),
        "CLAUDE.md",
        "\n\n# Demo Project — orientation guide\n\nSome body text.\n",
    );
    let (triples, _summary, _subject) = scan_project(tmp.path(), "fb").expect("scan");
    assert!(triples.iter().any(|t| t.subject == "demo"
        && t.predicate == "has_description"
        && t.object == "Demo Project — orientation guide"));
}

/// Why: .git/config is the canonical source-repo URL; confirm extraction
/// across SSH-style URLs.
#[test]
fn scan_project_extracts_git_origin() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write(
        tmp.path(),
        "Cargo.toml",
        r#"
[package]
name = "demo"
version = "0.1.0"
"#,
    );
    write(
        tmp.path(),
        ".git/config",
        "[core]\n\trepositoryformatversion = 0\n[remote \"origin\"]\n\turl = git@github.com:example/demo.git\n",
    );
    let (triples, _summary, _) = scan_project(tmp.path(), "fb").expect("scan");
    assert!(triples
        .iter()
        .any(|t| t.predicate == "source_repo" && t.object == "git@github.com:example/demo.git"));
}

/// Why: When no manifest matches, the fallback subject (palace id) must
/// be returned so temporal triples still have a stable anchor.
#[test]
fn scan_project_falls_back_to_palace_id_when_no_manifest() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (triples, summary, subject) = scan_project(tmp.path(), "my-palace").expect("scan");
    assert_eq!(subject, "my-palace");
    assert!(triples.is_empty());
    assert!(summary.is_empty());
}
