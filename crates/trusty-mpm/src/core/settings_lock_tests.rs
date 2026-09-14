//! Tests for the settings-file critical section (#7762).
//!
//! Why: the two guarantees this module sells — "no second writer runs inside my
//! cycle" and "a crash before the rename costs nothing" — are both invisible in
//! a single-threaded happy path, so each has a test that fails without the
//! mechanism rather than one that merely exercises it.
//! What: lock identity, mutual exclusion, fail-closed acquisition, and the
//! staging/rename split.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use serde_json::json;
use tempfile::TempDir;

use super::*;

/// The absolute sidecar `with_settings_lock` takes for `settings_path`.
///
/// Why: the canonical identity is what the lock is actually keyed by, and three
/// tests need it. It lives here rather than in `super` because production has no
/// use for it — the entry point already holds the [`stable_path`] it derives it
/// from.
fn lock_path(settings_path: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    Ok(lock_sidecar(&stable_path(settings_path)?))
}

/// The sidecar is the settings file's own name plus `.lock`, wherever it sits.
///
/// Why (#7762): `scaffold_gitignore` writes this spelling into a managed
/// project's `.gitignore` as a relative entry, so the pure, un-canonicalised
/// form is a contract and not an implementation detail.
#[test]
fn lock_sidecar_is_named_after_the_settings_file() {
    assert_eq!(
        lock_sidecar(std::path::Path::new(".claude/settings.json")),
        std::path::PathBuf::from(".claude/settings.json.lock"),
        "a relative settings path must yield a relative sidecar"
    );
}

/// The sidecar sits beside the settings file, named after it.
#[test]
fn lock_path_is_a_sidecar_of_the_settings_file() {
    let dir = TempDir::new().expect("tempdir");
    let settings = dir.path().join(".claude").join("settings.json");

    let lock = lock_path(&settings).expect("lock path");

    assert_eq!(lock.file_name().unwrap(), "settings.json.lock");
    assert_eq!(
        lock.parent().unwrap(),
        settings.parent().unwrap().canonicalize().unwrap(),
        "the sidecar must live in the settings file's own directory"
    );
}

/// Two spellings of one directory take ONE lock, not two.
///
/// Why: this is the whole point of canonicalising in `stable_path` — a lock keyed
/// by the caller's spelling would let `tm launch` and the daemon race while both
/// believed they held it.
#[cfg(unix)]
#[test]
fn a_symlinked_directory_resolves_to_one_lock() {
    let dir = TempDir::new().expect("tempdir");
    let real = dir.path().join("real");
    std::fs::create_dir_all(&real).expect("create real dir");
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");

    let through_real = lock_path(&real.join("settings.json")).expect("lock path via real");
    let through_link = lock_path(&link.join("settings.json")).expect("lock path via link");

    assert_eq!(through_real, through_link);
}

