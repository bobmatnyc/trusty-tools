//! Tests for `palace reclaim --apply` and `--purge-trash` (#9140 ruling f0).
//! Every test runs on a tempdir root with an injected home and drawer remover;
//! none resolves the live data dir or dials the live daemon.

use super::*;
use crate::commands::palace_reclaim::tests::{backdate, drawer, seed_palace};
use crate::commands::palace_reclaim_trash::{purge_trash, TrashManifest, MANIFEST_FILE};
use chrono::NaiveDate;
use trusty_common::memory_core::palace::Drawer;

const DATE: &str = "2026-10-04";

/// A palace root holding one item of every class, plus a home dir outside it.
struct Scene {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    now: i64,
    fixtures: Vec<Drawer>,
}

fn scene(fixture_count: usize) -> Scene {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("palaces");
    let home = tmp.path().join("home");
    std::fs::create_dir(&root).expect("root");
    std::fs::create_dir(&home).expect("home");
    let now = chrono::Utc::now().timestamp();

    let empty = seed_palace(&root, "old-empty", &[]);
    std::fs::write(empty.join("index.usearch.redb.v2-incompatible"), b"0123").expect("w");
    backdate(&empty, now, 40);

    let fixtures: Vec<Drawer> = (0..fixture_count)
        .map(|_| drawer("User: say hi\n\nAssistant: hello", &["turn"]))
        .collect();
    let mut drawers = fixtures.clone();
    drawers.push(drawer("User: a real question\n\nAssistant: ok", &["turn"]));
    let busy = seed_palace(&root, "busy", &drawers);
    std::fs::write(busy.join("kg.redb.pre-compact.bak"), b"backup").expect("w");

    std::fs::create_dir(root.join("t-orphan")).expect("orphan");
    std::fs::write(root.join("t-orphan/recall.redb"), b"orphan").expect("w");
    std::fs::write(root.join("uds_addr"), "/nonexistent/m1-9140.sock").expect("w");
    std::fs::write(root.join("activity.redb.v2-incompatible"), b"old").expect("w");
    Scene {
        _tmp: tmp,
        root,
        home,
        now,
        fixtures,
    }
}

/// The reviewed list exactly as an operator saves it: the dry run's JSON.
fn review(s: &Scene) -> ReviewedList {
    let report = scan(&s.root, s.now).expect("scan");
    serde_json::from_value(serde_json::to_value(&report).expect("to json")).expect("from json")
}

/// Run [`apply`] with a remover that records each call and answers `answer`.
fn run(
    s: &Scene,
    list: &ReviewedList,
    answer: impl Fn(&str) -> Result<()>,
) -> (Result<ApplyReport>, Vec<(String, String)>) {
    let mut calls = Vec::new();
    let mut remover = |p: &str, id: &str| {
        calls.push((p.to_string(), id.to_string()));
        answer(id)
    };
    let mut env = ApplyEnv {
        home: Some(&s.home),
        now_unix: s.now,
        date: DATE.into(),
        reviewed_path: PathBuf::from("reviewed.json"),
        remove_drawer: &mut remover,
    };
    let report = apply(&s.root, list, &mut env);
    (report, calls)
}

fn status_of(r: &ApplyReport, path: &Path) -> Vec<EntryStatus> {
    r.entries
        .iter()
        .filter(|e| e.original_path == path && e.drawer_id.is_none())
        .map(|e| e.status)
        .collect()
}

fn trash(s: &Scene) -> PathBuf {
    s.root.join(".trash").join(format!("{DATE}-reclaim"))
}

/// Why (#9140 ruling f0, AC 1-2): every reviewed item moves by rename to the
/// dated trash dir at its relative path, a nested item rides with its palace,
/// the manifest records each one, and the trash is not re-listed by the next
/// dry run.
/// Test: itself.
#[test]
fn apply_moves_the_reviewed_items_and_writes_the_manifest() {
    let s = scene(1);
    let list = review(&s);
    let (report, calls) = run(&s, &list, |_| Ok(()));
    let report = report.expect("apply");
    assert_eq!(report.not_applied(), 0, "{}", report.render_text());
    assert!(report.new_since_review.is_empty());
    let t = trash(&s);
    for rel in [
        "old-empty",
        "t-orphan",
        "uds_addr",
        "activity.redb.v2-incompatible",
        "busy/kg.redb.pre-compact.bak",
    ] {
        assert!(!s.root.join(rel).exists(), "{rel} must have moved");
        assert!(t.join(rel).exists(), "{rel} must be in the trash");
    }
    assert!(t
        .join("old-empty/index.usearch.redb.v2-incompatible")
        .is_file());
    assert_eq!(
        status_of(
            &report,
            &s.root.join("old-empty/index.usearch.redb.v2-incompatible")
        ),
        vec![EntryStatus::MovedWithParent]
    );
    assert!(
        s.root.join("busy/kg.redb").is_file(),
        "a listed palace's store stays"
    );
    assert_eq!(
        calls,
        vec![("busy".to_string(), s.fixtures[0].id.to_string())]
    );

    let manifest: TrashManifest =
        serde_json::from_slice(&std::fs::read(t.join(MANIFEST_FILE)).expect("manifest"))
            .expect("parse manifest");
    assert_eq!(manifest.entries.len(), list.items.len());
    assert!(manifest
        .entries
        .iter()
        .all(|e| e.status != EntryStatus::Pending));
    let after = scan(&s.root, s.now).expect("rescan");
    assert!(
        after
            .items
            .iter()
            .all(|i| !i.path.starts_with(s.root.join(".trash"))),
        "the trash must not be re-listed: {}",
        after.render_text()
    );
}

