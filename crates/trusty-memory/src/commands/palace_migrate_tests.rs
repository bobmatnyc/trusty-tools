//! #10094: `palace migrate` requests — `--all`, its `--yes` gate, the exit status.
//!
//! Why: the request runner is shared by the CLI's offline path and the
//! daemon's `palace_migrate` tool, so its refusals and its `ok` verdict are
//! what both routes report.
//! What: palaces under `<tmp>/palaces`, driven through [`run_request`],
//! [`outcome`] and the renderer.
//! Test: this file.

use super::{MigrateMode, MigrateRequest, outcome, run_request};
use crate::commands::palace_migrate_render::render;
use crate::engine::store::format_upgrade::{UpgradeSeams, migrate_with};
use crate::engine::store::format_v0_fixture::write_format_0_palace;
use crate::engine::store::format_v1_fixture::write_format_1_palace;
use crate::engine::store::kg_store::DRAWERS;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A registry with a format-0 palace `a` and a format-1 palace `b`.
fn two_palaces() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("palaces");
    write_format_0_palace(&root.join("a"), "a").expect("a");
    write_format_1_palace(&root.join("b"), "b").expect("b");
    (tmp, root)
}

fn req(palace: Option<&str>, all: bool, mode: MigrateMode, yes: bool) -> MigrateRequest {
    MigrateRequest {
        palace: palace.map(str::to_string),
        all,
        mode,
        yes,
    }
}

/// Every file and directory under `root`, with bytes and modification time.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).expect("read_dir") {
            let p = e.expect("entry").path();
            let meta = std::fs::metadata(&p).expect("stat");
            let bytes = if meta.is_dir() {
                stack.push(p.clone());
                Vec::new()
            } else {
                std::fs::read(&p).expect("read")
            };
            out.insert(p, (bytes, meta.modified().expect("mtime")));
        }
    }
    out
}

/// `--all --dry-run` lists every palace with its plan and writes nothing.
#[test]
fn dry_run_all_reports_every_palace_and_writes_nothing() {
    let (tmp, root) = two_palaces();
    let before = snapshot(tmp.path());
    let v = run_request(&root, &req(None, true, MigrateMode::DryRun, false)).expect("dry run");
    assert_eq!(snapshot(tmp.path()), before, "a dry run writes nothing");
    assert_eq!(v["ok"], true, "{v}");
    let plans = v["plans"].as_array().expect("plans");
    assert_eq!(plans.len(), 2, "{v}");
    assert_eq!(
        (plans[0]["found"].as_u64(), plans[1]["found"].as_u64()),
        (Some(0), Some(1))
    );
    assert!(
        v["total_needed_bytes"].as_u64().is_some_and(|n| n > 0),
        "{v}"
    );
    let text = render(&v).expect("render");
    assert!(text.contains("nothing is written"), "{text}");
    assert!(text.contains("needs migration"), "{text}");
    assert!(text.contains("2 of 2 palace(s) would change"), "{text}");
    outcome(&v).expect("a dry run of migratable palaces exits zero");
}

/// `--all` without `--yes` refuses when more than one palace would change,
/// and changes nothing.
#[test]
fn all_without_yes_refuses_and_changes_nothing() {
    let (tmp, root) = two_palaces();
    let before = snapshot(tmp.path());
    let err = run_request(&root, &req(None, true, MigrateMode::Migrate, false))
        .expect_err("refused without --yes");
    let shown = format!("{err:#}");
    assert!(shown.contains("2 palaces would change (a, b)"), "{shown}");
    assert!(shown.contains("--yes"), "{shown}");
    assert!(shown.contains("Nothing was changed"), "{shown}");
    assert_eq!(snapshot(tmp.path()), before);
}

/// `--all --yes` migrates every palace behind; a second run changes none.
#[test]
fn all_with_yes_migrates_every_palace_that_needs_it() {
    let (_tmp, root) = two_palaces();
    let v = run_request(&root, &req(None, true, MigrateMode::Migrate, true)).expect("migrate");
    assert_eq!(v["ok"], true, "{v}");
    let results = v["results"].as_array().expect("results");
    assert_eq!(results.len(), 2, "{v}");
    assert!(results.iter().all(|r| r["migrated"] == true), "{v}");
    outcome(&v).expect("exit zero");
    let text = render(&v).expect("render");
    assert!(text.contains("palace a: migrated format 0 -> 2"), "{text}");
    assert!(text.contains("palace b: migrated format 1 -> 2"), "{text}");

    let again = run_request(&root, &req(None, true, MigrateMode::Migrate, false))
        .expect("nothing to change needs no --yes");
    assert_eq!(
        again["results"].as_array().map(Vec::len),
        Some(0),
        "{again}"
    );
    let verified =
        run_request(&root, &req(Some("a"), false, MigrateMode::Verify, false)).expect("verify");
    assert_eq!(verified["ok"], true, "{verified}");
}

/// A request names one palace or `--all`, and an unknown palace is an error.
#[test]
fn a_request_names_one_existing_palace_or_all() {
    let (_tmp, root) = two_palaces();
    let missing = run_request(&root, &req(Some("zz"), false, MigrateMode::Verify, false));
    assert!(format!("{:#}", missing.expect_err("missing")).contains("no palace named 'zz'"));
    assert!(run_request(&root, &req(Some("a"), true, MigrateMode::DryRun, false)).is_err());
    assert!(run_request(&root, &req(None, false, MigrateMode::DryRun, false)).is_err());
}

/// Drop one drawer row, as a step that lost a row would.
fn drop_a_drawer(dir: &Path) {
    let db = trusty_common::redb_cache::create_palace_db(&dir.join("kg.redb")).expect("kg");
    let wtx = db.begin_write().expect("txn");
    wtx.open_table(DRAWERS)
        .expect("drawers")
        .pop_first()
        .expect("pop")
        .expect("a row");
    wtx.commit().expect("commit");
}

/// A post-migration check that fails is a non-zero exit naming the backup;
/// so is a `--verify` that finds a gap.
#[test]
fn a_failed_check_is_a_non_zero_exit_naming_the_backup() {
    let (_tmp, root) = two_palaces();
    let seams = UpgradeSeams {
        after_chain: drop_a_drawer,
        ..UpgradeSeams::default()
    };
    let r = migrate_with(&root.join("a"), "a", seams).expect("the chain itself succeeds");
    let backup = r.backup.clone().expect("a backup");
    assert!(
        r.problems.iter().any(|p| p.contains("row counts changed")),
        "{r:?}"
    );
    let v = json!({"mode": MigrateMode::Migrate, "ok": false, "results": [r]});
    let text = render(&v).expect("render");
    assert!(text.contains("FAILED: row counts changed"), "{text}");
    let err = format!("{:#}", outcome(&v).expect_err("non-zero exit"));
    assert!(err.contains(&backup.display().to_string()), "{err}");
    assert!(err.contains("nothing was deleted"), "{err}");

    // `b` is at format 1: `--verify` names the gap and exits non-zero.
    let v = run_request(&root, &req(Some("b"), false, MigrateMode::Verify, false)).expect("verify");
    assert_eq!(v["ok"], false, "{v}");
    let err = format!("{:#}", outcome(&v).expect_err("non-zero exit"));
    assert!(
        err.contains("palace b: format status is needs migration"),
        "{err}"
    );
}
