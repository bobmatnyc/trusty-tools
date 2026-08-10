//! Tests for the DD-manifest adapter (#5236, DOC-67 §6).
//!
//! Why: the adapter is pure precisely so its three obligations can be proved
//! here rather than by running an audit — the §6 field mapping, §9's byte-level
//! determinism, and the hard requirement that no configured credential reaches
//! an artifact handed to a third party.
//! What: drives `build_dd_manifest` against fixture `Config` values.
//! Test: this file.

use std::path::PathBuf;

use super::*;
use crate::core::config::{GithubConfig, RepositoryConfig};

/// A config naming `n` repositories, with no credentials.
fn config_with_repos(entries: &[(&str, Option<&str>)]) -> Config {
    Config {
        repositories: entries
            .iter()
            .map(|(path, name)| RepositoryConfig {
                path: PathBuf::from(path),
                name: name.map(str::to_string),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

fn options(title: &str) -> DdManifestOptions {
    DdManifestOptions {
        title: title.to_string(),
        ..Default::default()
    }
}

/// Why: §6's table is the contract between two processes; every row it fixes
/// must actually appear, and every field it says to omit must stay absent so
/// trusty-review's own defaults (slug derivation, git_ref, live `--analyze`)
/// apply.
/// Test: itself.
#[test]
fn maps_engagement_metadata() {
    let cfg = config_with_repos(&[("/src/northwind-web", Some("Northwind Web"))]);
    let opts = DdManifestOptions {
        title: "Acme — Technical Due Diligence".to_string(),
        analyst: Some("J. Reviewer".to_string()),
        client: Some("Acme Holdings".to_string()),
        gaps: vec!["Stage `dora` did not complete.".to_string()],
        ..Default::default()
    };

    let manifest = build_dd_manifest(&cfg, &opts).expect("builds");

    assert_eq!(manifest.report.title, "Acme — Technical Due Diligence");
    assert_eq!(manifest.report.analyst.as_deref(), Some("J. Reviewer"));
    assert_eq!(manifest.report.client.as_deref(), Some("Acme Holdings"));
    assert_eq!(manifest.report.gaps.len(), 1);
    assert_eq!(
        manifest.repositories,
        vec![DdRepositoryEntry {
            name: "Northwind Web".to_string(),
            path: PathBuf::from("/src/northwind-web"),
        }]
    );

    // Omitted-by-design keys never appear: `slug` is trusty-review's to derive,
    // `ref` follows HEAD, and a declared `metrics` file would block the live
    // `--analyze` fetch every AUDIT run depends on (§6).
    let toml = manifest.to_toml().expect("serializes");
    for absent in ["slug", "ref =", "metrics", "template"] {
        assert!(!toml.contains(absent), "{absent} must not appear:\n{toml}");
    }
}

/// Why: absent metadata must stay absent rather than becoming an empty string —
/// the template's own fallback ("not stated in source report") is what §2's
/// no-interactivity rule relies on.
/// Test: itself.
#[test]
fn absent_metadata_is_omitted_not_blanked() {
    let cfg = config_with_repos(&[("/src/a", Some("A"))]);
    let toml = build_dd_manifest(&cfg, &options("T"))
        .expect("builds")
        .to_toml()
        .expect("serializes");

    assert!(!toml.contains("analyst"), "{toml}");
    assert!(!toml.contains("client"), "{toml}");
    assert!(!toml.contains("gaps"), "{toml}");
}

/// Why: `RepositoryConfig.name` is optional and its documented fallback is the
/// directory basename; an audit of an org-discovered repo set relies on it.
/// Test: itself.
#[test]
fn names_fall_back_to_the_directory_basename() {
    let cfg = config_with_repos(&[
        ("/src/northwind-web", None),
        ("/src/billing", Some("  ")),
        ("/src/ledger", Some("Ledger Service")),
    ]);

    let manifest = build_dd_manifest(&cfg, &options("T")).expect("builds");
    let names: Vec<&str> = manifest
        .repositories
        .iter()
        .map(|r| r.name.as_str())
        .collect();

    assert_eq!(names, vec!["northwind-web", "billing", "Ledger Service"]);
}

/// Why: repository order is the report's application order; a set that reorders
/// between runs would make two audits of the same state incomparable.
/// Test: itself.
#[test]
fn repositories_keep_config_order() {
    let cfg = config_with_repos(&[("/src/c", None), ("/src/a", None), ("/src/b", None)]);
    let manifest = build_dd_manifest(&cfg, &options("T")).expect("builds");
    let names: Vec<&str> = manifest
        .repositories
        .iter()
        .map(|r| r.name.as_str())
        .collect();
    assert_eq!(names, vec!["c", "a", "b"], "config order, never sorted");
}

/// Why: DOC-67 §9 — the same DB and config must produce a byte-identical
/// manifest. A clock read, an environment read, or a hash-ordered collection in
/// the builder would break this and nothing else would catch it.
/// Test: itself.
#[test]
fn two_builds_are_byte_identical() {
    let cfg = config_with_repos(&[
        ("/src/northwind-web", Some("Northwind Web")),
        ("/src/billing", None),
    ]);
    let opts = DdManifestOptions {
        title: "Acme — Technical Due Diligence".to_string(),
        analyst: Some("J. Reviewer".to_string()),
        client: None,
        gaps: vec!["a".to_string(), "b".to_string()],
        ..Default::default()
    };

    let first = build_dd_manifest(&cfg, &opts).expect("builds");
    let second = build_dd_manifest(&cfg, &opts).expect("builds");

    assert_eq!(first, second);
    assert_eq!(
        first.to_toml().expect("serializes"),
        second.to_toml().expect("serializes"),
        "same input, same bytes"
    );
}

/// Why: the manifest is handed to a third party. A credential reaching it is
/// the one failure in this module that cannot be walked back, so the guarantee
/// is tested at the boundary a token can realistically cross — an expanded
/// `${GITHUB_TOKEN}` echoed into a stage's error message, and from there into a
/// gap line.
/// Test: itself.
#[test]
fn configured_token_never_reaches_the_manifest() {
    let token = "ghp_TESTONLYtoken0123456789abcdef"; // pragma: allowlist secret
    let mut cfg = config_with_repos(&[("/src/a", Some("A"))]);
    cfg.github = Some(GithubConfig {
        token: Some(token.to_string()),
        ..Default::default()
    });

    // Every string channel that reaches the file, each carrying the token the
    // way a real failure would: an API error body quoted into a stage message.
    let opts = DdManifestOptions {
        title: format!("Acme ({token})"),
        analyst: Some(format!("analyst {token}")),
        client: Some(format!("client {token}")),
        gaps: vec![format!(
            "Stage `collect` did not complete: GET /orgs/acme/repos returned 401 for {token}"
        )],
        ..Default::default()
    };

    let toml = build_dd_manifest(&cfg, &opts)
        .expect("builds")
        .to_toml()
        .expect("serializes");

    assert!(
        !toml.contains(token),
        "token leaked into the manifest:\n{toml}"
    );
    assert!(
        toml.contains("[REDACTED]"),
        "the value must be replaced, not merely dropped:\n{toml}"
    );
    // The surrounding diagnostic survives — scrubbing must not destroy the gap.
    assert!(toml.contains("Stage `collect` did not complete"), "{toml}");
}

/// Why: trusty-review rejects a manifest with no repositories, and a message
/// naming `config.yaml` is far more actionable than that rejection.
/// Test: itself.
#[test]
fn empty_config_is_an_actionable_error() {
    let err = build_dd_manifest(&Config::default(), &options("T")).expect_err("no repositories");
    assert!(matches!(err, DdManifestError::NoRepositories));
    assert!(err.to_string().contains("config.yaml"), "{err}");
}

/// Why: the file is only useful if the consumer parses it, and the consumer is
/// a different crate with its own validation. Serializing text that
/// trusty-review would reject is a defect this crate can catch alone.
/// Test: itself.
#[test]
fn round_trips_through_the_review_schema() {
    let cfg = config_with_repos(&[("/src/a", Some("A")), ("/src/b", None)]);
    let opts = DdManifestOptions {
        title: "T".to_string(),
        analyst: Some("An".to_string()),
        client: None,
        gaps: vec!["one gap".to_string()],
        ..Default::default()
    };
    let toml = build_dd_manifest(&cfg, &opts)
        .expect("builds")
        .to_toml()
        .expect("serializes");

    // The shape trusty-review's `load_manifest` requires: a `[report]` table
    // with a title, and one `[[repositories]]` entry per repo carrying exactly
    // one source key (`path`).
    let parsed: toml::Value = toml::from_str(&toml).expect("valid TOML");
    assert_eq!(parsed["report"]["title"].as_str(), Some("T"));
    assert_eq!(parsed["report"]["gaps"].as_array().map(Vec::len), Some(1));
    let repos = parsed["repositories"].as_array().expect("array of tables");
    assert_eq!(repos.len(), 2);
    for repo in repos {
        assert!(repo.get("path").is_some(), "local source key required");
        assert!(repo.get("remote").is_none(), "exactly one source key");
        assert!(repo.get("name").is_some());
    }
}

/// Why: tga resolves a relative repository path against its working directory
/// and trusty-review resolves one against the manifest's directory — and the
/// manifest lands in the output directory, somewhere else entirely. Copying the
/// path through verbatim silently points the renderer at a checkout that does
/// not exist, and the only symptom is a report with no analysis and no stated
/// reason. Caught by the end-to-end smoke run for #5238.
/// Test: itself.
#[test]
fn relative_paths_are_anchored_to_base_dir() {
    let cfg = config_with_repos(&[("./small-repo", Some("Small")), ("/abs/other", Some("Abs"))]);
    let opts = DdManifestOptions {
        title: "T".to_string(),
        base_dir: PathBuf::from("/work/audit"),
        ..Default::default()
    };

    let manifest = build_dd_manifest(&cfg, &opts).expect("builds");

    assert_eq!(
        manifest.repositories[0].path,
        PathBuf::from("/work/audit/./small-repo"),
        "a relative path is anchored, never passed through"
    );
    assert_eq!(
        manifest.repositories[1].path,
        PathBuf::from("/abs/other"),
        "an absolute path is untouched"
    );

    // An empty base leaves paths exactly as configured.
    let bare = build_dd_manifest(&cfg, &options("T")).expect("builds");
    assert_eq!(bare.repositories[0].path, PathBuf::from("./small-repo"));
}
