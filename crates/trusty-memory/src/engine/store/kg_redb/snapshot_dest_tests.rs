//! Tests for where a #9282 palace backup may be written.
//!
//! Why: a backup name is user input from an MCP client. Any name that lands
//! outside the backup root can create `<data root>/palaces`, which switches
//! the daemon's palace registry on the next start, or put a `palace.json` in
//! the registry dir, where the lister takes it for a palace.
//! What: the name rules as a pure table; then every refusal and failure
//! branch of the root and destination walk through `snapshot_palace`, each
//! asserting that the trees it could have touched are unchanged.
//! Test: this file is the test.

#![cfg(test)]

use super::dest::manual_backup_root;
use super::tests::{PALACE, fixture, listing};
use super::{SnapshotError, SnapshotStep, check_backup_name, no_fault, snapshot_palace};
use crate::engine::palace::{Palace, PalaceId};
use crate::engine::retrieval::{PalaceHandle, seed_shared_embedder_with_mock};
use crate::engine::store::palace_store::PalaceStore;
use chrono::Utc;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;

/// A name with a NUL byte, built at run time so the source stays plain text.
fn nul_name() -> String {
    format!("a{}b", char::from(0u8))
}

/// #9282: a backup name is a relative path of plain segments, checked on the
/// raw bytes (`Path::components` drops an interior `.` and a trailing `/`).
#[test]
fn backup_name_rules_accept_plain_segments_only() {
    for ok in ["bk", "team/weekly/bk", "palace\u{17f}/bk", "a.b/..c/c.."] {
        check_backup_name(Path::new(ok)).unwrap_or_else(|e| panic!("{ok:?} refused: {e}"));
    }
    let nul = nul_name();
    let mut refused = vec![
        "",
        "/abs/bk",
        "..",
        ".",
        "palaces/../l/bk",
        "Palaces/../l/bk",
        "a/..",
        "./a",
        "a/./b",
        "a//b",
        "a/",
        nul.as_str(),
    ];
    if cfg!(windows) {
        refused.extend(["C:x", r"\\?\x", r"\abs", r"a\..\b"]);
    }
    for bad in refused {
        let err = check_backup_name(Path::new(bad)).expect_err(bad);
        assert!(
            matches!(&err, SnapshotError::InvalidBackupName { name, .. } if name == Path::new(bad)),
            "{bad:?}: {err}"
        );
    }
}

/// Every path under `dir`, relative and sorted.
#[cfg(unix)]
fn tree(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).expect("list") {
            let p = e.expect("entry").path();
            if p.is_dir() && !p.is_symlink() {
                todo.push(p.clone());
            }
            out.push(p.strip_prefix(dir).expect("prefix").to_path_buf());
        }
    }
    out.sort();
    out
}

/// A small flat-layout palace: `<root>/flat-p`, so the registry is `<root>`
/// itself and no `<root>/palaces` exists. Returns the canonical root.
#[cfg(unix)]
fn flat_palace() -> (TempDir, PathBuf, Arc<PalaceHandle>) {
    seed_shared_embedder_with_mock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    let palace = Palace {
        id: PalaceId::new("flat-p"),
        name: "flat-p".into(),
        description: None,
        created_at: Utc::now(),
        data_dir: root.join("flat-p"),
    };
    PalaceStore::save_palace(&palace).expect("palace.json");
    let handle = PalaceHandle::open(&palace).expect("open palace");
    (tmp, root, handle)
}

/// The flat-layout backup root under a canonical registry `root`.
#[cfg(unix)]
fn flat_manual(root: &Path) -> PathBuf {
    root.join(".trusty-memory").join("backups").join("manual")
}

/// Ids of the palaces the lister finds in `registry`.
#[cfg(unix)]
fn palace_ids(registry: &Path) -> Vec<String> {
    let mut ids: Vec<String> = PalaceStore::list_palaces(registry)
        .expect("list palaces")
        .into_iter()
        .map(|p| p.id.as_str().to_string())
        .collect();
    ids.sort();
    ids
}

