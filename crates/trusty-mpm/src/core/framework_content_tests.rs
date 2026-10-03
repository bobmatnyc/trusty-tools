//! Tests for [`super::FrameworkContent`] (#9012).

use std::path::Path;

use trusty_agents_common::agent_content::checkout_content;

use super::*;
use crate::core::content_source::test_support::{repo_content, repo_root};

/// Builds a trusted checkout at `root` from `files` (`(repo-relative path,
/// body)`), with every dev class directory present.
fn fake_checkout(root: &Path, files: &[(&str, &str)]) {
    for (_, rel) in trusty_common::content::DEV_CLASS_SOURCES {
        std::fs::create_dir_all(root.join(rel)).expect("class dir");
    }
    std::fs::create_dir_all(root.join(".git")).expect(".git");
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").expect("Cargo.toml");
    for (rel, body) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(path, body).expect("file");
    }
}

/// Every required instruction, as checkout files.
fn required_files() -> Vec<(String, &'static str)> {
    REQUIRED_INSTRUCTIONS
        .iter()
        .map(|rel| (format!("content/instructions/{rel}"), "body"))
        .collect()
}

/// #9012: the repository's content carries every skill file and every
/// required instruction, read from `content/`.
#[test]
fn the_repository_content_loads() {
    let content = repo_content();
    let on_disk = walk(&repo_root().join("content/skills"));
    assert_eq!(
        content.skills().count(),
        on_disk,
        "one entry per skill file"
    );
    assert!(content.skill("skills/tm-workflow.md").is_some());
    assert!(
        content
            .skill("skills/tm-epic/references/anti-patterns.md")
            .is_some()
    );
    for rel in REQUIRED_INSTRUCTIONS {
        assert!(!content.required(rel).trim().is_empty(), "{rel} is empty");
    }
    assert!(content.instruction("docs/WHAT-IS-TRUSTY-MPM.md").is_some());
    assert!(matches!(
        content.source(),
        ContentSource::DevCheckout { .. }
    ));
}

/// Counts regular files under `dir`, recursively.
fn walk(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .expect("dir")
        .flatten()
        .map(|e| {
            if e.path().is_dir() {
                walk(&e.path())
            } else {
                1
            }
        })
        .sum()
}

/// #9012: a source with no skill is an error, never an empty catalog.
#[test]
fn a_source_without_skills_is_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let files = required_files();
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, b)| (p.as_str(), *b)).collect();
    fake_checkout(dir.path(), &refs);
    let err = FrameworkContent::load(&checkout_content(dir.path()).expect("checkout"))
        .expect_err("no skills");
    assert!(
        matches!(&err, AgentContentError::Missing { path, .. } if path == "skills/"),
        "got {err:?}"
    );
    assert!(err.to_string().contains("tm content update"), "{err}");
}

/// #9012: a source lacking one required instruction names that file.
#[test]
fn a_source_missing_a_required_instruction_is_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut files = required_files();
    files.retain(|(p, _)| !p.ends_with("sections/core.md"));
    files.push(("content/skills/tm.md".to_string(), "skill"));
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, b)| (p.as_str(), *b)).collect();
    fake_checkout(dir.path(), &refs);
    let err = FrameworkContent::load(&checkout_content(dir.path()).expect("checkout"))
        .expect_err("missing core");
    assert!(
        matches!(&err, AgentContentError::Missing { path, .. }
            if path == "instructions/sections/core.md"),
        "got {err:?}"
    );
}

/// `require_instruction` names an absent optional file.
#[test]
fn require_instruction_names_an_absent_file() {
    let err = repo_content()
        .require_instruction("docs/NOPE.md")
        .expect_err("absent");
    assert!(
        err.to_string().contains("instructions/docs/NOPE.md"),
        "{err}"
    );
}
