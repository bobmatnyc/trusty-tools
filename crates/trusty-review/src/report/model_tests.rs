//! `ReportModel::build`'s authorship fail-open contract (#5453, #6004).
//!
//! Why: `RepositoryReport::authorship` and `ReportModel::build` both promise
//! that a declared-but-unreadable authorship artifact becomes a NAMED GAP
//! rather than a failed build — the opposite of how a declared metrics or
//! ticketing file behaves. Nothing proved that until these tests; the contract
//! existed only in prose.

use std::path::Path;

use crate::report::manifest::parse_manifest;
use crate::report::model::ReportModel;

/// The artifact tga writes, as trusty-review reads it back.
const ARTIFACT: &str = r#"{
  "schema_version": "v0",
  "repository": "acme-web",
  "distinct_authors": 4,
  "bus_factor": 1,
  "top_author_share_pct": 71.5,
  "single_author_subsystems": ["migrations"],
  "monthly_trajectory": [
    {"month": "2026-01", "active_authors": 2, "commits": 10}
  ],
  "unresolved_authors": 0,
  "caveats": ["no vendored-path exclusion"]
}"#;

/// Build a one-repository manifest declaring `authorship = <name>`, then build
/// the model from it. Returns the built model so a caller can assert on both
/// the loaded figures and the gap list.
fn model_with_authorship(dir: &Path, declared: &str) -> ReportModel {
    let toml = format!(
        r#"
        [report]
        title = "Acme Due Diligence"
        analyst = "bobmatnyc"

        [[repositories]]
        name = "Acme Web"
        path = "/nonexistent/acme-web"
        authorship = "{declared}"
    "#
    );
    let manifest_path = dir.join("manifest.toml");
    let manifest = parse_manifest(&toml, &manifest_path).expect("manifest parse");
    ReportModel::build(&manifest, &manifest_path, "report-technical-dd", None)
        .expect("a declared authorship path must never fail the build")
}

/// A declared artifact that reads and parses lands on the repository, resolved
/// against the MANIFEST's directory (the path is relative, as tga writes it),
/// and contributes no gap.
#[test]
fn authorship_loads_when_declared() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("authorship-0.json"), ARTIFACT).expect("write artifact");

    let model = model_with_authorship(dir.path(), "authorship-0.json");

    let a = model.repositories[0]
        .authorship
        .as_ref()
        .expect("a readable artifact must reach the model");
    assert_eq!(a.repository, "acme-web");
    assert_eq!(a.distinct_authors, 4);
    assert_eq!(a.bus_factor, 1);
    assert_eq!(a.single_author_subsystems, vec!["migrations".to_string()]);
    assert!(
        !model.gaps.iter().any(|g| g.starts_with("Authorship (")),
        "a successful load states no gap: {:?}",
        model.gaps
    );
}

/// The fail-open half: an unreadable artifact leaves `authorship: None`, adds
/// one gap line naming the repository, and — the point of the contract —
/// returns `Ok`, so a report with real data for every other section still
/// renders.
///
/// Against a `?`-propagating load (the shape `metrics` and `ticketing` use)
/// `ReportModel::build` returns `Err` here and the whole report is lost.
#[test]
fn unreadable_authorship_is_a_named_gap_not_a_build_failure() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("authorship-0.json"), "{not json").expect("write artifact");

    let model = model_with_authorship(dir.path(), "authorship-0.json");

    assert!(
        model.repositories[0].authorship.is_none(),
        "a failed load must not leave half-parsed figures behind"
    );
    let gap = model
        .gaps
        .iter()
        .find(|g| g.starts_with("Authorship ("))
        .expect("the failure must be stated, never silent");
    assert!(
        gap.contains("Acme Web"),
        "the gap must name the repository it belongs to: {gap}"
    );
    assert!(
        gap.contains("no authorship/key-person signal"),
        "the gap must say what the report will state instead: {gap}"
    );
}

/// A declared path that does not exist at all takes the same fail-open route as
/// a malformed one — the two failures reach the reader identically, which is
/// what lets the section degrade to one gap line either way.
#[test]
fn a_missing_authorship_file_is_the_same_named_gap() {
    let dir = tempfile::tempdir().expect("tempdir");

    let model = model_with_authorship(dir.path(), "authorship-0.json");

    assert!(model.repositories[0].authorship.is_none());
    assert!(
        model.gaps.iter().any(|g| g.starts_with("Authorship (")),
        "a missing file is stated, not skipped: {:?}",
        model.gaps
    );
}

/// Why: #6135 replaced #6114's refusal with an attribution, so the line the
/// report shows IS the guarantee — an adjusted id must read `requested → ran`
/// and an unadjusted one must not pretend anything changed.
/// What: builds the record from two resolutions, one of each kind.
/// Test: this test itself.
#[test]
fn attribution_renders_requested_and_ran() {
    use crate::config::Provider;
    use crate::llm::resolve_model;
    use crate::report::model::{InferenceAttribution, RoleAttribution};

    let straight = resolve_model("anthropic/claude-opus-4.8", &Provider::OpenRouter)
        .expect("an agreeing pair resolves");
    let adjusted = resolve_model("bedrock/anthropic/claude-sonnet-4.6", &Provider::OpenRouter)
        .expect("a translatable id resolves");

    let record = InferenceAttribution::of(
        "the manifest's [inference] section",
        vec![
            RoleAttribution::of("reviewer", "anthropic/claude-opus-4.8", &straight),
            RoleAttribution::of("verifier", "bedrock/anthropic/claude-sonnet-4.6", &adjusted),
        ],
    );

    assert_eq!(
        record.provider, "openrouter",
        "the reviewer's provider leads"
    );
    let line = record.line();
    assert!(
        line.contains("reviewer: anthropic/claude-opus-4.8"),
        "an unadjusted role shows one id: {line}"
    );
    assert!(
        line.contains(
            "verifier: bedrock/anthropic/claude-sonnet-4.6 → us.anthropic.claude-sonnet-4-6"
        ),
        "an adjusted role shows both halves: {line}"
    );
    assert!(
        line.contains("the manifest's [inference] section"),
        "the line states which layer selected the models: {line}"
    );
}
