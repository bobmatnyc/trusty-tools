//! Tests for the ancestor memory-file repairs (#7673).
//!
//! Split out with `#[path]` so `ancestor_claude_md_repair.rs` stays under the
//! 500-SLOC production cap.

use super::*;
use crate::core::instruction_pipeline::CLAUDE_MD_STUB;
use tempfile::TempDir;

fn fixture() -> (TempDir, PathBuf, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let ancestor = root.join("ancestor");
    let project = ancestor.join("project");
    std::fs::create_dir_all(&project).unwrap();
    (tmp, ancestor, project)
}

fn settings_of(project: &Path) -> PathBuf {
    project.join(".claude").join("settings.local.json")
}

#[test]
fn the_check_name_matches_the_doctor_row() {
    assert_eq!(ANCESTOR_CHECK, "ancestor_claude_md");
}

/// FAILS BEFORE THIS CHANGE: `--fix` had no step for this finding at all.
#[test]
fn a_seed_ancestor_is_renamed_aside() {
    let (_tmp, ancestor, project) = fixture();
    let seed = ancestor.join("CLAUDE.md");
    std::fs::write(&seed, CLAUDE_MD_STUB).unwrap();
    // The project's own CLAUDE.md must be untouched by every repair here.
    let own = project.join("CLAUDE.md");
    std::fs::write(&own, CLAUDE_MD_STUB).unwrap();

    let steps = repair_ancestor_claude_md(&project, None, None, "20260912", RepairMode::Apply);

    assert_eq!(steps.len(), 1, "{steps:?}");
    assert!(
        matches!(steps[0].status, StepStatus::Applied { .. }),
        "{steps:?}"
    );
    assert!(!seed.exists(), "the seed is renamed out of the loaded name");
    assert_eq!(
        std::fs::read_to_string(ancestor.join("CLAUDE.md.stale-seed-20260912")).unwrap(),
        CLAUDE_MD_STUB
    );
    assert_eq!(
        std::fs::read_to_string(&own).unwrap(),
        CLAUDE_MD_STUB,
        "a file INSIDE the project root is never touched"
    );
}

/// FAILS BEFORE THIS CHANGE: there was no `claudeMdExcludes` repair, and the
/// briefed rename would have moved a file carrying the operator's own notes.
#[test]
fn a_content_ancestor_is_excluded_not_renamed() {
    let (_tmp, ancestor, project) = fixture();
    let notes = ancestor.join("CLAUDE.md");
    std::fs::write(&notes, "# Monorepo\n\nAll packages use pnpm.\n").unwrap();

    let steps = repair_ancestor_claude_md(&project, None, None, "20260912", RepairMode::Apply);

    assert_eq!(steps.len(), 1, "{steps:?}");
    assert!(
        matches!(steps[0].status, StepStatus::Applied { .. }),
        "{steps:?}"
    );
    assert!(notes.is_file(), "a file with real content is never renamed");
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(settings_of(&project)).unwrap()).unwrap();
    assert_eq!(
        value["claudeMdExcludes"],
        serde_json::json!([notes.display().to_string()])
    );
}

#[test]
fn dry_run_writes_nothing() {
    let (_tmp, ancestor, project) = fixture();
    let seed = ancestor.join("CLAUDE.md");
    std::fs::write(&seed, CLAUDE_MD_STUB).unwrap();
    std::fs::write(ancestor.join("CLAUDE.local.md"), "# notes\n").unwrap();

    let steps = repair_ancestor_claude_md(&project, None, None, "20260912", RepairMode::DryRun);

    assert_eq!(steps.len(), 2, "{steps:?}");
    assert!(
        steps.iter().all(|s| s.status == StepStatus::Planned),
        "{steps:?}"
    );
    assert!(
        seed.is_file(),
        "dry run leaves the seed exactly where it was"
    );
    assert!(
        !settings_of(&project).exists(),
        "dry run seeds no settings file"
    );
}

#[test]
fn an_already_excluded_ancestor_yields_no_step() {
    let (_tmp, ancestor, project) = fixture();
    let notes = ancestor.join("CLAUDE.md");
    std::fs::write(&notes, "# Monorepo\n").unwrap();
    let settings = settings_of(&project);
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(
        &settings,
        serde_json::json!({ "claudeMdExcludes": [notes.display().to_string()] }).to_string(),
    )
    .unwrap();

    let steps = repair_ancestor_claude_md(&project, None, None, "20260912", RepairMode::Apply);

    assert!(steps.is_empty(), "{steps:?}");
}

/// A second `--fix` on the same day must not overwrite the first run's rescue
/// copy, which would destroy the only remaining bytes of the original.
#[test]
fn a_rename_target_that_exists_is_refused() {
    let (_tmp, ancestor, project) = fixture();
    std::fs::write(ancestor.join("CLAUDE.md"), CLAUDE_MD_STUB).unwrap();
    std::fs::write(
        ancestor.join("CLAUDE.md.stale-seed-20260912"),
        "an earlier rescue copy\n",
    )
    .unwrap();

    let steps = repair_ancestor_claude_md(&project, None, None, "20260912", RepairMode::Apply);

    assert_eq!(steps.len(), 1);
    assert!(
        matches!(steps[0].status, StepStatus::Refused(_)),
        "{steps:?}"
    );
    assert_eq!(
        std::fs::read_to_string(ancestor.join("CLAUDE.md.stale-seed-20260912")).unwrap(),
        "an earlier rescue copy\n"
    );
}
