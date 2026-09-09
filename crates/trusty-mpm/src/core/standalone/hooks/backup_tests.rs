//! Unit tests for [`super`] (timestamped `settings.json` snapshots, #7244).
//!
//! Why: split out of `backup.rs` so the production file stays focused,
//! matching the `hooks/{mod.rs,tests.rs}` and `hooks/{cleanup.rs,cleanup_tests.rs}`
//! splits already used in this directory.
//! What: pins the clock through `snapshot_then_prune_at` to exercise naming,
//! same-second collision suffixing, prune ordering, the foreign-name
//! exclusions, and the fail-closed copy error.
//! Test: this module IS the test suite for `super`.

use super::*;
use tempfile::TempDir;

/// A fixed clock, so a snapshot's name never races a second boundary.
fn at(stamp: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(stamp)
        .expect("test stamp is valid RFC 3339")
        .with_timezone(&Utc)
}

/// Every timestamped snapshot of `settings.json` in `dir`, name-sorted.
fn snapshots(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("readable temp dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| is_snapshot_of("settings.json", n))
        .collect();
    names.sort();
    names
}

/// Why (#7244): the whole point of the snapshot is that an operator can read
/// back the file as it was BEFORE the rewrite. A snapshot that is empty, or
/// that captures the post-write bytes, restores nothing.
/// What: writes a known payload, snapshots it, and asserts the snapshot's name
/// carries the pinned stamp and its bytes equal the pre-snapshot file.
#[test]
fn snapshot_then_prune_copies_the_existing_file() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("settings.json");
    std::fs::write(&path, "{\"hooks\":\"before\"}").unwrap();

    let snapshot = snapshot_then_prune_at(&path, 3, at("2026-09-09T14:45:00Z"))
        .expect("snapshot succeeds")
        .expect("an existing file is snapshotted");

    assert_eq!(
        snapshot.file_name().unwrap().to_string_lossy(),
        "settings.json.20260909T144500Z.bak"
    );
    assert_eq!(
        std::fs::read_to_string(&snapshot).unwrap(),
        "{\"hooks\":\"before\"}",
        "the snapshot must be a byte-identical copy of the pre-rewrite file"
    );
}

/// Why: a rewrite of a file that does not exist yet has nothing to preserve,
/// and refusing it would make the FIRST hooks write on a clean project fail.
/// What: asserts `Ok(None)` and that no file appeared in the directory.
#[test]
fn snapshot_then_prune_ignores_a_missing_file() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("settings.json");

    let outcome = snapshot_then_prune_at(&path, 3, at("2026-09-09T14:45:00Z"))
        .expect("a missing source is not an error");

    assert!(outcome.is_none(), "nothing to snapshot means no snapshot");
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
}

/// Why: the stamp is second-precision, so two managed launches inside one
/// second would otherwise both claim the same name and the second would
/// overwrite the first — destroying the older state the feature exists to keep.
/// What: snapshots twice at the SAME pinned instant with different content and
/// asserts two distinct files whose contents are the two distinct payloads.
#[test]
fn snapshot_then_prune_suffixes_a_same_second_collision() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("settings.json");
    let now = at("2026-09-09T14:45:00Z");

    std::fs::write(&path, "first").unwrap();
    snapshot_then_prune_at(&path, 3, now).unwrap().unwrap();
    std::fs::write(&path, "second").unwrap();
    snapshot_then_prune_at(&path, 3, now).unwrap().unwrap();

    assert_eq!(
        snapshots(tmp.path()),
        vec![
            "settings.json.20260909T144500Z-1.bak".to_string(),
            "settings.json.20260909T144500Z.bak".to_string(),
        ]
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("settings.json.20260909T144500Z.bak")).unwrap(),
        "first"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("settings.json.20260909T144500Z-1.bak")).unwrap(),
        "second"
    );
}