/// #9282 (`palaceſ` hole): `palaceſ` case-folds to `palaces` on APFS. Under
/// the backup root it is a plain name, so the fold is harmless: no
/// `<root>/palaces` appears and the lister sees the same palaces.
#[cfg(unix)]
#[test]
fn a_long_s_name_lands_under_the_backup_root() {
    let (_tmp, root, handle) = flat_palace();
    let before = palace_ids(&root);
    let snap = snapshot_palace(&handle, None, Some(Path::new("palace\u{17f}/bk")), no_fault)
        .expect("a plain long-s name is accepted");
    let manual = flat_manual(&root);
    assert_eq!(snap.dir, manual.join("palace\u{17f}").join("bk"));
    assert!(snap.dir.join("manifest.json").is_file());
    assert!(!root.join("palaces").exists(), "<root>/palaces was created");
    assert_eq!(palace_ids(&root), before, "the registry changed");
    eprintln!(
        "fold probe: palace\u{17f} aliases palaces on this filesystem: {}",
        manual.join("palaces").exists()
    );
}

/// #9282 (ruling A): an absolute `dest` is refused before any write, even
/// one inside the backup root.
#[cfg(unix)]
#[test]
fn an_absolute_dest_is_refused_and_writes_nothing() {
    let (_tmp, root, handle) = flat_palace();
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    let (before_root, before_else) = (tree(&root), tree(elsewhere.path()));
    for dest in [
        elsewhere.path().join("bk"),
        root.join("palace\u{17f}").join("bk"),
        root.join("palaces").join("..").join("l").join("bk"),
        flat_manual(&root).join("inside"),
    ] {
        let err = snapshot_palace(&handle, None, Some(&dest), no_fault).expect_err("absolute");
        assert!(
            matches!(&err, SnapshotError::InvalidBackupName { .. }),
            "{}: {err}",
            dest.display()
        );
        assert_eq!(tree(&root), before_root, "{} wrote", dest.display());
        assert_eq!(
            tree(elsewhere.path()),
            before_else,
            "{} wrote",
            dest.display()
        );
    }
}

/// #9282 (was the registry-dir refusal): in the legacy layout, absolute
/// destinations in the registry, through a symlink to it, and a relative
/// `missing/../bk-dotdot` are all refused as names; nothing is written.
#[cfg(unix)]
#[test]
fn a_destination_in_the_registry_is_refused_and_writes_nothing() {
    let fx = fixture();
    let registry = fx.root.join("palaces");
    let link = fx.root.join("registry-link");
    std::os::unix::fs::symlink(&registry, &link).expect("symlink");
    let before = tree(&fx.root);
    for dest in [
        registry.join("bk"),
        link.join("bk-via-link"),
        PathBuf::from("missing/../bk-dotdot"),
    ] {
        let err = snapshot_palace(&fx.handle, Some(&fx.chat), Some(&dest), no_fault)
            .expect_err("refused");
        assert!(
            matches!(&err, SnapshotError::InvalidBackupName { .. }),
            "{}: {err}",
            dest.display()
        );
        assert_eq!(tree(&fx.root), before, "{} wrote something", dest.display());
    }
}

/// #9282 (was the flat `palaces/` refusal): absolute `<root>/palaces/bk`,
/// `<root>/Palaces/bk` and relative `palaces/../l/bk`, `Palaces/../l/bk` are
/// refused; relative `palaces/bk` is a plain name and lands under the root.
/// No `<root>/palaces` is created in any row.
#[cfg(unix)]
#[test]
fn a_flat_layout_dest_under_palaces_is_refused_and_creates_nothing() {
    let (_tmp, root, handle) = flat_palace();
    let before = tree(&root);
    for dest in [
        root.join("palaces").join("bk"),
        root.join("Palaces").join("bk"),
        PathBuf::from("palaces/../l/bk"),
        PathBuf::from("Palaces/../l/bk"),
    ] {
        let err = snapshot_palace(&handle, None, Some(&dest), no_fault).expect_err("refused");
        assert!(
            matches!(&err, SnapshotError::InvalidBackupName { .. }),
            "{}: {err}",
            dest.display()
        );
        assert_eq!(tree(&root), before, "{} wrote something", dest.display());
    }
    let snap = snapshot_palace(&handle, None, Some(Path::new("palaces/bk")), no_fault)
        .expect("a plain name");
    assert_eq!(snap.dir, flat_manual(&root).join("palaces").join("bk"));
    assert!(!root.join("palaces").exists() && !root.join("Palaces").exists());
    assert!(!root.join("l").exists());
}

