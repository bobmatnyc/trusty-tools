//! Tests for the bundled framework-tier manifest (#4760).
//!
//! Why: this module is the one place "which agents always deploy" is decided, so
//! its two properties must be pinned independently — the SHIPPED asset validates
//! (a build-integrity guard), and the composition rule behaves correctly against
//! SYNTHETIC categories (so a behaviour test cannot pass by accident of what the
//! real manifest happens to contain).
//! What: validation-failure coverage for every [`FrameworkManifestError`]
//! variant, composition coverage for each of the four deployment categories, the
//! skill-roster declaration (#4765), and the "no marker detected" vs "manifest
//! is broken" distinction.
//! Test: this file.

use super::*;
use std::fs;
use tempfile::TempDir;

/// A synthetic catalog whose names are deliberately NOT real bundled agents, so
/// a composition test can never pass because of what the shipped manifest says.
fn fake_catalog() -> BTreeSet<String> {
    [
        "rust-engineer",
        "js-engineer",
        "react-engineer",
        "vercel-ops",
        "qa",
        "ops",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

/// A synthetic manifest over [`fake_catalog`], exercising all five categories.
///
/// Since #4765 the markers are part of the fixture, so every composition test
/// below is independent of the SHIPPED markers as well as of the shipped roster.
fn fake_manifest() -> String {
    r#"
version = 1

[agent_categories]
universal = ["qa"]
deprecated = ["ops"]

[[agent_categories.language]]
stem = "rust-engineer"
markers = ["Cargo.toml"]

[[agent_categories.language]]
stem = "js-engineer"
markers = ["package.json"]

[[agent_categories.framework]]
stem = "react-engineer"
markers = ["package.json::\"react\""]

[[agent_categories.platform]]
stem = "vercel-ops"
markers = ["vercel.json"]

[skill_categories]
universal = ["tm"]
"#
    .to_string()
}

/// A synthetic skill catalog, deliberately not the real bundled one.
fn fake_skill_catalog() -> BTreeSet<String> {
    ["tm"].iter().map(|s| (*s).to_string()).collect()
}

fn parsed_fake() -> AgentCategories {
    parse_framework_manifest(&fake_manifest(), &fake_catalog()).expect("fixture is valid")
}

fn touch(dir: &Path, name: &str) {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, "").unwrap();
}

// ---------------------------------------------------------------------------
// The shipped asset
// ---------------------------------------------------------------------------

#[test]
fn bundled_framework_manifest_is_valid() {
    // The build-integrity guard `framework_agent_scope`'s panic branch relies
    // on. If this fails, the shipped asset is corrupt and the panic is live.
    let categories = framework_agent_categories().expect("bundled manifest must validate");
    assert!(!categories.universal.is_empty());
    assert_eq!(
        categories
            .platform
            .iter()
            .map(|entry| entry.stem.as_str())
            .collect::<Vec<_>>(),
        vec!["gcp-ops", "vercel-ops"],
        "the two platform-ops agents are platform-gated"
    );
    // #4765: the markers are the manifest's too, so the shipped gate is pinned
    // here rather than in a Rust table nobody reads next to the category.
    let entry = |list: &[GatedAgent], stem: &str| {
        list.iter()
            .find(|e| e.stem == stem)
            .unwrap_or_else(|| panic!("{stem} must be declared"))
            .markers
            .clone()
    };
    assert_eq!(
        entry(&categories.language, "elixir-engineer"),
        vec!["mix.exs".to_string()],
        "elixir-engineer is language-gated on mix.exs"
    );
    assert_eq!(
        entry(&categories.framework, "phoenix-engineer"),
        vec!["mix.exs::{:phoenix,".to_string()],
        "phoenix-engineer is framework-gated on the phoenix dependency"
    );
    // #4760: `deprecated` is EMPTY and that is correct. `ops` was its only
    // occupant and the owner ruled it deleted from the bundle outright, so it
    // needs no declaration. The category remains as the MECHANISM for retiring
    // a future agent that must stay bundled; its behaviour is proven against a
    // synthetic catalog by `deprecated_agent_never_deploys`, which does not
    // depend on whichever agent happens to be retired today.
    assert!(categories.deprecated.is_empty());
}

#[test]
fn bundled_agent_stems_excludes_foundations() {
    let stems = bundled_agent_stems();
    assert!(
        stems.contains("engineer"),
        "dispatchable agents are included"
    );
    assert!(
        stems.contains("elixir-engineer"),
        "#4760 added elixir-engineer to the bundle"
    );
    assert!(
        !stems.contains("ops"),
        "#4760 deleted the deprecated `ops` agent from the bundle"
    );
    for base in [
        "BASE-AGENT",
        "BASE-ENGINEER",
        "BASE-OPS",
        "BASE-QA",
        "BASE-RESEARCH",
    ] {
        assert!(
            !stems.contains(base),
            "{base} is an inheritance fragment, not a dispatchable agent"
        );
    }
}

#[test]
fn bundled_manifest_partitions_the_whole_catalog() {
    // Every bundled agent is declared exactly once — the invariant that makes a
    // newly added agent fail loudly instead of quietly never deploying.
    let categories = framework_agent_categories().expect("valid");
    let declared: Vec<String> = labelled_stems(&categories)
        .into_iter()
        .map(|(_, stem)| stem.clone())
        .collect();
    let mut sorted = declared.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), declared.len(), "no stem declared twice");
    assert_eq!(
        sorted,
        bundled_agent_stems().into_iter().collect::<Vec<_>>(),
        "the five categories must exactly cover the bundled catalog"
    );
}

