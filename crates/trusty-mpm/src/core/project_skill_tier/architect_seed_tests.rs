//! #9132: the Architect seed files read from content, failing closed.
//!
//! Why: each error arm must name the file and stop every write; a loader that
//! fell back to an empty or partial seed would pass a happy-path test.
//! What: loads the checkout's content, a legacy source with no seed file, and
//! scratch sources missing or blanking one file.
//! Test: this is the test module.

use trusty_agents_common::agent_content::checkout_content;

use super::*;
use crate::core::content_source::test_support::{rc, repo_content};
use crate::core::framework_content::tests::{fake_checkout_with_package, legacy_content};
use crate::core::project_skill_tier::architect_agents::SHIPPED_AGENTS;
use crate::core::project_skill_tier::architect_skills::{
    self, CONTEXT_REFRESH, FLEET_CHECK, SHIPPED, ShippedSkill, deploy_resolved, deploy_skills_into,
};

/// sha256 of each skill as `include_str!` compiled it in at eada92a230, before
/// #9132 turned `version:` into a `metadata:` block.
const COMPILED_IN: [(&str, &str); 2] = [
    (
        "tm-fleet-check",
        "58a841696ac206c4bb456de7be55ac3eb1f1ff0caa6ae21d366526b22bb72a8a",
    ),
    (
        "tm-context-refresh",
        "2267c3436de2a177509f99a9445496e0243f0a3a55e32cd51d5264608175bf7f",
    ),
];

/// A content source with the repository's required instructions plus
/// `extra` (`(path relative to instructions/, body)`).
fn content_with(extra: &[(&str, &str)]) -> FrameworkContent {
    let dir = tempfile::tempdir().expect("tempdir");
    let package = repo_content()
        .required("pm-instruction-package.json")
        .to_owned();
    fake_checkout_with_package(dir.path(), &package);
    for (rel, body) in extra {
        let path = dir.path().join("content/instructions").join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(path, body).expect("seed file");
    }
    FrameworkContent::load(&checkout_content(dir.path()).expect("checkout")).expect("loads")
}

/// The checkout's seed text for `rel`.
fn shipped(rel: &str) -> String {
    rc().instruction(rel).expect("shipped").to_owned()
}

/// Acceptance 4 (#9132), and #9997: the default content seeds every
/// [`SHIPPED`] skill, each with a `metadata:` block, and each skill's
/// compiled-in text is still in its history, so a seed of it refreshes, never
/// reads custom. The template carries the `local/` layout.
#[test]
fn the_repository_content_seeds_the_architect() {
    let seed = ArchitectSeed::load(rc()).expect("the checkout carries the seed");
    assert!(
        seed.claude_md.contains("`local/state.md`"),
        "{}",
        seed.claude_md
    );
    assert!(!seed.claude_md.contains("records/"), "{}", seed.claude_md);
    let stems: Vec<&str> = seed.skills.iter().map(|s| s.stem).collect();
    let shipped_stems: Vec<&str> = SHIPPED.iter().map(|s| s.stem).collect();
    assert_eq!(stems, shipped_stems);
    for (stem, compiled_in) in COMPILED_IN {
        let skill = seed
            .skills
            .iter()
            .find(|s| s.stem == stem)
            .unwrap_or_else(|| panic!("{stem} is not seeded"));
        assert!(
            skill.contents.contains("metadata:\n  version: "),
            "{stem}: no metadata block"
        );
        assert!(
            skill.previous.contains(&compiled_in),
            "{stem}: the compiled-in text {compiled_in} is not in its history"
        );
    }
}

/// A release older than #9132 carries no seed file: the first one, the
/// template, is named, with the remedy.
#[test]
fn a_source_without_the_seed_files_names_the_first_missing_one() {
    let err = ArchitectSeed::load(&legacy_content()).expect_err("no seed file");
    match &err {
        ArchitectSeedError::Missing { path, .. } => {
            assert_eq!(path, "instructions/architect/templates/CLAUDE.md");
        }
        other => panic!("expected Missing, got {other:?}"),
    }
    assert!(err.to_string().contains("tm content update"), "{err}");
}