/// Why: every managed launch that changes the file adds a snapshot, so without
/// a prune a long-lived project accumulates one per launch forever.
/// What: four snapshots at four pinned seconds, then asserts exactly three
/// survive and the one removed is the OLDEST — dropping the newest would keep
/// the archive bounded while throwing away the copy an operator actually wants.
#[test]
fn snapshot_then_prune_keeps_only_the_newest_three() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("settings.json");

    for second in 0..4 {
        std::fs::write(&path, format!("state-{second}")).unwrap();
        snapshot_then_prune_at(
            &path,
            HOOK_SETTINGS_SNAPSHOTS_KEPT,
            at(&format!("2026-09-09T14:45:0{second}Z")),
        )
        .unwrap()
        .unwrap();
    }

    assert_eq!(
        snapshots(tmp.path()),
        vec![
            "settings.json.20260909T144501Z.bak".to_string(),
            "settings.json.20260909T144502Z.bak".to_string(),
            "settings.json.20260909T144503Z.bak".to_string(),
        ],
        "the oldest snapshot must be the one pruned"
    );
}

/// Why: the prune DELETES files in a directory that also holds the atomic
/// writer's own `settings.json.bak`, its transient staging names, and whatever
/// an operator left behind — including the hand-made repair copy this repo
/// already carries. A loose pattern here destroys data no one asked it to touch.
/// What: seeds four foreign names plus four real snapshots, prunes to one, and
/// asserts every foreign name survives untouched.
#[test]
fn snapshot_then_prune_leaves_foreign_backups_alone() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("settings.json");
    let foreign = [
        "settings.json.bak",
        "settings.json.bak.4242.7",
        "settings.json.tmp.4242.7",
        "settings.local.json.20260909T144500Z.bak",
    ];
    for name in foreign {
        std::fs::write(tmp.path().join(name), name).unwrap();
    }

    for second in 0..4 {
        std::fs::write(&path, format!("state-{second}")).unwrap();
        snapshot_then_prune_at(&path, 1, at(&format!("2026-09-09T14:45:0{second}Z")))
            .unwrap()
            .unwrap();
    }

    for name in foreign {
        let kept = tmp.path().join(name);
        assert!(kept.exists(), "{name} must survive the prune");
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), name);
    }
    assert_eq!(
        snapshots(tmp.path()),
        vec!["settings.json.20260909T144503Z.bak".to_string()]
    );
}

/// Why: the predicate is what stands between the prune and unrelated files, so
/// its rejections are worth asserting directly rather than only through the
/// files a prune happens to leave behind.
/// What: table of names the prune must ignore, plus the two shapes it accepts.
#[test]
fn snapshot_order_key_rejects_foreign_names() {
    for name in [
        "settings.json",
        "settings.json.bak",
        "settings.json.bak.4242.7",
        "settings.json.tmp.4242.7",
        "settings.json.20260909T144500.bak",
        "settings.json.20260909T144500Z.txt",
        "settings.json.2026-09-09T14:45:00Z.bak",
        "settings.json.20260909T144500Z-.bak",
        "settings.json.20260909T144500Z-x.bak",
        "settings.local.json.20260909T144500Z.bak",
    ] {
        assert!(
            snapshot_order_key("settings.json", name).is_none(),
            "{name} must not be treated as a snapshot"
        );
    }

    assert_eq!(
        snapshot_order_key("settings.json", "settings.json.20260909T144500Z.bak"),
        Some(("20260909T144500Z".to_string(), 0))
    );
    assert_eq!(
        snapshot_order_key("settings.json", "settings.json.20260909T144500Z-12.bak"),
        Some(("20260909T144500Z".to_string(), 12))
    );
}

/// Why (#7244, fail-closed): the snapshot is the caller's precondition for
/// rewriting. If it cannot be taken the error must REACH the caller — a
/// swallowed failure turns "back up, then overwrite" into a plain overwrite,
/// which is the situation this whole module exists to prevent.
/// What: makes the parent directory unwritable so the exclusive create cannot
/// claim a name, then asserts an error came back, no snapshot was left behind,
/// and the source file is byte-for-byte as it was. Unix-only: the read-only
/// directory bit is the portable way to deny file creation.
#[cfg(unix)]
#[test]
fn snapshot_then_prune_errors_when_the_parent_is_read_only() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("claude");
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("settings.json");
    std::fs::write(&path, "original").unwrap();

    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = snapshot_then_prune_at(&path, 3, at("2026-09-09T14:45:00Z"));
    // Restore before asserting, so a failed assertion still leaves a removable
    // temp dir behind.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();

    let err = result.expect_err("an unwritable parent must fail the snapshot");
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "original",
        "a failed snapshot must not disturb the source file"
    );
    assert!(
        snapshots(&dir).is_empty(),
        "a failed snapshot must leave no partial file"
    );
}