/// The closure runs inside the lock, so two threads never overlap.
#[test]
fn serialises_concurrent_threads() {
    let dir = TempDir::new().expect("tempdir");
    let settings = Arc::new(dir.path().join("settings.json"));
    let inside = Arc::new(AtomicUsize::new(0));
    let overlapped = Arc::new(AtomicBool::new(false));

    let handles: Vec<_> = (0..4)
        .map(|_| {
            let settings = Arc::clone(&settings);
            let inside = Arc::clone(&inside);
            let overlapped = Arc::clone(&overlapped);
            std::thread::spawn(move || {
                for _ in 0..25 {
                    with_settings_lock(&settings, || {
                        if inside.fetch_add(1, Ordering::SeqCst) != 0 {
                            overlapped.store(true, Ordering::SeqCst);
                        }
                        std::thread::yield_now();
                        inside.fetch_sub(1, Ordering::SeqCst);
                    })
                    .expect("lock");
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("thread");
    }

    assert!(
        !overlapped.load(Ordering::SeqCst),
        "two holders were inside the critical section at once"
    );
}

/// An unopenable sidecar is an error and the closure never runs.
///
/// Why (#7762 fail-open check): a writer that proceeds when it could not lock is
/// the lost update this module exists to remove, dressed up as success.
#[test]
fn errors_when_the_lock_is_unopenable() {
    let dir = TempDir::new().expect("tempdir");
    let settings = dir.path().join("settings.json");
    // A DIRECTORY sitting on the sidecar's name cannot be opened for write.
    std::fs::create_dir(lock_path(&settings).expect("lock path")).expect("occupy the sidecar");
    let ran = AtomicBool::new(false);

    let result = with_settings_lock(&settings, || ran.store(true, Ordering::SeqCst));

    assert!(result.is_err(), "an unopenable sidecar must not lock");
    assert!(!ran.load(Ordering::SeqCst), "the closure must not have run");
}

/// The acquisition failure names the SIDECAR, not the settings file (#7762).
///
/// Why: every consumer's own error already names `settings.json`. The file the
/// operator must inspect — a sidecar another uid left `0644`, which this opens
/// `O_RDWR` and therefore fails on forever — appears nowhere unless this wrapper
/// puts it there.
#[test]
fn an_acquisition_failure_names_the_sidecar() {
    let dir = TempDir::new().expect("tempdir");
    let settings = dir.path().join("settings.json");
    std::fs::create_dir(lock_path(&settings).expect("lock path")).expect("occupy the sidecar");

    let err = with_settings_lock(&settings, || ()).expect_err("an unopenable sidecar must error");

    let message = err.to_string();
    assert!(
        message.contains("settings.json.lock"),
        "the message must name the sidecar: {message}"
    );
}

/// A parent directory that cannot exist is an error, not an unlocked write.
#[test]
fn errors_when_the_parent_cannot_be_created() {
    let dir = TempDir::new().expect("tempdir");
    let blocker = dir.path().join("not-a-dir");
    std::fs::write(&blocker, b"x").expect("write blocker");
    let ran = AtomicBool::new(false);

    let result = with_settings_lock(&blocker.join("settings.json"), || {
        ran.store(true, Ordering::SeqCst);
    });

    assert!(result.is_err());
    assert!(!ran.load(Ordering::SeqCst));
}

/// The publish replaces the file and leaves no `.bak` sibling (#7762).
#[test]
fn publish_replaces_the_file_and_leaves_no_bak() {
    let dir = TempDir::new().expect("tempdir");
    let settings = dir.path().join("settings.json");
    std::fs::write(&settings, br#"{"outputStyle":"old"}"#).expect("seed");

    publish(&settings, &json!({ "outputStyle": "new" })).expect("publish");

    let text = std::fs::read_to_string(&settings).expect("read back");
    assert!(text.contains("\"new\""), "{text}");
    assert!(
        !settings.with_file_name("settings.json.bak").exists(),
        "the publish must not leave a .bak sibling"
    );
}

/// A successful publish leaves no staging file behind.
#[test]
fn publish_leaves_no_staged_file_behind() {
    let dir = TempDir::new().expect("tempdir");
    let settings = dir.path().join("settings.json");

    publish(&settings, &json!({ "a": 1 })).expect("publish");

    let strays: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(strays.is_empty(), "left staging files: {strays:?}");
}

/// Stopping between the stage and the rename leaves the original untouched.
///
/// Why: this is the crash-safety half of the atomic publish. Calling `stage`
/// alone is exactly the state a process killed after the temp write is in.
#[test]
fn a_crash_between_stage_and_rename_leaves_the_original_intact() {
    let dir = TempDir::new().expect("tempdir");
    let settings = dir.path().join("settings.json");
    let original = br#"{"outputStyle":"original"}"#;
    std::fs::write(&settings, original).expect("seed");

    let staged = stage(&settings, b"{\"outputStyle\":\"interrupted\"}").expect("stage");

    assert_eq!(
        std::fs::read(&settings).expect("read original"),
        original,
        "the settings file must be byte-for-byte what it was"
    );
    assert_eq!(
        staged.parent().unwrap(),
        settings.parent().unwrap(),
        "the staged file must be a sibling so the publish is a same-filesystem rename"
    );
}

/// Two staging paths for one file never collide.
#[test]
fn staged_paths_are_unique_per_call() {
    let settings = std::path::Path::new("/tmp/settings.json");

    let first = temp_path(settings);
    let second = temp_path(settings);

    assert_ne!(first, second);
    assert_eq!(first.parent(), settings.parent());
}