/// #9282: a symlinked segment below the backup root is refused, never
/// followed; the link's target stays empty.
#[cfg(unix)]
#[test]
fn a_symlinked_component_is_refused_and_writes_nothing() {
    let (_tmp, root, handle) = flat_palace();
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    let manual = flat_manual(&root);
    std::fs::create_dir_all(&manual).expect("root");
    std::os::unix::fs::symlink(elsewhere.path(), manual.join("out")).expect("link");
    let before = tree(&root);
    let err = snapshot_palace(&handle, None, Some(Path::new("out/bk")), no_fault)
        .expect_err("symlinked component");
    assert!(
        matches!(&err, SnapshotError::UnsafeBackupComponent { path, .. } if *path == manual.join("out")),
        "{err}"
    );
    assert_eq!(tree(&root), before);
    assert!(
        listing(elsewhere.path()).is_empty(),
        "the link was followed"
    );
}

/// #9282 (was the stack-overflow crash): `evil -> missing/../evil` in the
/// backup root is a symlinked component: refused, no crash, no `missing`.
#[cfg(unix)]
#[test]
fn a_self_referencing_dangling_symlink_dest_is_an_error_not_a_crash() {
    let (_tmp, root, handle) = flat_palace();
    let manual = flat_manual(&root);
    std::fs::create_dir_all(&manual).expect("root");
    std::os::unix::fs::symlink(
        Path::new("missing").join("..").join("evil"),
        manual.join("evil"),
    )
    .expect("symlink");
    let before = tree(&root);
    let err = snapshot_palace(&handle, None, Some(Path::new("evil/bk")), no_fault)
        .expect_err("a destination through a dangling symlink must fail");
    assert!(
        matches!(&err, SnapshotError::UnsafeBackupComponent { .. }),
        "{err}"
    );
    assert_eq!(tree(&root), before, "the data root changed");
    assert!(!manual.join("missing").exists() && !root.join("palaces").exists());
}

/// #9282: a fixed backup-root segment that is a symlink (live or dangling) is
/// refused with the default dest; nothing lands at the link target, no
/// `palaces` appears, and the registry lists the same palaces.
#[cfg(unix)]
#[test]
fn a_symlinked_backup_root_segment_is_refused() {
    for dangling in [false, true] {
        let (_tmp, root, handle) = flat_palace();
        let elsewhere = tempfile::tempdir().expect("elsewhere");
        let target = if dangling {
            PathBuf::from("palaces")
        } else {
            elsewhere.path().to_path_buf()
        };
        std::os::unix::fs::symlink(&target, root.join(".trusty-memory")).expect("link");
        let (before, ids) = (tree(&root), palace_ids(&root));
        let err = snapshot_palace(&handle, None, None, no_fault).expect_err("symlinked root");
        assert!(
            matches!(&err, SnapshotError::BackupRootUnsafe { path, .. } if *path == root.join(".trusty-memory")),
            "dangling={dangling}: {err}"
        );
        assert_eq!(tree(&root), before, "dangling={dangling}");
        assert_eq!(palace_ids(&root), ids);
        assert!(!root.join("palaces").exists(), "dangling={dangling}");
        assert!(listing(elsewhere.path()).is_empty(), "dangling={dangling}");
    }
    // Legacy: `<data>/backups -> <data>/palaces`, the registry itself.
    let fx = fixture();
    let data = fx.root.canonicalize().expect("canonical");
    std::os::unix::fs::symlink(data.join("palaces"), data.join("backups")).expect("link");
    let registry_before = listing(&data.join("palaces"));
    let err = snapshot_palace(&fx.handle, Some(&fx.chat), None, no_fault).expect_err("legacy");
    assert!(
        matches!(&err, SnapshotError::BackupRootUnsafe { path, .. } if *path == data.join("backups")),
        "{err}"
    );
    assert_eq!(listing(&data.join("palaces")), registry_before);
}

