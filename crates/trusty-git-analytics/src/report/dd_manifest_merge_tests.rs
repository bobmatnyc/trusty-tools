//! Tests for #6190's merging write.
//!
//! The fixture is the shape the 2026-08-23 incident produced: a manifest tga
//! wrote, then trusty-audit's grounding pass edited — `inspect_priority`,
//! `[repositories.crate_topology]`, and the `investigate_*` budget keys, none
//! of which tga knows exist.

use std::path::PathBuf;

use super::super::dd_manifest::{DdManifest, DdManifestError, DdReportSection, DdRepositoryEntry};
use super::merge_into;

/// A manifest as trusty-audit leaves it: tga's keys plus the grounding pass's.
const GROUNDED: &str = r#"
[report]
title = "Acme — Technical Due Diligence"
analyst = "J. Reviewer"
gaps = ["jira sync: no JIRA project configured"]
investigate_max_files = 240
investigate_max_bytes = 1200000
attributed_only = true

[[repositories]]
name = "acme-api"
path = "/srv/checkouts/acme-api"
inspect_priority = ["src/auth.rs", "src/billing.rs"]

[repositories.crate_topology]
members = 12
edges = 31
cycles = 0
"#;

/// The manifest a second `tga audit --output <same dir>` builds: thin, and
/// naming only what tga itself collects.
fn fresh() -> DdManifest {
    DdManifest {
        report: DdReportSection {
            title: "Acme — Technical Due Diligence".to_string(),
            analyst: None,
            client: Some("Acme Holdings".to_string()),
            gaps: vec!["linear sync: no Linear workspace configured".to_string()],
            ticketing: Some(PathBuf::from("ticketing.json")),
        },
        repositories: vec![DdRepositoryEntry {
            name: "acme-api".to_string(),
            path: PathBuf::from("/srv/checkouts/acme-api"),
            authorship: Some(PathBuf::from("authorship-0.json")),
        }],
    }
}

