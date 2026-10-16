//! #9132: the Architect seed files read from content, failing closed.
//!
//! Why: each error arm must name the file and stop every write; a loader that
//! fell back to an empty or partial seed would pass a happy-path test.
//! What: loads the checkout's content, a legacy source with no seed file, and
//! scratch sources missing or blanking one file.
//! Test: this is the test module.

use sha2::{Digest, Sha256};
use trusty_agents_common::agent_content::checkout_content;

use super::*;
use crate::core::content_source::test_support::{rc, repo_content};
use crate::core::framework_content::tests::{fake_checkout_with_package, legacy_content};
use crate::core::project_skill_tier::architect_skills::{
    self, CONTEXT_REFRESH, FLEET_CHECK, deploy_resolved,
};

/// sha256 of the `CLAUDE.md` template `include_str!` compiled in at eada92a230.
const EMBEDDED_CLAUDE_MD: &str = "620df7a8a94a6db7e466917ea542002b597a46a32be8bb5f473949e07e8d2df1";

fn sha256_hex(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

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

/// Acceptance 4: the default content seeds what the binary compiled in. The
/// template is byte-identical; each skill differs only in its `version:`
/// line, which ADR-0064 content declares as `metadata:`, and the compiled-in
/// hash is in its history, so an untouched seed refreshes, never reads custom.
#[test]
fn the_repository_content_seeds_the_architect() {
    let seed = ArchitectSeed::load(rc()).expect("the checkout carries the seed");
    assert_eq!(sha256_hex(&seed.claude_md), EMBEDDED_CLAUDE_MD);
    let stems: Vec<&str> = seed.skills.iter().map(|s| s.stem).collect();
    assert_eq!(stems, [FLEET_CHECK.stem, CONTEXT_REFRESH.stem]);
    for skill in &seed.skills {
        let embedded = skill.contents.replacen(
            "metadata:\n  version: \"0.1.0\"\n",
            "version: \"0.1.0\"\n",
            1,
        );
        assert_ne!(
            embedded, skill.contents,
            "{}: no metadata block",
            skill.stem
        );
        let embedded_hash = sha256_hex(&embedded);
        assert!(
            skill.previous.contains(&embedded_hash.as_str()),
            "{}: the compiled-in text {embedded_hash} is not in its history",
            skill.stem
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
        (FLEET_CHECK.source, &shipped(FLEET_CHECK.source)),
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
        (FLEET_CHECK.source, " \n\n"),
        (CONTEXT_REFRESH.source, &shipped(CONTEXT_REFRESH.source)),
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