/// #9282: the default dest is `<palace>/<UTC>` under `backups/manual` in
/// both layouts.
#[cfg(unix)]
#[test]
fn the_default_dest_is_palace_slash_utc_under_manual() {
    let (_tmp, root, handle) = flat_palace();
    let snap = snapshot_palace(&handle, None, None, no_fault).expect("flat default");
    assert_eq!(
        snap.dir.parent(),
        Some(flat_manual(&root).join("flat-p").as_path())
    );
    let fx = fixture();
    let snap = snapshot_palace(&fx.handle, Some(&fx.chat), None, no_fault).expect("legacy");
    assert_eq!(
        snap.dir.parent(),
        Some(fx.manual_root().join(PALACE).as_path())
    );
    let stamp = snap.dir.file_name().and_then(|n| n.to_str()).expect("name");
    assert!(stamp.ends_with('Z') && stamp.contains('T'), "{stamp}");
}

/// #9282: a nested plain name creates its parents under the root.
#[cfg(unix)]
#[test]
fn a_valid_nested_name_lands_under_the_backup_root() {
    let (_tmp, root, handle) = flat_palace();
    let snap = snapshot_palace(&handle, None, Some(Path::new("team/weekly/bk")), no_fault)
        .expect("nested name");
    let want = flat_manual(&root).join("team").join("weekly").join("bk");
    assert_eq!(snap.dir, want);
    assert!(want.join("manifest.json").is_file());
    assert!(!flat_manual(&root).join("team/weekly/bk.tmp").exists());
}

/// Fail-closed check (#9282): a fixed root segment that is a file is refused.
#[cfg(unix)]
#[test]
fn a_backup_root_segment_that_is_a_file_is_refused() {
    let (_tmp, root, handle) = flat_palace();
    std::fs::write(root.join(".trusty-memory"), b"not a dir").expect("file");
    let before = tree(&root);
    let err = snapshot_palace(&handle, None, None, no_fault).expect_err("file in root");
    assert!(
        matches!(&err, SnapshotError::BackupRootUnsafe { .. }),
        "{err}"
    );
    assert_eq!(tree(&root), before);
    assert_eq!(
        std::fs::read(root.join(".trusty-memory")).expect("read"),
        b"not a dir"
    );
}

/// Restores a directory's mode on drop, so the tempdir can be removed.
#[cfg(unix)]
struct ModeGuard(PathBuf, u32);

#[cfg(unix)]
impl Drop for ModeGuard {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(self.1));
    }
}