/// Whether `stem` deploys under `scope`, by the same rule the deployer applies.
///
/// Why: the composition states its gate as an `exclude` list, so asserting on
/// list membership would pin the mechanism. These tests assert the behaviour the
/// deployer actually consults — `HarnessPlan::agent_selected`'s rule.
fn deploys(scope: &AgentSet, stem: &str) -> bool {
    super::super::schema::selection_matches(stem, &scope.include, &scope.exclude)
}

#[test]
fn framework_agent_scope_selects_universal_agents() {
    // The real entry point over the real asset: a bare directory still gets the
    // universal roster, and never the deprecated agent.
    let tmp = TempDir::new().unwrap();
    let scope = framework_agent_scope(tmp.path());
    assert!(deploys(&scope, "engineer"));
    assert!(deploys(&scope, "qa"));
    assert_eq!(scope.source, ContentSource::Bundled);
}

#[test]
fn undeclared_source_agents_still_deploy() {
    // The gate is an exclusion, not an allowlist: a source-directory agent the
    // manifest never names — a `BASE-*` inheritance fragment, or an agent a
    // catalog checkout adds — must still deploy. Silently deploying LESS than
    // the source carries is the failure class this work removes.
    let tmp = TempDir::new().unwrap();
    let scope = framework_agent_scope(tmp.path());
    assert!(deploys(&scope, "base-engineer"));
    assert!(deploys(&scope, "some-catalog-only-agent"));
}

// ---------------------------------------------------------------------------
// Loud failure — every variant, none reachable from "no marker detected"
// ---------------------------------------------------------------------------

#[test]
fn malformed_manifest_is_an_error() {
    let err = parse_framework_manifest("this is not toml {{{", &fake_catalog())
        .expect_err("malformed input must not resolve to a selection");
    assert!(matches!(err, FrameworkManifestError::Malformed(_)));
}

#[test]
fn missing_section_is_an_error() {
    // A well-formed manifest with no [agent_categories] must NOT fall through to
    // "deploy everything" — it is an error.
    let err = parse_framework_manifest("version = 1\n", &fake_catalog())
        .expect_err("a manifest with no categories declares nothing");
    assert_eq!(err, FrameworkManifestError::MissingCategories);
}

#[test]
fn empty_universal_is_an_error() {
    // The "never silently deploy nothing" half of the requirement.
    let raw = "[agent_categories]\nuniversal = []\n";
    let err = parse_framework_manifest(raw, &fake_catalog()).expect_err("empty universal");
    assert_eq!(err, FrameworkManifestError::EmptyUniversal);
}

#[test]
fn unknown_stem_is_an_error() {
    let raw = "[agent_categories]\nuniversal = [\"qa\", \"no-such-agent\"]\n";
    let err = parse_framework_manifest(raw, &fake_catalog()).expect_err("unknown stem");
    assert_eq!(
        err,
        FrameworkManifestError::UnknownAgent {
            category: "universal",
            stem: "no-such-agent".to_string(),
        }
    );
}

#[test]
fn undeclared_bundled_agent_is_an_error() {
    // `rust-engineer`, `react-engineer`, `vercel-ops` and `ops` are in the
    // catalog but not declared here.
    let raw = "[agent_categories]\nuniversal = [\"qa\"]\n";
    let err = parse_framework_manifest(raw, &fake_catalog()).expect_err("undeclared agents");
    match err {
        FrameworkManifestError::UndeclaredAgents(missing) => {
            assert_eq!(
                missing,
                vec![
                    "js-engineer".to_string(),
                    "ops".to_string(),
                    "react-engineer".to_string(),
                    "rust-engineer".to_string(),
                    "vercel-ops".to_string()
                ]
            );
        }
        other => panic!("expected UndeclaredAgents, got {other:?}"),
    }
}

#[test]
fn duplicate_declaration_is_an_error() {
    let raw = "[agent_categories]\nuniversal = [\"qa\"]\ndeprecated = [\"qa\"]\n";
    let err = parse_framework_manifest(raw, &fake_catalog()).expect_err("duplicate");
    assert_eq!(
        err,
        FrameworkManifestError::DuplicateDeclaration("qa".to_string())
    );
}

