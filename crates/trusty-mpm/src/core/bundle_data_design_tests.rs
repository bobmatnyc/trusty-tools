//! Registration and import-safety tests for the `data-design` skill and the
//! `data-designer` agent (#10401).
//!
//! Why: the skill vendors third-party data (ColorBrewer) and ships a script,
//! and the agent's no-network rule is structural (no web tools). Both must
//! stay true as the content changes, and both must stay registered so
//! `tm install` deploys them.
//! What: frontmatter parse of skill and agent, manifest and roster
//! registration, the SKILL entry line cap, and an import-safety scan of every
//! file under `skills/data-design/`.
//! Test: this file; run with `cargo test -p trusty-mpm bundle::tests::data_design`.
use crate::core::content_source::test_support::{rc, repo_roster};

const SKILL_ENTRY: &str = "skills/data-design.md";
const SKILL_DIR: &str = "skills/data-design/";
const AGENT_FILE: &str = "data-designer.md";
/// The entry file stays a routing table plus hard rules (#10401).
const SKILL_LINE_CAP: usize = 120;
/// Verbatim upstream licence text: exempt from the URL scan, never from the
/// fetch and hook scans.
const LICENCE_FILE: &str = "skills/data-design/references/LICENSE-colorbrewer.txt";

const REFERENCES: [&str; 9] = [
    "chart-selection.md",
    "colour.md",
    "encoding-and-labels.md",
    "hierarchy-and-part-to-whole.md",
    "time-flow-network-geo.md",
    "empty-and-missing-data.md",
    "accessibility.md",
    "palettes-colorbrewer.md",
    "PROVENANCE.md",
];

/// The YAML between the leading `---` fences of `doc`.
fn frontmatter(doc: &str) -> serde_yaml::Value {
    let rest = doc
        .strip_prefix("---\n")
        .expect("frontmatter opens the file");
    let end = rest.find("\n---").expect("frontmatter closes");
    serde_yaml::from_str(&rest[..end]).expect("frontmatter is valid YAML")
}

/// Every file of the skill as `(bundle path, body)`, entry file included.
fn skill_files() -> Vec<(&'static str, &'static str)> {
    rc().skills()
        .filter(|(path, _)| *path == SKILL_ENTRY || path.starts_with(SKILL_DIR))
        .collect()
}

#[test]
fn data_design_skill_frontmatter_parses() {
    let body = rc()
        .skill(SKILL_ENTRY)
        .expect("data-design skill in content");
    let fm = frontmatter(body);
    assert_eq!(fm["name"].as_str(), Some("data-design"));
    assert_eq!(fm["user-invocable"].as_bool(), Some(false));
    assert_eq!(fm["category"].as_str(), Some("agent-reference"));
    assert_eq!(fm["metadata"]["version"].as_str(), Some("0.1.0"));
    assert!(
        fm["description"]
            .as_str()
            .is_some_and(|d| d.contains("chart"))
    );
    assert!(fm.get("allowed-tools").is_none(), "no tool widening");
    assert!(fm.get("hooks").is_none(), "no hooks");
}

#[test]
fn data_design_skill_entry_is_within_line_cap() {
    let lines = rc().skill(SKILL_ENTRY).expect("skill").lines().count();
    assert!(
        lines <= SKILL_LINE_CAP,
        "{SKILL_ENTRY} is {lines} lines (cap {SKILL_LINE_CAP}); move detail to references/"
    );
}

#[test]
fn data_design_skill_ships_every_reference_and_the_checker() {
    let files: Vec<&str> = skill_files().into_iter().map(|(p, _)| p).collect();
    let entry = rc().skill(SKILL_ENTRY).expect("skill");
    for name in REFERENCES {
        let path = format!("{SKILL_DIR}references/{name}");
        assert!(files.contains(&path.as_str()), "missing {path}");
        assert!(
            entry.contains(&format!("references/{name}")),
            "the entry file must route to references/{name}"
        );
    }
    assert!(files.contains(&"skills/data-design/scripts/check_palette.py"));
    assert!(files.contains(&LICENCE_FILE));
}