/// True when running as root, which ignores mode 000.
#[cfg(unix)]
fn is_root() -> bool {
    // SAFETY: `geteuid` has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

/// Fail-closed check (#9282, was the unresolvable-prefix row): a root
/// segment that cannot be stat'ed is `Io { op: "stat" }`, never a pass.
/// What: `R/.trusty-memory` at mode 000, so `lstat` of `backups` fails.
#[cfg(unix)]
#[test]
fn an_unreadable_backup_root_segment_is_an_error_and_writes_nothing() {
    use std::os::unix::fs::PermissionsExt;
    if is_root() {
        eprintln!("skipped: root ignores mode 000");
        return;
    }
    let (_tmp, root, handle) = flat_palace();
    let locked = root.join(".trusty-memory");
    std::fs::create_dir(&locked).expect("dir");
    let err = {
        let _restore = ModeGuard(locked.clone(), 0o755);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        snapshot_palace(&handle, None, None, no_fault).expect_err("unreadable root")
    };
    assert!(
        matches!(&err, SnapshotError::Io { op: "stat", path, .. } if *path == locked.join("backups")),
        "{err}"
    );
    assert!(listing(&locked).is_empty(), "nothing under the locked dir");
}

/// Fail-closed check (#9282): a palace directory with no parent has no
/// registry, so there is no backup root; an error, never a pass.
#[test]
fn a_palace_dir_without_a_parent_is_refused() {
    let err = manual_backup_root(Path::new("/")).expect_err("no parent");
    assert!(
        matches!(&err, SnapshotError::Io { op: "resolve", .. }),
        "{err}"
    );
}

/// Fail-closed check (#9282): a registry that cannot be canonicalised is an
/// error. What: `<tmp>/locked/reg/p` with `locked` at mode 000; nothing is
/// created under `reg`.
#[cfg(unix)]
#[test]
fn an_uncanonicalisable_registry_is_an_error() {
    use std::os::unix::fs::PermissionsExt;
    if is_root() {
        eprintln!("skipped: root ignores mode 000");
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let locked = tmp.path().join("locked");
    let reg = locked.join("reg");
    std::fs::create_dir_all(reg.join("p")).expect("dirs");
    let err = {
        let _restore = ModeGuard(locked.clone(), 0o755);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        manual_backup_root(&reg.join("p")).expect_err("registry cannot be resolved")
    };
    assert!(
        matches!(&err, SnapshotError::Io { op: "resolve", .. }),
        "{err}"
    );
    assert_eq!(listing(&reg), vec!["p".to_string()]);
}

/// Fail-closed check (#9282): a name segment that is a file is refused.
#[cfg(unix)]
#[test]
fn a_name_segment_that_is_a_file_is_refused() {
    let (_tmp, root, handle) = flat_palace();
    let manual = flat_manual(&root);
    std::fs::create_dir_all(&manual).expect("root");
    std::fs::write(manual.join("f"), b"file").expect("file");
    let before = tree(&root);
    let err = snapshot_palace(&handle, None, Some(Path::new("f/bk")), no_fault)
        .expect_err("file segment");
    assert!(
        matches!(&err, SnapshotError::UnsafeBackupComponent { path, .. } if *path == manual.join("f")),
        "{err}"
    );
    assert_eq!(tree(&root), before);
}

/// Fails the create of the second segment of `team/weekly/bk`.
#[cfg(unix)]
fn fail_second_create(step: SnapshotStep, path: &Path) -> io::Result<()> {
    if step == SnapshotStep::CreateDest && path.ends_with("weekly") {
        Err(io::Error::other("injected fault"))
    } else {
        Ok(())
    }
}

/// Fail-closed check (#9282): a parent create failing mid-walk is an `Io`
/// error, and the parents this call created are removed.
#[cfg(unix)]
#[test]
fn a_failed_parent_create_removes_what_it_created() {
    let (_tmp, root, handle) = flat_palace();
    let manual = flat_manual(&root);
    std::fs::create_dir_all(&manual).expect("root");
    let err = snapshot_palace(
        &handle,
        None,
        Some(Path::new("team/weekly/bk")),
        fail_second_create,
    )
    .expect_err("injected");
    assert!(
        matches!(&err, SnapshotError::Io { op: "create", path, .. } if path.ends_with("team/weekly")),
        "{err}"
    );
    assert!(
        listing(&manual).is_empty(),
        "created parents were left behind"
    );
}

/// Swaps `race-a` for a symlink to `../swapped` just before `race-b` is
/// created: the race the canonical containment check exists to catch.
#[cfg(unix)]
fn swap_first_segment(step: SnapshotStep, path: &Path) -> io::Result<()> {
    if step == SnapshotStep::CreateDest && path.ends_with("race-b") {
        let a = path.parent().ok_or_else(|| io::Error::other("no parent"))?;
        let backups = a
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| io::Error::other("no backups dir"))?;
        std::fs::rename(a, backups.join("swapped"))?;
        std::os::unix::fs::symlink(Path::new("..").join("swapped"), a)?;
    }
    Ok(())
}

/// Fail-closed check (#9282, ruling D): a segment swapped for a symlink
/// after the walk makes the create land outside the root; the canonical
/// recheck refuses it and removes the stray directory.
#[cfg(unix)]
#[test]
fn a_segment_swapped_after_the_walk_is_refused() {
    let (_tmp, root, handle) = flat_palace();
    let manual = flat_manual(&root);
    std::fs::create_dir_all(&manual).expect("root");
    let err = snapshot_palace(
        &handle,
        None,
        Some(Path::new("race-a/race-b/bk")),
        swap_first_segment,
    )
    .expect_err("escaped the root");
    assert!(
        matches!(&err, SnapshotError::UnsafeBackupComponent { .. }),
        "{err}"
    );
    let swapped = manual.parent().expect("backups").join("swapped");
    assert!(
        listing(&swapped).is_empty(),
        "the stray create was left behind"
    );
    assert!(!root.join("palaces").exists());
}