/// Why (#9140 ruling f0, AC 1): an item that changed or vanished since the
/// review is skipped and reported, and an item that appeared is untouched.
/// Test: itself.
#[test]
fn apply_skips_an_item_that_changed_since_review() {
    let s = scene(0);
    let list = review(&s);
    let bak = s.root.join("busy/kg.redb.pre-compact.bak");
    std::fs::write(&bak, b"backup grew").expect("change");
    std::fs::remove_file(s.root.join("activity.redb.v2-incompatible")).expect("vanish");
    std::fs::create_dir(s.root.join("t-new")).expect("appear");

    let (report, _) = run(&s, &list, |_| Ok(()));
    let report = report.expect("apply");
    assert_eq!(report.unmatched.len(), 2, "{}", report.render_text());
    assert!(report.not_applied() > 0);
    assert_eq!(std::fs::read(&bak).expect("bak stays"), b"backup grew");
    assert!(
        s.root.join("t-new").is_dir(),
        "an unreviewed item is untouched"
    );
    assert!(report.new_since_review.iter().any(|l| l.ends_with("t-new")));
    assert!(
        !s.root.join("t-orphan").exists(),
        "unchanged items still move"
    );
}

/// Why (#9140 ruling f0): the scan-to-move window is closed by a second
/// size/mtime check under the store locks.
/// Test: itself.
#[test]
fn move_one_skips_an_item_changed_after_the_scan() {
    let s = scene(0);
    let list = review(&s);
    let item = list
        .items
        .iter()
        .find(|i| i.class == ReclaimClass::OrphanDir)
        .expect("orphan listed");
    std::fs::write(s.root.join("t-orphan/late.txt"), b"late").expect("change");
    let target = s.root.join("moved-here");
    let outcome = move_one(item, Some(&item.path), &target);
    assert!(matches!(outcome, Err(Outcome::Skip(_))));
    assert!(item.path.is_dir() && !target.exists());
}

/// Why (#9140 ruling f0): a rename onto an existing trash path would replace
/// an earlier trashed file, so an occupied target fails the item.
/// Test: itself.
#[test]
fn move_one_refuses_to_overwrite_an_existing_trash_path() {
    let s = scene(0);
    let list = review(&s);
    let item = list
        .items
        .iter()
        .find(|i| i.class == ReclaimClass::StaleUdsAddr)
        .expect("uds_addr listed");
    let target = s.root.join("occupied");
    std::fs::write(&target, b"earlier").expect("occupy");
    assert!(matches!(
        move_one(item, None, &target),
        Err(Outcome::Fail(_))
    ));
    assert_eq!(std::fs::read(&target).expect("kept"), b"earlier");
    assert!(item.path.is_file());
}

/// Why (#9140 ruling f0, AC 1): a list reviewed for another root names other
/// paths, so the apply refuses it outright.
/// Test: itself.
#[test]
fn apply_refuses_a_list_reviewed_for_another_root() {
    let s = scene(0);
    let mut list = review(&s);
    list.root = s.home.clone();
    let (report, _) = run(&s, &list, |_| Ok(()));
    assert!(report.is_err());
    assert!(!s.root.join(".trash").exists());
}

/// Why (#9140 ruling f0, AC 2): a `.trash` that is a symlink would put the
/// trash, and the moves, outside the root; the apply refuses it.
/// Test: itself.
#[cfg(unix)]
#[test]
fn apply_refuses_a_symlinked_trash_dir() {
    let s = scene(0);
    let list = review(&s);
    let elsewhere = s.home.join("elsewhere");
    std::fs::create_dir(&elsewhere).expect("elsewhere");
    std::os::unix::fs::symlink(&elsewhere, s.root.join(".trash")).expect("link");
    let (report, _) = run(&s, &list, |_| Ok(()));
    assert!(report.is_err());
    assert!(s.root.join("t-orphan").is_dir());
    assert_eq!(std::fs::read_dir(&elsewhere).expect("ls").count(), 0);
}