/// #6190, the regression this ticket is: against the replacing write, every
/// assertion in the first block fails — `tga audit --output` into a live
/// engagement threw away the investigation scope trusty-audit had grounded, and
/// collapsed the run from 226 findings to 31 with no error anywhere.
#[test]
fn a_rebuilt_manifest_never_destroys_the_grounded_investigation_scope() {
    // The write this ticket removed, kept here as the baseline: `to_toml` is
    // what `tga audit` used to hand `fs::write`, and it names none of the three
    // key families below. Asserting it is what makes the delta a fact in the
    // test rather than a claim in the commit message.
    let replacing = fresh().to_toml().expect("serialize");
    for key in [
        "inspect_priority",
        "crate_topology",
        "investigate_max_files",
    ] {
        assert!(
            !replacing.contains(key),
            "the pre-#6190 write carried `{key}` — this baseline is wrong: {replacing}"
        );
    }

    let merged = merge_into(&fresh(), GROUNDED).expect("a grounded manifest is valid TOML");

    // The three key families the incident named survive verbatim.
    assert!(
        merged.contains("inspect_priority") && merged.contains("src/billing.rs"),
        "the evidence ranking must survive a tga re-run: {merged}"
    );
    assert!(
        merged.contains("crate_topology") && merged.contains("edges"),
        "the crate topology must survive a tga re-run: {merged}"
    );
    assert!(
        merged.contains("investigate_max_files = 240")
            && merged.contains("investigate_max_bytes = 1200000"),
        "the investigation budget must survive a tga re-run: {merged}"
    );
    // And so does every other key neither closure condition names.
    assert!(
        merged.contains("attributed_only = true"),
        "the rule is 'tga rewrites only its own keys', not a preserve-list: {merged}"
    );

    // tga's own keys are this run's values.
    assert!(
        merged.contains(r#"client = "Acme Holdings""#)
            && merged.contains(r#"authorship = "authorship-0.json""#)
            && merged.contains(r#"ticketing = "ticketing.json""#),
        "this run's own contributions must reach the file: {merged}"
    );

    // No duplicate entry: the fresh repository matched the grounded one by path.
    assert_eq!(
        merged.matches("[[repositories]]").count(),
        1,
        "matching by path is what keeps the ranking attached to one entry: {merged}"
    );

    // The merged text is still a manifest both tools can read back.
    let reparsed: toml::Value = toml::from_str(&merged).expect("the merge emits valid TOML");
    assert_eq!(
        reparsed["repositories"][0]["inspect_priority"][0].as_str(),
        Some("src/auth.rs")
    );
}

/// An absent flag says "tga has nothing to state here", never "delete what the
/// file states". `--analyst` was passed on the first run and not the second;
/// the delivered report must not lose the analyst's name because of it.
#[test]
fn an_absent_flag_keeps_the_value_the_file_already_carries() {
    let merged = merge_into(&fresh(), GROUNDED).expect("merge");
    assert!(
        merged.contains(r#"analyst = "J. Reviewer""#),
        "an absent --analyst must not delete the declared one: {merged}"
    );
}

/// The one key that is deliberately replaced rather than preserved — a gap line
/// states what THIS run could not assess, and a stale one is a false statement
/// inside the delivered report.
#[test]
fn this_runs_gaps_replace_the_files_rather_than_accumulating() {
    let merged = merge_into(&fresh(), GROUNDED).expect("merge");
    assert!(
        merged.contains("no Linear workspace configured"),
        "this run's gaps must reach the file: {merged}"
    );
    assert!(
        !merged.contains("no JIRA project configured"),
        "a previous run's gap must not be asserted about this one: {merged}"
    );
}

/// A fresh manifest naming a repository the file does not know appends an
/// entry; one the file knows and this run does not is left where it is. Both
/// directions of "touch only what you own".
#[test]
fn entries_are_added_without_disturbing_the_ones_this_run_does_not_name() {
    let mut manifest = fresh();
    manifest.repositories[0] = DdRepositoryEntry {
        name: "acme-web".to_string(),
        path: PathBuf::from("/srv/checkouts/acme-web"),
        authorship: None,
    };

    let merged = merge_into(&manifest, GROUNDED).expect("merge");
    let reparsed: toml::Value = toml::from_str(&merged).expect("valid TOML");
    let repos = reparsed["repositories"].as_array().expect("array");
    assert_eq!(repos.len(), 2, "the new repository is appended: {merged}");
    assert_eq!(repos[0]["name"].as_str(), Some("acme-api"));
    assert!(
        repos[0].get("inspect_priority").is_some(),
        "an entry this run does not name keeps everything it had: {merged}"
    );
    assert_eq!(repos[1]["name"].as_str(), Some("acme-web"));
}

/// A checkout that moved between runs still matches on `name`, so its ranking
/// stays on one entry instead of splitting across two.
#[test]
fn a_moved_checkout_matches_on_name_when_the_path_changed() {
    let no_path = r#"
[[repositories]]
name = "acme-api"
inspect_priority = ["src/auth.rs"]
"#;
    let merged = merge_into(&fresh(), no_path).expect("merge");
    assert_eq!(
        merged.matches("[[repositories]]").count(),
        1,
        "the name fallback must not produce a second entry: {merged}"
    );
    assert!(
        merged.contains("inspect_priority") && merged.contains("/srv/checkouts/acme-api"),
        "the ranking stays and the path is updated: {merged}"
    );
}

/// Refusing is the point: a file this crate cannot parse is a file whose
/// contents it cannot promise to preserve, and the only alternative is the
/// replacing write #6190 removed.
#[test]
fn an_unparseable_manifest_is_refused_not_replaced() {
    let err = merge_into(&fresh(), "this is not = = TOML [[[")
        .expect_err("an unparseable manifest must never be silently replaced");
    assert!(matches!(err, DdManifestError::MergeSource(_)));
    assert!(
        format!("{err}").contains("move it aside"),
        "the error must tell the operator what to do: {err}"
    );

    // A `report` of the wrong shape is refused for the same reason.
    let wrong_shape = merge_into(&fresh(), "report = 3\n")
        .expect_err("a `report` that is not a table cannot be merged into");
    assert!(matches!(wrong_shape, DdManifestError::MergeSource(_)));
}

/// The no-existing-file path is unchanged: `to_toml_merged(None)` is byte-
/// identical to the `to_toml` every run before this ticket produced.
#[test]
fn a_first_run_writes_exactly_what_it_always_did() {
    let manifest = fresh();
    assert_eq!(
        manifest.to_toml_merged(None).expect("serialize"),
        manifest.to_toml().expect("serialize"),
        "a fresh output directory must not change shape"
    );
}

/// Merging into an empty file is the same as writing a fresh one: an empty
/// document has nothing to preserve, so the result is a complete manifest.
#[test]
fn an_empty_manifest_file_yields_a_complete_manifest() {
    let merged = merge_into(&fresh(), "").expect("an empty document is valid TOML");
    let reparsed: toml::Value = toml::from_str(&merged).expect("valid TOML");
    assert_eq!(
        reparsed["report"]["title"].as_str(),
        Some("Acme — Technical Due Diligence")
    );
    assert_eq!(
        reparsed["repositories"][0]["name"].as_str(),
        Some("acme-api")
    );
}
