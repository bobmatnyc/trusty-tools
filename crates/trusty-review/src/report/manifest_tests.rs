//! Tests for the report manifest loader and validation.
//!
//! Why: the manifest is the user-facing entry point; its parse + validation
//! rules (exactly one source per entry, non-empty list) must be pinned so a
//! malformed manifest always fails with the precise error.
//! What: exercises both source kinds, the two mutual-exclusion errors, the
//! empty-manifest error, orphaned-username tolerance, and slug derivation.
//! Test: included as `#[cfg(test)] mod tests` from `manifest.rs`.

use std::path::Path;

use super::{ManifestError, RepositorySource, parse_manifest, slugify};

fn parse(text: &str) -> std::result::Result<super::Manifest, ManifestError> {
    parse_manifest(text, Path::new("manifest.toml"))
}

/// Why: a local-path entry is the common case and must parse into `LocalPath`.
/// What: parses a single local entry and asserts fields + source kind.
/// Test: this test itself.
#[test]
fn parse_local_path_entry() {
    let toml = r#"
        [report]
        title = "Acme DD"
        analyst = "bobmatnyc"

        [[repositories]]
        name = "Website"
        path = "/tmp/acme-web"
        ref = "main"
        metrics = "acme.json"
    "#;
    let m = parse(toml).expect("parse ok");
    assert_eq!(m.report.title, "Acme DD");
    assert_eq!(m.report.analyst.as_deref(), Some("bobmatnyc"));
    assert_eq!(m.repositories.len(), 1);
    let r = &m.repositories[0];
    assert_eq!(r.name, "Website");
    assert_eq!(r.slug, "website");
    assert_eq!(r.git_ref.as_deref(), Some("main"));
    assert!(matches!(r.source, RepositorySource::LocalPath { .. }));
}

/// Why: a remote entry must parse into `Remote` and retain the username.
/// What: parses a single remote entry and asserts source kind + username.
/// Test: this test itself.
#[test]
fn parse_remote_entry() {
    let toml = r#"
        [report]
        title = "Remote DD"

        [[repositories]]
        name = "API"
        remote = "bobmatnyc/trusty-tools"
        username = "bobmatnyc"
        ref = "v1.2.3"
    "#;
    let m = parse(toml).expect("parse ok");
    let r = &m.repositories[0];
    assert_eq!(r.username.as_deref(), Some("bobmatnyc"));
    match &r.source {
        RepositorySource::Remote { remote } => assert_eq!(remote, "bobmatnyc/trusty-tools"),
        other => panic!("expected remote, got {other:?}"),
    }
}

/// Why: a manifest with no repositories cannot produce a report.
/// What: asserts `NoRepositories` for a report-only manifest.
/// Test: this test itself.
#[test]
fn no_repositories_errors() {
    let toml = r#"
        [report]
        title = "Empty"
    "#;
    let err = parse(toml).expect_err("must error");
    assert!(matches!(err, ManifestError::NoRepositories));
}

/// Why: declaring both `path` and `remote` is ambiguous and must be rejected.
/// What: asserts `ConflictingSources` naming the offending entry.
/// Test: this test itself.
#[test]
fn conflicting_sources_error() {
    let toml = r#"
        [report]
        title = "Conflict"

        [[repositories]]
        name = "Both"
        path = "/tmp/x"
        remote = "owner/repo"
    "#;
    let err = parse(toml).expect_err("must error");
    match err {
        ManifestError::ConflictingSources { name } => assert_eq!(name, "Both"),
        other => panic!("expected ConflictingSources, got {other:?}"),
    }
}

/// Why: an entry with neither source cannot be analyzed and must be rejected.
/// What: asserts `MissingSource` naming the offending entry.
/// Test: this test itself.
#[test]
fn missing_source_error() {
    let toml = r#"
        [report]
        title = "Missing"

        [[repositories]]
        name = "Neither"
    "#;
    let err = parse(toml).expect_err("must error");
    match err {
        ManifestError::MissingSource { name } => assert_eq!(name, "Neither"),
        other => panic!("expected MissingSource, got {other:?}"),
    }
}

/// Why: a username on a local entry is meaningless but must not fail the load
/// (documented behaviour: kept + warned, not an error).
/// What: asserts a local entry with a username parses and retains the username.
/// Test: this test itself.
#[test]
fn orphaned_username_tolerated() {
    let toml = r#"
        [report]
        title = "Orphan"

        [[repositories]]
        name = "Local"
        path = "/tmp/local"
        username = "ignored"
    "#;
    let m = parse(toml).expect("parse ok — username tolerated");
    assert_eq!(m.repositories[0].username.as_deref(), Some("ignored"));
    assert!(matches!(
        m.repositories[0].source,
        RepositorySource::LocalPath { .. }
    ));
}

/// Why: bad TOML must surface a parse error, not a silent empty manifest.
/// What: asserts `Parse` for malformed input.
/// Test: this test itself.
#[test]
fn bad_toml_parse_error() {
    let err = parse("this is not = valid = toml").expect_err("must error");
    assert!(matches!(err, ManifestError::Parse { .. }));
}

/// Why: slug derivation must be stable and filesystem-safe.
/// What: asserts lowercasing, non-alnum collapse, trimming, and empty fallback.
/// Test: this test itself.
#[test]
fn slug_derivation() {
    assert_eq!(slugify("Acme Web App"), "acme-web-app");
    assert_eq!(slugify("  Foo//Bar  "), "foo-bar");
    assert_eq!(slugify("!!!"), "report");
    assert_eq!(slugify("Already-slugged"), "already-slugged");
}