/// Why (#9140 ruling f0, AC 3): a palace whose redb store the live daemon
/// holds open is never moved, nor is anything inside it.
/// Test: itself.
#[test]
fn apply_skips_a_palace_whose_store_is_locked() {
    let s = scene(0);
    let list = review(&s);
    let store = s.root.join("old-empty/kg.redb");
    assert!(store.is_file(), "the seeded palace has a store to hold");
    let held = redb::Database::open(&store).expect("hold the store as the daemon does");

    let (report, _) = run(&s, &list, |_| Ok(()));
    let report = report.expect("apply");
    drop(held);
    let empty = s.root.join("old-empty");
    assert!(
        empty.join("kg.redb").is_file(),
        "a held palace stays in place"
    );
    let st = status_of(&report, &empty);
    assert_eq!(st, vec![EntryStatus::Skipped], "{}", report.render_text());
    let why = report
        .entries
        .iter()
        .find(|e| e.original_path == empty)
        .and_then(|e| e.detail.clone())
        .unwrap_or_default();
    assert!(why.contains("held open by the live daemon"), "{why}");
    assert_eq!(
        status_of(&report, &empty.join("index.usearch.redb.v2-incompatible")),
        vec![EntryStatus::Skipped]
    );
    assert!(report.not_applied() >= 2);
}

/// Why (#9140 ruling f0, AC 3): the root must never be `/`, home, or an
/// ancestor of home; an unknown home refuses too.
/// Test: itself.
#[test]
fn reclaim_root_check_refuses_root_home_and_its_ancestors() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("home");
    let data = home.join("data");
    std::fs::create_dir_all(&data).expect("dirs");
    for (root, home_arg) in [
        (Path::new("/"), Some(home.as_path())),
        (home.as_path(), Some(home.as_path())),
        (tmp.path(), Some(home.as_path())),
        (Path::new("relative/root"), Some(home.as_path())),
        (data.as_path(), None),
    ] {
        assert!(
            check_root(root, home_arg).is_err(),
            "{} must be refused",
            root.display()
        );
    }
    check_root(&data, Some(&home)).expect("a dir inside home is a valid root");
}

/// Why (#9140 ruling f0, AC 3): a bad root stops the apply before it scans,
/// creates a trash dir, or moves anything.
/// Test: itself.
#[test]
fn apply_refuses_a_bad_root_before_touching_anything() {
    let mut s = scene(0);
    let list = review(&s);
    s.home = s.root.join("inside-home-check");
    std::fs::create_dir(&s.home).expect("home under root");
    let (report, calls) = run(&s, &list, |_| Ok(()));
    let err = report.expect_err("an ancestor of home is refused");
    assert!(err.to_string().contains("ancestor"), "{err:#}");
    assert!(calls.is_empty());
    assert!(!s.root.join(".trash").exists(), "no trash dir is made");
    assert!(s.root.join("t-orphan").is_dir() && s.root.join("uds_addr").is_file());
}

/// Why (#9140 ruling f0, AC 3): a failed rename leaves that item in place,
/// records it failed, and the run carries on; nothing is copied or deleted.
/// Test: itself.
#[cfg(unix)]
#[test]
fn apply_continues_past_a_rename_failure_and_deletes_nothing() {
    use std::os::unix::fs::PermissionsExt as _;
    let s = scene(0);
    let list = review(&s);
    let busy = s.root.join("busy");
    std::fs::set_permissions(&busy, std::fs::Permissions::from_mode(0o555)).expect("chmod");
    let (report, _) = run(&s, &list, |_| Ok(()));
    std::fs::set_permissions(&busy, std::fs::Permissions::from_mode(0o755)).expect("chmod back");
    let report = report.expect("apply");
    let bak = busy.join("kg.redb.pre-compact.bak");
    assert_eq!(status_of(&report, &bak), vec![EntryStatus::Failed]);
    assert_eq!(std::fs::read(&bak).expect("bak stays"), b"backup");
    assert!(!trash(&s).join("busy/kg.redb.pre-compact.bak").exists());
    assert!(!s.root.join("uds_addr").exists(), "later items still move");
    assert_eq!(report.not_applied(), 1, "{}", report.render_text());
}

