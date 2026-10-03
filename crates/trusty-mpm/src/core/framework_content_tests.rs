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

/// The repository's PM instruction package with one section id this binary
/// does not know, as a newer content release could ship it.
fn package_with_an_unknown_section() -> String {
    let mut package: serde_json::Value =
        serde_json::from_str(repo_content().required("pm-instruction-package.json"))
            .expect("the repository package is JSON");
    let sections = package["sections"].as_array_mut().expect("sections array");
    let mut newer = sections[0].clone();
    newer["id"] = "a-section-from-a-newer-release".into();
    sections.push(newer);
    package.to_string()
}

/// A trusted checkout at `root` carrying the repository's required
/// instructions and one skill, with `package` as the PM instruction package.
fn fake_checkout_with_package(root: &Path, package: &str) {
    let content = repo_content();
    let mut files: Vec<(String, String)> = REQUIRED_INSTRUCTIONS
        .iter()
        .map(|rel| {
            let body = if *rel == "pm-instruction-package.json" {
                package.to_string()
            } else {
                content.required(rel).to_string()
            };
            (format!("content/instructions/{rel}"), body)
        })
        .collect();
    files.push(("content/skills/tm.md".to_string(), "skill".to_string()));
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, b)| (p.as_str(), b.as_str()))
        .collect();
    fake_checkout(root, &refs);
}

/// #9012: a PM package this binary cannot parse refuses the load, naming the
/// file, the defect and the remedy — never a prompt missing its rules.
#[test]
fn a_source_whose_package_does_not_parse_is_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    fake_checkout_with_package(dir.path(), &package_with_an_unknown_section());
    let err = FrameworkContent::load(&checkout_content(dir.path()).expect("checkout"))
        .expect_err("a package with an unknown section id must not load");
    let shown = err.to_string();
    for needle in [
        "instructions/pm-instruction-package.json",
        "a-section-from-a-newer-release",
        "tm content update",
    ] {
        assert!(shown.contains(needle), "{needle} missing: {shown}");
    }
}

/// #9012: the launch-time refresh (fresh start, resume, in-place relaunch) of
/// a project whose content carries an unparseable PM package is refused, and
/// no compiled prompt is written.
#[test]
fn a_launch_from_a_checkout_whose_package_does_not_parse_is_refused() {
    let checkout = tempfile::tempdir().expect("tempdir");
    fake_checkout_with_package(checkout.path(), &package_with_an_unknown_section());
    let project = checkout.path().join("project");
    std::fs::create_dir_all(&project).expect("project dir");
    let root = tempfile::tempdir().expect("framework root");
    let dest = crate::core::instruction_pipeline::compiled_prompt_path(&project, "sess-1");

    let msg = crate::core::instruction_pipeline::refresh_compiled_prompt_in(
        root.path(),
        &project,
        "sess-1",
    )
    .expect_err("a package this binary cannot parse must refuse the launch");
    assert!(msg.contains("pm-instruction-package.json"), "{msg}");
    assert!(msg.contains("tm content update"), "{msg}");
    assert!(!dest.exists(), "a refused launch writes no compiled prompt");
}