#[test]
fn data_designer_agent_frontmatter_and_standing_rule() {
    let roster = repo_roster();
    let body = roster.require(AGENT_FILE).unwrap_or_else(|e| panic!("{e}"));
    let meta = crate::core::agent_metadata::agent_metadata_from_str(body);
    assert_eq!(meta.name.as_deref(), Some("data-designer"));
    assert_eq!(meta.extends.as_deref(), Some("base-agent"));
    assert_eq!(meta.skills, vec!["data-design".to_string()]);
    let tools = meta.tools.expect("tools declared");
    for banned in ["WebFetch", "WebSearch"] {
        assert!(
            !tools.iter().any(|t| t == banned),
            "{banned} must not be granted"
        );
    }
    let fm = frontmatter(body);
    assert!(fm.get("allowed-tools").is_none() && fm.get("hooks").is_none());
    assert!(
        body.contains("never leave this machine"),
        "standing data rule"
    );
    assert!(body.contains("Datawrapper") && body.contains("Flourish"));
    assert!(
        body.contains("Design Spec"),
        "the agent's output is a design spec"
    );
}

#[test]
fn data_design_entries_are_registered_universal() {
    use crate::core::manifest::framework::{
        framework_agent_categories_for, framework_skill_categories_for,
    };
    let agents = framework_agent_categories_for(&repo_roster()).expect("agent partition");
    assert!(agents.universal.iter().any(|s| s == "data-designer"));
    let skills = framework_skill_categories_for(rc()).expect("skill partition");
    assert!(skills.universal.iter().any(|s| s == "data-design"));
}

#[test]
fn data_designer_composes_against_the_real_base() {
    let dir = crate::core::content_source::test_support::repo_agents_dir();
    let composed = crate::core::agent_builder::compose_agent("data-designer", &dir)
        .unwrap_or_else(|e| panic!("compose_agent(data-designer) failed: {e}"));
    assert!(
        composed.starts_with("---\n"),
        "composed agent keeps frontmatter"
    );
    assert!(!composed.contains("\nextends:"), "builder strips extends");
}

/// Why: the brief's import-safety checklist (#10401): no fetch instruction,
/// no executable surface beyond our own checker, and a URL only where it is a
/// citation.
/// What: scans every skill file. URLs may appear only inside a `## Sources`
/// section or an HTML provenance comment; the licence file is exempt from the
/// URL rule only. The checker may import no network module.
#[test]
fn data_design_import_safety_scan() {
    const FORBIDDEN: [&str; 9] = [
        "curl ",
        "wget ",
        "WebFetch",
        "WebSearch",
        "npx ",
        "pip install",
        "allowed-tools:",
        "hooks:",
        "!`",
    ];
    for (path, body) in skill_files() {
        for needle in FORBIDDEN {
            assert!(!body.contains(needle), "{path} contains `{needle}`");
        }
        if path == LICENCE_FILE {
            continue;
        }
        let (mut in_sources, mut in_comment) = (false, false);
        for (n, line) in body.lines().enumerate() {
            if line.starts_with("## ") {
                in_sources = line.trim() == "## Sources";
            }
            if line.contains("<!--") {
                in_comment = true;
            }
            let has_url = line.contains("http://") || line.contains("https://");
            assert!(
                !has_url || in_sources || in_comment,
                "{path}:{} has a URL outside a Sources block: {line}",
                n + 1
            );
            if line.contains("-->") {
                in_comment = false;
            }
        }
        if path.ends_with(".py") {
            for module in ["urllib", "socket", "http.client", "requests", "subprocess"] {
                assert!(
                    !body.contains(&format!("import {module}")),
                    "{path} imports {module}"
                );
            }
        }
    }
}
