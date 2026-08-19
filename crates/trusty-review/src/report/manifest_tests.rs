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

/// Why: #5239 — an orchestrator's unassessed areas travel to the report through
/// the manifest, and a manifest that declares none must behave exactly as before.
/// Test: itself.
#[test]
fn parse_declared_gaps() {
    let m = parse_manifest(
        "[report]\ntitle = \"T\"\ngaps = [\"Stage `dora` did not complete.\", \"No CVE scan.\"]\n\n\
         [[repositories]]\nname = \"A\"\npath = \"/x\"\n",
        Path::new("m.toml"),
    )
    .expect("parses");
    assert_eq!(
        m.report.gaps,
        vec![
            "Stage `dora` did not complete.".to_string(),
            "No CVE scan.".to_string()
        ]
    );

    let none = parse_manifest(
        "[report]\ntitle = \"T\"\n\n[[repositories]]\nname = \"A\"\npath = \"/x\"\n",
        Path::new("m.toml"),
    )
    .expect("parses");
    assert!(none.report.gaps.is_empty(), "absent key defaults to empty");
}

/// #5405: the ticketing artifact rides on `[report]`, and `metrics` rides on a
/// repository entry. The fixture declares both, so each must land in its own
/// field with the other's value untouched — `metrics` is asserted nowhere else
/// in this file (`parse_local_path_entry` declares the key and never reads it).
#[test]
fn parse_ticketing_path() {
    let m = parse_manifest(
        "[report]\ntitle = \"T\"\nticketing = \"ticketing.json\"\n\n\
         [[repositories]]\nname = \"A\"\npath = \"/x\"\nmetrics = \"acme.json\"\n",
        Path::new("m.toml"),
    )
    .expect("parses");
    assert_eq!(
        m.report.ticketing,
        Some(std::path::PathBuf::from("ticketing.json"))
    );
    assert_eq!(
        m.repositories[0].metrics,
        Some(std::path::PathBuf::from("acme.json")),
        "a declared metrics file must survive alongside a declared ticketing artifact"
    );

    let none = parse_manifest(
        "[report]\ntitle = \"T\"\n\n[[repositories]]\nname = \"A\"\npath = \"/x\"\n",
        Path::new("m.toml"),
    )
    .expect("parses");
    assert!(
        none.report.ticketing.is_none(),
        "absent key defaults to None, which renders the section as unassessed"
    );
}

/// Why: `inspect_priority` is the interface trusty-audit writes its selection
/// ranking to, and it must accept both a bare path list (hand-written) and a
/// weighted table (generated) without a second key.
/// What: parses a mixed list and asserts each entry's path and weight.
/// Test: this test itself.
#[test]
fn parse_inspect_priority_shapes() {
    let toml = r#"
        [report]
        title = "Acme DD"

        [[repositories]]
        name = "Website"
        path = "/tmp/acme-web"
        inspect_priority = [
            "src/auth/login.rs",
            { path = "src/billing.rs", weight = 5000 },
            { path = "src/queue.rs" },
        ]
    "#;
    let m = parse(toml).expect("parse ok");
    let p = &m.repositories[0].inspect_priority;
    assert_eq!(p.len(), 3);
    assert_eq!(p[0].path, "src/auth/login.rs");
    assert_eq!(p[1].path, "src/billing.rs");
    assert_eq!(
        p[1].weight, 5000,
        "an explicit weight wins over the position"
    );
    assert_eq!(p[2].path, "src/queue.rs");
}

/// Why: the declared order IS the ranking, so an unweighted list must come out
/// strictly descending — otherwise the selection sort breaks ties by path and
/// scrambles what the ranker asked for.
/// What: three bare paths get 1000/999/998; an absent key yields an empty list,
/// which is what keeps selection byte-identical to a pre-#6078 manifest.
/// Test: this test itself.
#[test]
fn inspect_priority_weights_follow_declared_rank() {
    let toml = r#"
        [report]
        title = "Acme DD"

        [[repositories]]
        name = "Website"
        path = "/tmp/acme-web"
        inspect_priority = ["c.rs", "b.rs", "a.rs"]
    "#;
    let m = parse(toml).expect("parse ok");
    let weights: Vec<u32> = m.repositories[0]
        .inspect_priority
        .iter()
        .map(|p| p.weight)
        .collect();
    assert_eq!(weights, vec![1000, 999, 998]);

    let none = parse("[report]\ntitle = \"T\"\n\n[[repositories]]\nname = \"A\"\npath = \"/x\"\n")
        .expect("parses");
    assert!(
        none.repositories[0].inspect_priority.is_empty(),
        "absent key must leave selection byte-identical to a pre-#6078 manifest"
    );
}
