//! Tests for the `content_requires` doctor row (#8390).

use std::path::Path;

use trusty_agents_common::agent_content::{AgentRoster, checkout_content};

use super::*;
use crate::core::content_source::test_support::repo_root;
use crate::core::framework_content::FrameworkContent;

/// A requirement no released `tm` satisfies.
const UNMET: &str = "requires: {tm: \">=999.0.0\"}";

/// Copies `src` into `dst`, recursively.
fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("dir");
    for entry in std::fs::read_dir(src).expect("read_dir") {
        let entry = entry.expect("entry");
        let to = dst.join(entry.file_name());
        if entry.file_type().expect("type").is_dir() {
            copy_tree(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).expect("copy");
        }
    }
}

/// A trusted checkout at `root` holding this repository's `content/`.
fn checkout_with_repo_content(root: &Path) {
    copy_tree(&repo_root().join("content"), &root.join("content"));
    std::fs::create_dir_all(root.join(".git")).expect(".git");
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").expect("Cargo.toml");
}

/// A trusted checkout at `root` holding only `files` (`content/`-relative).
fn checkout_with(root: &Path, files: &[(&str, &str)]) {
    for (_, rel) in trusty_common::content::DEV_CLASS_SOURCES {
        std::fs::create_dir_all(root.join(rel)).expect("class dir");
    }
    std::fs::create_dir_all(root.join(".git")).expect(".git");
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").expect("Cargo.toml");
    for (rel, body) in files {
        let path = root.join("content").join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(path, body).expect("file");
    }
}

/// Inserts `line` as the first frontmatter line of `content/<rel>`.
fn add_frontmatter_line(root: &Path, rel: &str, line: &str) {
    let path = root.join("content").join(rel);
    let text = std::fs::read_to_string(&path).expect("asset");
    let rest = text
        .strip_prefix("---\n")
        .expect("asset opens with frontmatter");
    std::fs::write(&path, format!("---\n{line}\n{rest}")).expect("write");
}

fn v(text: &str) -> semver::Version {
    semver::Version::parse(text).expect("version")
}

/// #8390 acceptance 1: an asset whose `requires:` this binary does not satisfy
/// still deploys and runs, and the row reports it as advisory, never FAIL.
#[test]
fn an_unmet_requires_still_deploys_and_runs_and_doctor_only_warns() {
    let tmp = tempfile::tempdir().expect("tmp");
    let root = tmp.path().join("checkout");
    checkout_with_repo_content(&root);
    add_frontmatter_line(&root, "agents/engineer.md", UNMET);
    add_frontmatter_line(&root, "skills/tm-doctor.md", UNMET);
    let content = checkout_content(&root).expect("content");

    // Runs: the launch path's loader accepts the tree.
    let framework = FrameworkContent::load(&content).expect("content still loads");
    let skill = framework
        .skill("skills/tm-doctor.md")
        .expect("skill served");
    assert!(skill.contains(UNMET), "the skill is served as written");

    // Deploys: the agent composes and is written, the skill is written.
    let roster = AgentRoster::load(&content).expect("roster");
    let agents_src = tmp.path().join("agents-src");
    roster.materialize(&agents_src).expect("materialize");
    let agents_dst = tmp.path().join("agents");
    let skills_dst = tmp.path().join("skills");
    let deployed =
        crate::core::agent_deployer::deploy_agents(&agents_src, &agents_dst, &skills_dst)
            .expect("agent deploy");
    assert!(deployed.failed.is_empty(), "{:?}", deployed.failed);
    assert!(deployed.deployed.iter().any(|f| f == "engineer.md"));
    let skills_src = tmp.path().join("skills-src");
    std::fs::create_dir_all(&skills_src).expect("dir");
    std::fs::write(skills_src.join("tm-doctor.md"), skill).expect("skill");
    let stats =
        crate::core::skill_deployer::deploy_skills(&skills_src, &skills_dst).expect("skill deploy");
    assert!(
        stats.deployed.iter().any(|f| f.starts_with("tm-doctor")),
        "{stats:?}"
    );

    // Doctor: advisory WARN naming both assets and the requirement.
    let row = grade_requires(&content, &v("1.9.0"));
    assert_eq!(
        (row.name.as_str(), row.status),
        (CHECK_NAME, CheckStatus::Warn)
    );
    for needle in [
        "agents/engineer.md",
        "skills/tm-doctor.md",
        ">=999.0.0",
        "1.9.0",
    ] {
        assert!(
            row.message.contains(needle),
            "`{needle}` missing: {}",
            row.message
        );
    }
}

/// A `requires:` this binary satisfies, or one for another harness, is OK.
#[test]
fn a_satisfied_or_foreign_requires_is_ok() {
    let tmp = tempfile::tempdir().expect("tmp");
    checkout_with(
        tmp.path(),
        &[
            (
                "agents/a.md",
                "---\nname: a\nrequires: {tm: \">=1.8\"}\n---\n",
            ),
            (
                "skills/b.md",
                "---\nname: b\nrequires:\n  tcode: \">=99.0.0\"\n---\n",
            ),
            ("skills/c.md", "---\nname: c\n---\n"),
        ],
    );
    let content = checkout_content(tmp.path()).expect("content");
    let row = grade_requires(&content, &v("1.9.0"));
    assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
    assert!(row.message.contains("1 satisfied"), "{}", row.message);
    assert!(
        row.message.contains("1 for another harness"),
        "{}",
        row.message
    );
}

/// An unreadable `requires:` warns naming the asset; it is never FAIL.
#[test]
fn a_malformed_requires_warns_naming_the_asset() {
    let tmp = tempfile::tempdir().expect("tmp");
    checkout_with(
        tmp.path(),
        &[("instructions/x.md", "---\nrequires: \">=1.0\"\n---\n")],
    );
    let content = checkout_content(tmp.path()).expect("content");
    let row = grade_requires(&content, &v("1.9.0"));
    assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
    assert!(row.message.contains("instructions/x.md"), "{}", row.message);
}

/// No content resolves: the row says so, as WARN, and never fails.
#[test]
fn the_row_never_fails_when_no_content_resolves() {
    let empty = tempfile::tempdir().expect("tmp");
    let row = check_content_requires_in(Some(empty.path()), Some(empty.path()), None);
    assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
    assert!(row.message.contains("not checked"), "{}", row.message);
}
