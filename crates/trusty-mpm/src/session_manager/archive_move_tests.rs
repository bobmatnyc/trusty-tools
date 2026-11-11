//! Tests for the archive move (#10991): root mode, rename, cross-filesystem
//! copy + verify, and every failure arm's rollback.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use chrono::Utc;

use super::*;

/// A project tree with a nested file, an executable, and a symlink.
fn project(base: &Path) -> PathBuf {
    let src = base.join("widget");
    fs::create_dir_all(src.join("src")).expect("mkdir");
    fs::write(src.join("README.md"), "hello").expect("write");
    fs::write(src.join("src/main.rs"), "fn main() {}").expect("write");
    fs::write(src.join("run.sh"), "#!/bin/sh").expect("write");
    fs::set_permissions(src.join("run.sh"), fs::Permissions::from_mode(0o755)).expect("chmod");
    std::os::unix::fs::symlink("README.md", src.join("link")).expect("symlink");
    src
}

fn mode(p: &Path) -> u32 {
    fs::metadata(p).expect("stat").permissions().mode() & 0o777
}

fn plan_for(base: &Path) -> ArchivePlan {
    let src = project(base);
    plan_archive(
        &src,
        &base.join("archive"),
        "0123456789abcdef",
        Some("widget".into()),
        Utc::now(),
    )
    .expect("plan")
}

/// Owner ruling D8b: the archive root is created mode 0700.
#[test]
fn archive_root_is_created_owner_only() {
    let base = tempfile::tempdir().expect("tempdir");
    let root = base.path().join("deep/archive");
    ensure_archive_root(&root).expect("root");
    assert_eq!(mode(&root), ARCHIVE_DIR_MODE);
}

#[test]
fn an_existing_loose_archive_root_is_tightened() {
    let base = tempfile::tempdir().expect("tempdir");
    let root = base.path().join("archive");
    fs::create_dir(&root).expect("mkdir");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).expect("chmod");
    ensure_archive_root(&root).expect("root");
    assert_eq!(mode(&root), ARCHIVE_DIR_MODE);
}

#[test]
fn a_symlinked_archive_root_is_refused() {
    let base = tempfile::tempdir().expect("tempdir");
    let real = base.path().join("real");
    fs::create_dir(&real).expect("mkdir");
    let root = base.path().join("archive");
    std::os::unix::fs::symlink(&real, &root).expect("symlink");
    assert!(matches!(
        ensure_archive_root(&root),
        Err(ArchiveMoveError::Refused { .. })
    ));
}

#[test]
fn plan_refuses_home_and_the_archive_itself() {
    let base = tempfile::tempdir().expect("tempdir");
    let root = base.path().join("archive");
    let inside = root.join("old");
    fs::create_dir_all(&inside).expect("mkdir");
    let refused = |src: &Path, root: &Path| {
        matches!(
            plan_archive(src, root, "id", None, Utc::now()),
            Err(ArchiveMoveError::Refused { .. })
        )
    };
    assert!(refused(&inside, &root), "a source inside the archive");
    assert!(
        refused(base.path(), &root),
        "a source containing the archive"
    );
    assert!(refused(Path::new("/"), &root), "the filesystem root");
    if let Some(home) = dirs::home_dir() {
        assert!(refused(&home, &root), "the home directory");
    }
}

/// A not-yet-created root reached through a symlink into the source is still
/// "inside the source": the check canonicalizes the nearest existing ancestor.
#[test]
fn plan_refuses_a_missing_root_symlinked_into_the_source() {
    let base = tempfile::tempdir().expect("tempdir");
    let src = project(base.path());
    let link = base.path().join("link");
    std::os::unix::fs::symlink(&src, &link).expect("symlink");
    let err = plan_archive(&src, &link.join("archive"), "id", None, Utc::now())
        .expect_err("the root would land inside the moved tree");
    assert!(
        matches!(&err, ArchiveMoveError::Refused { reason, .. } if reason.contains("inside it")),
        "{err}"
    );
}

#[test]
fn plan_refuses_a_missing_source() {
    let base = tempfile::tempdir().expect("tempdir");
    let err = plan_archive(
        &base.path().join("gone"),
        &base.path().join("archive"),
        "id",
        None,
        Utc::now(),
    )
    .expect_err("missing");
    assert!(matches!(err, ArchiveMoveError::NotADirectory(_)), "{err}");
}

/// Closure 1: the directory moves and a manifest records its origin. Fails if
/// the manifest is missing.
#[test]
fn rename_moves_the_directory_and_writes_the_manifest() {
    let base = tempfile::tempdir().expect("tempdir");
    let plan = plan_for(base.path());
    let manifest = execute_plan(&plan).expect("archive");

    assert!(!plan.source.exists(), "the source is still there");
    assert_eq!(
        fs::read_to_string(plan.destination.join("README.md")).expect("moved"),
        "hello"
    );
    assert_eq!(manifest.method, MoveMethod::Rename);
    let on_disk = read_manifest(&plan.entry).expect("the manifest must exist");
    assert_eq!(on_disk, manifest);
    assert_eq!(on_disk.origin_path, plan.source);
    assert_eq!(on_disk.archived_path, plan.destination);
    assert_eq!(on_disk.project.as_deref(), Some("widget"));
    assert_eq!(mode(&plan.root), ARCHIVE_DIR_MODE);
    assert_eq!(mode(&plan.entry), ARCHIVE_DIR_MODE);
}

