//! Tests for [`super::TemplateFile`] and [`super::sweep_stale_templates`]
//! (#7519): modes, removal on drop and on panic, and the startup sweep.
//!
//! Every path is under a temp dir; nothing touches `~/.trusty-tools`.
//! Test: itself.

use std::os::unix::fs::{PermissionsExt, symlink};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tempfile::TempDir;

use super::{TemplateFile, sweep_stale_templates};
use crate::api::{SecretValue, SecretsError};

const CANARY: &str = "sk-canary-7519-template-0123456789";

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777
}

/// A pid that belonged to a process now exited and reaped.
fn dead_pid() -> u32 {
    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .spawn()
        .unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

/// Why: owner ruling 2026-10-07 — the template file is 0600 in a 0700
/// directory, holds exactly the bytes given, and both are gone after drop;
/// a symlinked tmp root is refused, never followed.
/// Test: itself.
#[test]
fn template_file_is_0600_in_a_0700_dir_and_removed_on_drop() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("trusty-secrets").join("tmp");
    let guard = TemplateFile::create(&root, &SecretValue::new(CANARY)).unwrap();
    let path = guard.path().to_path_buf();
    let dir = path.parent().unwrap().to_path_buf();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), CANARY);
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&root), 0o700);
    assert!(!format!("{guard:?}").contains(CANARY));

    let second = TemplateFile::create(&root, &SecretValue::new("other")).unwrap();
    assert_ne!(second.path(), guard.path());
    drop(guard);
    assert!(!path.exists() && !dir.exists());
    assert!(root.is_dir() && second.path().exists());

    let link = tmp.path().join("root-link");
    symlink(&root, &link).unwrap();
    let err = TemplateFile::create(&link, &SecretValue::new(CANARY)).unwrap_err();
    assert!(
        matches!(err, SecretsError::StorageRefused { .. }),
        "{err:?}"
    );
}

/// Why: owner ruling 2026-10-07 — the drop guard removes the file and its
/// directory on a panic path too.
/// Test: itself.
#[test]
fn template_file_is_removed_when_its_scope_panics() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("tmp");
    let seen: Mutex<Option<PathBuf>> = Mutex::new(None);
    let result = catch_unwind(AssertUnwindSafe(|| {
        let guard = TemplateFile::create(&root, &SecretValue::new(CANARY)).unwrap();
        *seen.lock().unwrap() = Some(guard.path().to_path_buf());
        panic!("a panic inside the guard's scope");
    }));
    assert!(result.is_err());
    let path = seen.into_inner().unwrap().expect("the guard was created");
    assert!(!path.exists());
    assert!(!path.parent().unwrap().exists());
    assert!(root.is_dir());
}

/// Why: owner ruling 2026-10-07 — a crash skips the drop, so a startup
/// sweep removes a dead process's guard directories. It matches only its
/// own prefix, keeps a live process's guard, never follows a symlink, and
/// refuses a symlinked root.
/// Test: itself.
#[test]
fn template_sweep_removes_stale_dirs_and_leaves_the_rest() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("tmp");
    assert_eq!(sweep_stale_templates(&root).unwrap(), 0);
    let live = TemplateFile::create(&root, &SecretValue::new("live")).unwrap();

    let dead = dead_pid();
    let stale = root.join(format!("tpl.{dead}.1.0"));
    std::fs::create_dir(&stale).unwrap();
    std::fs::write(stale.join("template.json"), CANARY).unwrap();
    let stale_empty = root.join(format!("tpl.{dead}.2.0"));
    std::fs::create_dir(&stale_empty).unwrap();
    let crowded = root.join(format!("tpl.{dead}.3.0"));
    std::fs::create_dir(&crowded).unwrap();
    std::fs::write(crowded.join("extra.txt"), "keep").unwrap();

    let prefixed_file = root.join(format!("tpl.{dead}.4.0"));
    std::fs::write(&prefixed_file, "a file, not a guard dir").unwrap();
    std::fs::write(root.join("notes.txt"), "keep").unwrap();
    std::fs::create_dir(root.join("other")).unwrap();
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("template.json"), "keep").unwrap();
    let linked = root.join(format!("tpl.{dead}.5.0"));
    symlink(&outside, &linked).unwrap();

    assert_eq!(sweep_stale_templates(&root).unwrap(), 2);
    assert!(!stale.exists() && !stale_empty.exists());
    assert!(live.path().exists());
    assert!(crowded.join("extra.txt").exists());
    assert!(prefixed_file.is_file());
    assert!(root.join("notes.txt").is_file() && root.join("other").is_dir());
    assert!(outside.join("template.json").is_file());
    assert!(std::fs::symlink_metadata(&linked).is_ok());

    let link = tmp.path().join("root-link");
    symlink(&root, &link).unwrap();
    let err = sweep_stale_templates(&link).unwrap_err();
    assert!(
        matches!(err, SecretsError::StorageRefused { .. }),
        "{err:?}"
    );
}

/// Why: #7519 — one entry the sweep cannot remove must not leave the stale
/// directories after it on disk; the failure is still reported.
/// What: the bad entries' `template.json` is a directory, which
/// `remove_file` refuses for root too. A bad entry is made first and last,
/// so the sweep meets one before the stale ones under creation-order and
/// reverse-order directory listings alike.
/// Test: itself.
#[test]
fn template_sweep_goes_past_a_bad_entry_and_reports_it() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("tmp");
    drop(TemplateFile::create(&root, &SecretValue::new("x")).unwrap());
    let dead = dead_pid();
    let bad = |n: u32| {
        let dir = root.join(format!("tpl.{dead}.{n}.0"));
        std::fs::create_dir_all(dir.join("template.json")).unwrap();
        std::fs::write(dir.join("template.json").join("inner"), CANARY).unwrap();
        dir
    };
    let first_bad = bad(0);
    let stale: Vec<PathBuf> = (1..=6)
        .map(|n| {
            let dir = root.join(format!("tpl.{dead}.{n}.0"));
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("template.json"), CANARY).unwrap();
            dir
        })
        .collect();
    let last_bad = bad(7);

    let err = sweep_stale_templates(&root).unwrap_err();
    match &err {
        SecretsError::Io { path, .. } => assert!(path.ends_with("template.json"), "{path:?}"),
        other => panic!("expected Io, got {other:?}"),
    }
    assert!(!format!("{err} {err:?}").contains(CANARY));
    for dir in &stale {
        assert!(!dir.exists(), "{} survived the bad entry", dir.display());
    }
    assert!(first_bad.is_dir() && last_bad.is_dir());
}