/// All or nothing: one absent skill fails the whole load.
#[test]
fn a_source_missing_one_skill_loads_nothing() {
    let content = content_with(&[
        (CLAUDE_MD_TEMPLATE, &shipped(CLAUDE_MD_TEMPLATE)),
        (&FLEET_CHECK.source(), &shipped(&FLEET_CHECK.source())),
    ]);
    let err = ArchitectSeed::load(&content).expect_err("a skill is missing");
    assert!(
        matches!(&err, ArchitectSeedError::Missing { path, .. }
            if path == "instructions/architect/skills/tm-context-refresh.md"),
        "{err:?}"
    );
}

/// A blank seed file is refused by name, never seeded blank.
#[test]
fn a_blank_seed_file_is_refused() {
    let content = content_with(&[
        (CLAUDE_MD_TEMPLATE, &shipped(CLAUDE_MD_TEMPLATE)),
        (&FLEET_CHECK.source(), " \n\n"),
        (
            &CONTEXT_REFRESH.source(),
            &shipped(&CONTEXT_REFRESH.source()),
        ),
    ]);
    let err = ArchitectSeed::load(&content).expect_err("a blank skill");
    assert!(
        matches!(&err, ArchitectSeedError::Empty { path, .. }
            if path == "instructions/architect/skills/tm-fleet-check.md"),
        "{err:?}"
    );
}

/// No checkout and nothing installed: a content error, never a seed.
#[test]
fn an_unresolvable_source_is_a_content_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = tempfile::tempdir().expect("cache");
    let err = ArchitectSeed::resolve_in(dir.path(), None, Some(cache.path()), Fetch::Never)
        .expect_err("nothing serves content");
    assert!(
        matches!(&err, ArchitectSeedError::Content(inner) if inner.is_not_installed()),
        "{err:?}"
    );
}

/// The launch path: skills that do not resolve write no skill, and the one
/// error line names the missing file.
#[test]
fn an_unreadable_seed_writes_no_skill_and_names_the_file() {
    let project = tempfile::tempdir().expect("project");
    let report = deploy_resolved(project.path(), &[], load_skills(&legacy_content()));
    assert!(report.written.is_empty(), "{:?}", report.written);
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert!(
        report.errors[0].contains("instructions/architect/skills/tm-fleet-check.md"),
        "{:?}",
        report.errors
    );
    assert!(
        !architect_skills::project_tier(project.path()).exists(),
        "a skill tier was written"
    );
}

/// #9997 AC5: one more [`ShippedSkill`] entry plus its content file is the
/// whole change: the loader, the seed and the deployer pick it up.
#[test]
fn one_more_shipped_entry_and_content_file_load_seed_and_deploy() {
    const EXTRA: ShippedSkill = ShippedSkill {
        stem: "tm-probe-extra",
        previous: &[],
    };
    let body = "---\nname: tm-probe-extra\n---\n\n# Probe\n";
    let mut files: Vec<(String, String)> = SHIPPED
        .iter()
        .map(|s| (s.source(), shipped(&s.source())))
        .collect();
    files.push((CLAUDE_MD_TEMPLATE.to_owned(), shipped(CLAUDE_MD_TEMPLATE)));
    // #10161 D3c: the seed carries the agents too.
    files.extend(
        SHIPPED_AGENTS
            .iter()
            .map(|a| (a.source(), shipped(&a.source()))),
    );
    files.push((EXTRA.source(), body.to_owned()));
    let extra: Vec<(&str, &str)> = files
        .iter()
        .map(|(rel, text)| (rel.as_str(), text.as_str()))
        .collect();
    let content = content_with(&extra);
    let table: Vec<ShippedSkill> = SHIPPED.iter().copied().chain([EXTRA]).collect();

    let seed = ArchitectSeed::load_from(&content, &table).expect("the extra skill loads");
    let stems: Vec<&str> = seed.skills.iter().map(|s| s.stem).collect();
    let want: Vec<&str> = table.iter().map(|s| s.stem).collect();
    assert_eq!(stems, want);
    assert_eq!(seed.skills.last().map(|s| s.contents.as_str()), Some(body));

    let tier = tempfile::tempdir().expect("tier");
    let report = deploy_skills_into(tier.path(), &seed.skills);
    assert_eq!(report.written, want, "{report:?}");
    assert_eq!(
        std::fs::read_to_string(architect_skills::skill_path(tier.path(), EXTRA.stem))
            .expect("the extra skill was written"),
        body
    );
}