#[test]
fn gated_entry_without_markers_is_an_error() {
    // #4765: markers moved into the manifest, so "gated but with no condition"
    // is now representable — and undeployable. That must be loud, not silent.
    let raw = r#"
[agent_categories]
universal = ["ops"]

[[agent_categories.language]]
stem = "qa"
markers = []

[[agent_categories.language]]
stem = "rust-engineer"
markers = ["Cargo.toml"]

[[agent_categories.language]]
stem = "js-engineer"
markers = ["package.json"]

[[agent_categories.framework]]
stem = "react-engineer"
markers = ["package.json"]

[[agent_categories.platform]]
stem = "vercel-ops"
markers = ["vercel.json"]
"#;
    let err = parse_framework_manifest(raw, &fake_catalog()).expect_err("ungated entry");
    assert_eq!(
        err,
        FrameworkManifestError::UngatedEntry {
            category: "language",
            stem: "qa".to_string(),
        }
    );
}

// ---------------------------------------------------------------------------
// The skill roster (#4765)
// ---------------------------------------------------------------------------

#[test]
fn bundled_skill_roster_is_valid() {
    // The shipped `[skill_categories]` must exactly cover the bundled skills.
    let skills = framework_skill_categories().expect("bundled skill roster must validate");
    assert_eq!(
        skills.universal.len(),
        bundled_skill_stems().len(),
        "every bundled skill is declared exactly once"
    );
    assert!(skills.universal.contains(&"tm".to_string()));
}

#[test]
fn bundled_skill_stems_excludes_reference_files() {
    let stems = bundled_skill_stems();
    assert!(stems.contains("systematic-debugging"));
    assert!(
        !stems.iter().any(|s| s.contains('/')),
        "a skill's own references/*.md are not catalog entries"
    );
}

#[test]
fn missing_skill_section_is_an_error() {
    let err = parse_framework_skills("version = 1\n", &fake_skill_catalog())
        .expect_err("a manifest with no skill section declares nothing");
    assert_eq!(err, FrameworkManifestError::MissingSkillCategories);
}

#[test]
fn unknown_skill_is_an_error() {
    let raw = "[skill_categories]\nuniversal = [\"tm\", \"no-such-skill\"]\n";
    let err = parse_framework_skills(raw, &fake_skill_catalog()).expect_err("unknown skill");
    assert_eq!(
        err,
        FrameworkManifestError::UnknownSkill("no-such-skill".to_string())
    );
}

#[test]
fn undeclared_bundled_skill_is_an_error() {
    let raw = "[skill_categories]\nuniversal = []\n";
    let err = parse_framework_skills(raw, &fake_skill_catalog()).expect_err("undeclared skill");
    assert_eq!(
        err,
        FrameworkManifestError::UndeclaredSkills(vec!["tm".to_string()])
    );
}

#[test]
fn duplicate_skill_is_an_error() {
    let raw = "[skill_categories]\nuniversal = [\"tm\", \"tm\"]\n";
    let err = parse_framework_skills(raw, &fake_skill_catalog()).expect_err("duplicate skill");
    assert_eq!(
        err,
        FrameworkManifestError::DuplicateSkill("tm".to_string())
    );
}

// ---------------------------------------------------------------------------
// Composition — against SYNTHETIC categories
// ---------------------------------------------------------------------------

#[test]
fn universal_agents_deploy_with_no_markers() {
    let tmp = TempDir::new().unwrap();
    touch(tmp.path(), "README.md");
    let scope = agent_scope_from(&parsed_fake(), tmp.path());
    assert!(
        deploys(&scope, "qa"),
        "universal deploys with zero detection"
    );
}

#[test]
fn language_engineers_still_gate_on_detection() {
    // A Cargo project keeps rust-engineer; a project whose detected stack is a
    // DIFFERENT declared language does not. The negative case must detect
    // something — a project matching no fixture marker at all takes the
    // unknown-project fallback and keeps everything, which is
    // `unknown_project_keeps_every_stack_engineer`'s subject, not this one.
    let rust = TempDir::new().unwrap();
    touch(rust.path(), "Cargo.toml");
    let scope = agent_scope_from(&parsed_fake(), rust.path());
    assert!(deploys(&scope, "rust-engineer"));
    assert!(
        deploys(&scope, "qa"),
        "universal is unaffected by stack gating"
    );

    let js = TempDir::new().unwrap();
    touch(js.path(), "package.json");
    let scope = agent_scope_from(&parsed_fake(), js.path());
    assert!(
        deploys(&scope, "js-engineer"),
        "the JS project detects its own engineer"
    );
    assert!(
        !deploys(&scope, "rust-engineer"),
        "a JS project must not get the Rust engineer"
    );
    assert!(
        deploys(&scope, "qa"),
        "...and the exclusion must be stack-specific, not a blanket drop"
    );
}