/// Closure 1, cross-filesystem arm: the copy verifies, then the source goes.
#[test]
fn copy_moves_the_directory_and_verifies_it() {
    let base = tempfile::tempdir().expect("tempdir");
    let plan = plan_for(base.path());
    let manifest = execute_with(&plan, Strategy::ForceCopy, &Hooks::NONE).expect("archive");

    assert_eq!(manifest.method, MoveMethod::CopyVerified);
    assert!(!plan.source.exists(), "the source is still there");
    assert_eq!(mode(&plan.destination.join("run.sh")), 0o755);
    assert_eq!(
        fs::read_link(plan.destination.join("link")).expect("symlink kept"),
        PathBuf::from("README.md")
    );
    assert_eq!(read_manifest(&plan.entry).expect("manifest"), manifest);
}

/// Closure 1: a copy that does not verify fails, removes the copy, and keeps
/// the source.
#[test]
fn a_copy_that_does_not_verify_keeps_the_source() {
    let base = tempfile::tempdir().expect("tempdir");
    let plan = plan_for(base.path());
    let corrupt = |dst: &Path| fs::write(dst.join("README.md"), "HELLO").expect("corrupt");
    let hooks = Hooks {
        after_copy: &corrupt,
        before_manifest: &|_| {},
    };
    let err = execute_with(&plan, Strategy::ForceCopy, &hooks).expect_err("must not verify");

    assert!(
        matches!(&err, ArchiveMoveError::VerifyFailed { reason, .. } if reason.contains("README.md")),
        "{err}"
    );
    assert!(
        plan.source.join("README.md").is_file(),
        "the source was lost"
    );
    assert!(!plan.entry.exists(), "the bad copy was left behind");
}

#[test]
fn verify_copy_catches_an_extra_file() {
    let base = tempfile::tempdir().expect("tempdir");
    let src = project(base.path());
    let dst = base.path().join("copy");
    copy_tree(&src, &dst).expect("copy");
    verify_copy(&src, &dst).expect("identical");
    fs::write(dst.join("extra"), "x").expect("write");
    assert!(verify_copy(&src, &dst).is_err());
}

/// Fail-open check: a manifest that cannot be written undoes the rename.
#[test]
fn a_failed_manifest_write_undoes_a_rename() {
    let base = tempfile::tempdir().expect("tempdir");
    let plan = plan_for(base.path());
    // A directory at the manifest path makes the final rename fail.
    let block = |p: &Path| fs::create_dir(p).expect("block");
    let hooks = Hooks {
        after_copy: &|_| {},
        before_manifest: &block,
    };
    let err = execute_with(&plan, Strategy::Auto, &hooks).expect_err("must fail");

    assert!(matches!(err, ArchiveMoveError::Manifest { .. }), "{err}");
    assert!(plan.source.join("README.md").is_file(), "not moved back");
    assert!(!plan.entry.exists(), "the entry was left behind");
}

#[test]
fn a_failed_manifest_write_removes_a_copy() {
    let base = tempfile::tempdir().expect("tempdir");
    let plan = plan_for(base.path());
    let block = |p: &Path| fs::create_dir(p).expect("block");
    let hooks = Hooks {
        after_copy: &|_| {},
        before_manifest: &block,
    };
    let err = execute_with(&plan, Strategy::ForceCopy, &hooks).expect_err("must fail");

    assert!(matches!(err, ArchiveMoveError::Manifest { .. }), "{err}");
    assert!(
        plan.source.join("README.md").is_file(),
        "the source was lost"
    );
    assert!(!plan.entry.exists(), "the copy was left behind");
}

/// A verified copy whose source cannot be removed is an error, not a success.
#[test]
fn a_source_that_cannot_be_removed_is_reported() {
    let base = tempfile::tempdir().expect("tempdir");
    let plan = plan_for(base.path());
    let src_dir = plan.source.join("src");
    let lock = |_: &Path| {
        fs::set_permissions(&src_dir, fs::Permissions::from_mode(0o500)).expect("lock");
    };
    let hooks = Hooks {
        after_copy: &lock,
        before_manifest: &|_| {},
    };
    let result = execute_with(&plan, Strategy::ForceCopy, &hooks);
    fs::set_permissions(&src_dir, fs::Permissions::from_mode(0o755)).expect("unlock");

    let err = result.expect_err("must report the kept source");
    assert!(matches!(err, ArchiveMoveError::SourceKept { .. }), "{err}");
    assert!(
        read_manifest(&plan.entry).is_ok(),
        "the archive is complete"
    );
}

#[test]
fn an_existing_entry_is_never_overwritten() {
    let base = tempfile::tempdir().expect("tempdir");
    let plan = plan_for(base.path());
    fs::create_dir_all(&plan.entry).expect("squat");
    let err = execute_plan(&plan).expect_err("must refuse");
    assert!(matches!(err, ArchiveMoveError::EntryExists(_)), "{err}");
    assert!(plan.source.is_dir(), "the source moved");
}
