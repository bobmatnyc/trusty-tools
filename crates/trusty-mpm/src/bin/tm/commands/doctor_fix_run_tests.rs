//! Tests for `doctor_fix_run` (#10425): the `--only` scope, the dispatch
//! table, the fail-closed `NOT EVALUATED` report, a dry run over a fake root,
//! and the CLI flags that select them.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use clap::Parser;
use trusty_mpm::core::doctor_repair::{RepairMode, StepStatus};

use super::*;
use crate::cli::{Cli, Command};

/// The repairs whose inputs are all paths the context supplies — none reads
/// the operator's framework root or a daemon — so a fake root covers every
/// byte they could touch.
const HERMETIC: &[&str] = &[
    "hooks_build_tree_binary",
    "hook-interpreter",
    "statusline",
    "hooks_contamination",
    "hooks_missing_tm_group",
    "push_guard",
    "session_scope",
    "ancestor_claude_md",
    "auto_memory",
    "rust_build_env",
    "legacy_sources",
    "launchd_process_type",
];

/// Every entry under `root`: relative path to `(mode, bytes)`; a dir has none.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, (u32, Option<Vec<u8>>)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            let rel = path.strip_prefix(root).unwrap().to_path_buf();
            if meta.is_dir() {
                out.insert(rel, (meta.permissions().mode(), None));
                stack.push(path);
            } else {
                let bytes = std::fs::read(&path).unwrap();
                out.insert(rel, (meta.permissions().mode(), Some(bytes)));
            }
        }
    }
    out
}

/// A fake root: `home/`, `managed/`, and `project/` holding one tm hook entry.
fn fake_root() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let root = crate::test_support::hermetic_temp_dir();
    let home = root.path().join("home");
    let managed = root.path().join("managed");
    let project = root.path().join("project");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::create_dir_all(&managed).unwrap();
    std::fs::create_dir_all(project.join(".claude")).unwrap();
    let settings = serde_json::json!({
        "hooks": { "PreToolUse": [
            { "hooks": [{ "type": "command", "command": "tm hook" }] }
        ]}
    });
    std::fs::write(project.join(".claude/settings.json"), settings.to_string()).unwrap();
    (root, home, managed, project)
}

#[test]
fn every_repair_id_dispatches_and_no_other_does() {
    for id in FIX_REPAIR_IDS {
        assert!(dispatch(id).is_some(), "`{id}` has no repair");
    }
    assert!(dispatch("no_such_check").is_none());
    let unique: std::collections::BTreeSet<_> = FIX_REPAIR_IDS.iter().collect();
    assert_eq!(unique.len(), FIX_REPAIR_IDS.len(), "duplicate repair id");
}

#[test]
fn an_unknown_only_id_is_an_error() {
    let err = resolve_scope(&["push_guard".into(), "bogus".into()]).unwrap_err();
    assert!(err.to_string().contains("`--only bogus`"), "{err}");
}

#[test]
fn only_selects_in_run_order() {
    assert_eq!(resolve_scope(&[]).unwrap(), FIX_REPAIR_IDS.to_vec());
    let scope = resolve_scope(&[
        "push_guard".into(),
        "hooks_contamination".into(),
        "push_guard".into(),
    ])
    .unwrap();
    assert_eq!(scope, vec!["hooks_contamination", "push_guard"]);
}

#[test]
fn the_apply_hint_keeps_the_only_scope() {
    assert_eq!(apply_hint(FIX_REPAIR_IDS, true), "tm doctor --fix --yes");
    assert_eq!(
        apply_hint(&["hooks_contamination", "push_guard"], false),
        "tm doctor --fix --only hooks_contamination --only push_guard --yes"
    );
}

#[test]
fn a_repair_without_its_prerequisite_is_reported_not_evaluated() {
    let ctx = FixContext::new(RepairMode::DryRun, false, None, None, None);
    let scope = [
        "hooks_contamination",
        "legacy_sources",
        "claude_json_secrets",
        "rust_build_env",
    ];
    let (steps, skipped) = collect(&ctx, &scope);
    assert!(steps.is_empty(), "{steps:?}");
    let ids: Vec<&str> = skipped.iter().map(|(id, _)| *id).collect();
    assert_eq!(ids, scope.to_vec());
}

#[test]
fn a_dry_run_changes_nothing_under_a_fake_root() {
    let (root, home, managed, project) = fake_root();
    let before = snapshot(root.path());
    let ctx = FixContext::new(
        RepairMode::DryRun,
        false,
        Some(project),
        Some(home),
        Some(managed),
    );

    let (steps, _skipped) = collect(&ctx, HERMETIC);

    assert_eq!(
        before,
        snapshot(root.path()),
        "the dry run wrote: {steps:?}"
    );
    assert!(
        steps
            .iter()
            .any(|s| s.check == "hooks_contamination" && s.status == StepStatus::Planned),
        "the seeded tm hook must be planned: {steps:?}"
    );
    assert!(
        steps.iter().all(|s| !s.changed()),
        "a dry run reported an applied step: {steps:?}"
    );
}

#[test]
fn only_runs_just_the_named_repair() {
    let (_root, home, managed, project) = fake_root();
    let ctx = FixContext::new(
        RepairMode::DryRun,
        false,
        Some(project),
        Some(home),
        Some(managed),
    );
    let scope = resolve_scope(&["hooks_contamination".into()]).unwrap();
    let (steps, skipped) = collect(&ctx, &scope);
    assert!(skipped.is_empty(), "{skipped:?}");
    assert!(!steps.is_empty());
    assert!(
        steps.iter().all(|s| s.check == "hooks_contamination"),
        "{steps:?}"
    );
}

/// `--dry-run` requires `--repair` and parses beside it.
#[test]
fn cli_parses_validate_repair_dry_run() {
    let cli = Cli::try_parse_from(["trusty-mpm", "validate", "--repair", "--dry-run"]).unwrap();
    assert!(matches!(
        cli.command.unwrap(),
        Command::Validate {
            repair: true,
            dry_run: true,
            ..
        }
    ));
    assert!(Cli::try_parse_from(["trusty-mpm", "validate", "--dry-run"]).is_err());
}

/// `--dry-run` previews any write action and refuses `--yes`; `--only` is
/// repeatable and needs `--fix`.
#[test]
fn cli_parses_doctor_fix_dry_run_and_only() {
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "doctor",
        "--fix",
        "--dry-run",
        "--only",
        "push_guard",
        "--only",
        "hooks_contamination",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Doctor { flags } => {
            assert!(flags.fix && flags.dry_run && !flags.yes);
            assert_eq!(flags.only, vec!["push_guard", "hooks_contamination"]);
        }
        other => panic!("expected Command::Doctor, got {other:?}"),
    }
    for bad in [
        &["trusty-mpm", "doctor", "--fix", "--dry-run", "--yes"][..],
        &["trusty-mpm", "doctor", "--dry-run"][..],
        &["trusty-mpm", "doctor", "--only", "push_guard"][..],
        &[
            "trusty-mpm",
            "doctor",
            "--fix-skills",
            "--only",
            "push_guard",
        ][..],
    ] {
        assert!(Cli::try_parse_from(bad).is_err(), "{bad:?} must not parse");
    }
}

/// An unknown `--only` id is a parse error naming it, never a no-op.
#[test]
fn cli_rejects_an_unknown_doctor_only_id() {
    let err = Cli::try_parse_from([
        "trusty-mpm",
        "doctor",
        "--fix",
        "--only",
        "hooks_contaminaton",
    ])
    .unwrap_err()
    .to_string();
    assert!(err.contains("hooks_contaminaton"), "{err}");
}