#[test]
fn framework_engineers_gate_on_detection() {
    // #4760: `react-engineer` requires a DECLARED react dependency, not merely
    // a `package.json`. A framework-free JS project and a Rust project both
    // miss it, while the language engineers in the same call still fire.
    let react = TempDir::new().unwrap();
    std::fs::write(
        react.path().join("package.json"),
        "{\"dependencies\": {\"react\": \"^18.0.0\"}}",
    )
    .unwrap();
    assert!(deploys(
        &agent_scope_from(&parsed_fake(), react.path()),
        "react-engineer"
    ));

    let bare_js = TempDir::new().unwrap();
    std::fs::write(
        bare_js.path().join("package.json"),
        "{\"dependencies\": {\"lodash\": \"^4.0.0\"}}",
    )
    .unwrap();
    assert!(
        !deploys(
            &agent_scope_from(&parsed_fake(), bare_js.path()),
            "react-engineer"
        ),
        "a framework-free JS project must not receive react-engineer"
    );

    let rust = TempDir::new().unwrap();
    touch(rust.path(), "Cargo.toml");
    let scope = agent_scope_from(&parsed_fake(), rust.path());
    assert!(!deploys(&scope, "react-engineer"));
    assert!(
        deploys(&scope, "rust-engineer"),
        "the same call still selects the detected stack engineer"
    );
}

#[test]
fn deprecated_agent_never_deploys() {
    // `ops` IS in the synthetic catalog and IS declared — only its category
    // keeps it out. Every other fixture agent is checked in the same call so a
    // blanket-drop selection cannot make this pass.
    for markers in [
        vec!["README.md"],
        vec!["Cargo.toml"],
        vec!["package.json", "vercel.json"],
    ] {
        let tmp = TempDir::new().unwrap();
        for m in &markers {
            touch(tmp.path(), m);
        }
        let scope = agent_scope_from(&parsed_fake(), tmp.path());
        assert!(
            !deploys(&scope, "ops"),
            "deprecated `ops` must not deploy for markers {markers:?}"
        );
        assert!(
            deploys(&scope, "qa"),
            "...while the universal agent still does, for markers {markers:?}"
        );
    }
}

#[test]
fn platform_agent_absent_without_marker() {
    // "No platform detected" is an ORDINARY result: a selection that simply
    // drops the platform agents, distinguishable from the Err a broken manifest
    // produces (see `malformed_manifest_is_an_error`).
    let tmp = TempDir::new().unwrap();
    touch(tmp.path(), "Cargo.toml");
    let scope = agent_scope_from(&parsed_fake(), tmp.path());
    assert!(
        !deploys(&scope, "vercel-ops"),
        "no Vercel marker -> no vercel-ops"
    );
    assert!(
        deploys(&scope, "rust-engineer") && deploys(&scope, "qa"),
        "the same call still deploys the stack and universal agents — the empty \
         platform result is explicit, not a collapsed selection"
    );
}

#[test]
fn platform_agent_deploys_with_marker() {
    let tmp = TempDir::new().unwrap();
    touch(tmp.path(), "Cargo.toml");
    touch(tmp.path(), "vercel.json");
    assert!(deploys(
        &agent_scope_from(&parsed_fake(), tmp.path()),
        "vercel-ops"
    ));
}

#[test]
fn no_platform_detected_is_not_a_manifest_error() {
    // The distinguishability requirement, stated as one assertion set: the same
    // manifest that yields Ok for a platform-less project yields Err when the
    // document itself is broken.
    let tmp = TempDir::new().unwrap();
    touch(tmp.path(), "Cargo.toml");
    assert!(parse_framework_manifest(&fake_manifest(), &fake_catalog()).is_ok());
    assert!(!deploys(
        &agent_scope_from(&parsed_fake(), tmp.path()),
        "vercel-ops"
    ));
    assert!(parse_framework_manifest("broken {{{", &fake_catalog()).is_err());
}

#[test]
fn unknown_project_keeps_every_stack_engineer() {
    // Zero-regression fallback: a project with NO recognised stack marker keeps
    // every language and framework engineer, exactly as `language_agent_scope`
    // behaved before #4760. Platform agents get no such fallback.
    let tmp = TempDir::new().unwrap();
    touch(tmp.path(), "README.md");
    let scope = agent_scope_from(&parsed_fake(), tmp.path());
    assert!(deploys(&scope, "rust-engineer"));
    assert!(deploys(&scope, "react-engineer"));
    assert!(
        !deploys(&scope, "vercel-ops"),
        "platform has no unknown-project fallback"
    );
    assert!(!deploys(&scope, "ops"));
}