/// #10161 D3c: every agent the seed needs is required. An absent agent and a
/// blank one each fail the whole load by name, so `tm fleet init` writes
/// nothing; the checkout's content seeds all five.
#[test]
fn a_missing_or_blank_agent_source_loads_nothing() {
    let seed = ArchitectSeed::load(rc()).expect("the checkout carries the seed");
    let names: Vec<&str> = seed.agents.iter().map(|a| a.stem).collect();
    let want: Vec<&str> = SHIPPED_AGENTS.iter().map(|a| a.name).collect();
    assert_eq!(names, want);

    let mut files: Vec<(String, String)> = SHIPPED
        .iter()
        .map(|s| (s.source(), shipped(&s.source())))
        .collect();
    files.push((CLAUDE_MD_TEMPLATE.to_owned(), shipped(CLAUDE_MD_TEMPLATE)));
    let last = SHIPPED_AGENTS.last().expect("an agent ships").source();
    for agent in SHIPPED_AGENTS {
        if agent.source() != last {
            files.push((agent.source(), shipped(&agent.source())));
        }
    }
    let view = |files: &[(String, String)]| -> FrameworkContent {
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(r, b)| (r.as_str(), b.as_str()))
            .collect();
        content_with(&refs)
    };
    let err = ArchitectSeed::load(&view(&files)).expect_err("an agent is missing");
    assert!(
        matches!(&err, ArchitectSeedError::Missing { path, .. }
            if *path == format!("instructions/{last}")),
        "{err:?}"
    );
    files.push((last.clone(), " \n\n".to_owned()));
    let err = ArchitectSeed::load(&view(&files)).expect_err("an agent is blank");
    assert!(
        matches!(&err, ArchitectSeedError::Empty { path, .. }
            if *path == format!("instructions/{last}")),
        "{err:?}"
    );
}

/// The shipped manual, relative to `instructions/`.
const MANUAL: &str = "architect/architect.md";

/// #9997: one context bar. No shipped manual, skill or template states a
/// percent threshold or a retired name, and the texts that set the bar name
/// it in tokens.
#[test]
fn the_shipped_texts_name_the_250k_bar_and_no_percent_threshold() {
    let mut texts: Vec<(String, String)> = SHIPPED
        .iter()
        .map(|s| (s.source(), shipped(&s.source())))
        .collect();
    for rel in [CLAUDE_MD_TEMPLATE, MANUAL] {
        texts.push((rel.to_owned(), shipped(rel)));
    }
    assert!(texts.len() >= 4, "{} texts", texts.len());
    for (rel, text) in &texts {
        let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
        for banned in [
            "50%",
            "45 to 49%",
            "records/",
            "trusty-fleet-",
            "trusty-context-",
        ] {
            assert!(!flat.contains(banned), "{rel} still names `{banned}`");
        }
    }
    for rel in [
        MANUAL.to_owned(),
        CLAUDE_MD_TEMPLATE.to_owned(),
        FLEET_CHECK.source(),
        CONTEXT_REFRESH.source(),
    ] {
        let flat = shipped(&rel)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            flat.contains("250k tokens"),
            "{rel} does not name the 250k bar"
        );
    }
}