/// Why (#9140 ruling f0): fixture drawers are exported to the trash before
/// the daemon removes them; a removal that fails leaves no export behind.
/// Test: itself.
#[test]
fn apply_routes_fixture_drawers_through_the_remover_and_exports_them() {
    let s = scene(2);
    let list = review(&s);
    let refused = s.fixtures[1].id.to_string();
    let refused2 = refused.clone();
    let (report, calls) = run(&s, &list, move |id| {
        if id == refused2 {
            Err(anyhow!("daemon refused"))
        } else {
            Ok(())
        }
    });
    let report = report.expect("apply");
    assert_eq!(calls.len(), 2);
    let exports = trash(&s).join("busy/kg.redb.drawers");
    let kept = exports.join(format!("{}.json", s.fixtures[0].id));
    let body = std::fs::read_to_string(&kept).expect("export written");
    assert!(body.contains("say hi"), "{body}");
    assert!(!exports.join(format!("{refused}.json")).exists());
    let failed: Vec<_> = report
        .entries
        .iter()
        .filter(|e| e.status == EntryStatus::Failed)
        .filter_map(|e| e.drawer_id.clone())
        .collect();
    assert_eq!(failed, vec![refused]);
}

/// A dated trash dir under `base` with an optional manifest.
fn trash_dir(base: &Path, name: &str, manifest: bool) -> PathBuf {
    let d = base.join(name);
    std::fs::create_dir_all(&d).expect("mkdir");
    std::fs::write(d.join("item"), b"x").expect("item");
    if manifest {
        std::fs::write(d.join(MANIFEST_FILE), b"{}").expect("manifest");
    }
    d
}

/// Why (#9140 ruling f0, AC 4): the purge removes only real, dated,
/// manifest-bearing dirs under `.trash` older than 7 days, and never follows
/// a symlink out of the trash.
/// Test: itself.
#[cfg(unix)]
#[test]
fn purge_removes_only_old_dated_trash_dirs_with_a_manifest() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("palaces");
    let base = root.join(".trash");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).expect("outside");
    std::fs::write(outside.join("precious"), b"keep").expect("precious");
    let today = NaiveDate::from_ymd_opt(2026, 10, 4).expect("date");

    let old = trash_dir(&base, "2026-09-01-reclaim", true);
    symlink(outside.join("precious"), old.join("link")).expect("link");
    let edge_gone = trash_dir(&base, "2026-09-26-reclaim", true);
    let edge_kept = trash_dir(&base, "2026-09-27-reclaim", true);
    let recent = trash_dir(&base, "2026-10-01-reclaim", true);
    let no_manifest = trash_dir(&base, "2026-09-02-reclaim", false);
    let undated = trash_dir(&base, "old-stuff", true);
    let suffixed = trash_dir(&base, "2026-09-01-reclaim-x", true);
    let outside_dated = trash_dir(&outside, "2026-09-03-reclaim", true);
    symlink(&outside_dated, base.join("2026-09-03-reclaim")).expect("dir link");
    let non_trash = trash_dir(&root, "2026-09-01-reclaim", true);
    let link_manifest = trash_dir(&base, "2026-09-05-reclaim", false);
    symlink(outside.join("precious"), link_manifest.join(MANIFEST_FILE)).expect("mlink");

    let report = purge_trash(&root, today).expect("purge");
    assert_eq!(report.removed, vec![old.clone(), edge_gone.clone()]);
    assert!(report.failed.is_empty());
    for kept in [
        &edge_kept,
        &recent,
        &no_manifest,
        &undated,
        &suffixed,
        &link_manifest,
    ] {
        assert!(kept.is_dir(), "{} must be kept", kept.display());
    }
    assert!(base.join("2026-09-03-reclaim").symlink_metadata().is_ok());
    assert!(outside_dated.join(MANIFEST_FILE).is_file());
    assert!(
        non_trash.join(MANIFEST_FILE).is_file(),
        "a path outside .trash"
    );
    assert_eq!(
        std::fs::read(outside.join("precious")).expect("target"),
        b"keep"
    );
}

/// Why (#9140 ruling f0, AC 4): a `.trash` that is a symlink is refused, so
/// the purge can never reach a directory outside the root.
/// Test: itself.
#[cfg(unix)]
#[test]
fn purge_refuses_a_symlinked_trash_base() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("palaces");
    std::fs::create_dir(&root).expect("root");
    let outside = tmp.path().join("elsewhere");
    let victim = trash_dir(&outside, "2020-01-01-reclaim", true);
    std::os::unix::fs::symlink(&outside, root.join(".trash")).expect("link");
    let today = NaiveDate::from_ymd_opt(2026, 10, 4).expect("date");
    assert!(purge_trash(&root, today).is_err());
    assert!(victim.join(MANIFEST_FILE).is_file());
}
