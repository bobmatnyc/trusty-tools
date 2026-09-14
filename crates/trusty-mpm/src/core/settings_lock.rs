//! Cross-process serialisation and `.bak`-free atomic publish for a Claude Code
//! `settings*.json`.
//!
//! Why (#7762): a dozen writers across this crate run the same
//! read → merge → write-the-whole-file cycle over one `settings.json`, and they
//! do not all run in one process. `tm launch` prepares a session from the CLI
//! process while the daemon prepares one from its own, and `tm doctor --fix`
//! repairs the very same file from a third. Two of those cycles overlapping is a
//! lost update: the slower writer republishes the snapshot it read BEFORE the
//! faster writer's publish, so the faster writer's keys silently disappear with
//! both callers reporting success. [`crate::core::claude_json_guard`] cannot
//! close that — it is a process-wide `Mutex`, and the racing writers are
//! separate processes.
//!
//! What: [`with_settings_lock`] runs a writer's WHOLE cycle under the
//! `flock(2)`-style advisory lock `trusty_common::file_lock` already owns, and
//! [`publish`] replaces the file by `rename` from a per-call temp sibling.
//! Together they are the only settings-file critical section in this crate;
//! every writer of a `settings.json` or `settings.local.json` goes through them.
//!
//! # Contract
//!
//! - **Serialisation.** The lock is held by the open file description, so it
//!   serialises separate processes and separate threads alike. A writer holds it
//!   across the read AND the write, never just the write.
//! - **Never fail open.** A lock that cannot be created or acquired is an error
//!   and the closure never runs, per `trusty_common::file_lock`'s own rule.
//!   Proceeding unlocked is the bug this module removes.
//! - **Not reentrant.** Two nested [`with_settings_lock`] calls on one path
//!   self-deadlock; the inner acquisition uses a different descriptor. No writer
//!   in this crate calls another writer of the same file.
//! - **One identity per file.** The lock sidecar is named from the CANONICAL
//!   parent directory, so two processes that spell one file differently — a
//!   symlinked project root, macOS's `/var` → `/private/var` — still take the
//!   same lock.
//! - **No `.bak`.** `trusty_common::claude_config::write_json_atomic` publishes a
//!   `<path>.bak` sibling on every call, which for a once-per-launch writer is
//!   untracked litter in every managed project (#7762). [`publish`] is that
//!   function's staging-and-rename half without the copy; writers that REPORT a
//!   backup to the operator keep their own.
//!
//! Test: `settings_lock_tests.rs`,
//! `core::session_launch::tests_settings_lock_7762`.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

/// Run `f` while holding the cross-process lock guarding `settings_path`.
///
/// Why: see the module doc — this is the one place a settings-file
/// read → merge → write cycle is made safe against a writer in another process.
/// What: resolves `settings_path` to its [`stable_path`] identity, then delegates
/// to `trusty_common::file_lock::with_exclusive_lock`, which blocks until the
/// lock is free and releases it by RAII on every exit path including a panic.
/// `f`'s own return value passes through untouched, so the `Err` returned here is
/// only ever an acquisition failure — a caller can never confuse "could not lock"
/// with "the work failed".
/// Test: `serialises_concurrent_threads`, `errors_when_the_lock_is_unopenable`,
/// `errors_when_the_parent_cannot_be_created`.
pub(crate) fn with_settings_lock<R>(settings_path: &Path, f: impl FnOnce() -> R) -> io::Result<R> {
    let stable = stable_path(settings_path)?;
    trusty_common::file_lock::with_exclusive_lock(&stable, f)
}

/// The sidecar path [`with_settings_lock`] locks for `settings_path`.
///
/// Why: the tests, and any future reader wondering which file appeared beside
/// their settings, need the spelling named once rather than re-derived.
/// What: `trusty_common::file_lock::lock_path` of the [`stable_path`] identity —
/// `<canonical dir>/settings.json.lock`. Errors for the same reasons
/// [`stable_path`] does.
/// Test: `lock_path_is_a_sidecar_of_the_settings_file`.
#[cfg(test)]
pub(crate) fn lock_path(settings_path: &Path) -> io::Result<PathBuf> {
    Ok(trusty_common::file_lock::lock_path(&stable_path(
        settings_path,
    )?))
}

/// Publish `value` over `settings_path` atomically, leaving no `.bak`.
///
/// Why (#7762): `rename(2)` within a directory is atomic, so a reader — or a
/// writer in another process that is next in line for the lock — sees the whole
/// previous file or the whole new one, never a splice, and a crash between the
/// temp write and the rename leaves the original byte-for-byte intact. The
/// `.bak` half of `write_json_atomic` is deliberately absent: this runs on every
/// launch, and a copy taken on every launch is untracked litter in the
/// operator's project, not a recovery artifact.
/// What: serialises `value` pretty-printed (byte-compatible with
/// `write_json_atomic`, no trailing newline), [`stage`]s it beside
/// `settings_path`, renames it into place, and fsyncs the parent directory so
/// the rename itself is durable. Any failure removes the staged file and returns
/// `Err` with `settings_path` untouched.
/// Test: `publish_replaces_the_file_and_leaves_no_bak`,
/// `publish_leaves_no_staged_file_behind`,
/// `a_crash_between_stage_and_rename_leaves_the_original_intact`.
pub(crate) fn publish(settings_path: &Path, value: &Value) -> io::Result<()> {
    let serialized = serde_json::to_string_pretty(value).map_err(io::Error::other)?;
    let staged = stage(settings_path, serialized.as_bytes())?;

    if let Err(err) = std::fs::rename(&staged, settings_path) {
        let _ = std::fs::remove_file(&staged);
        return Err(err);
    }

    // Durability of the rename itself. Unix-only: Windows exposes no directory
    // handle to sync, and its rename is already committed to the log.
    #[cfg(unix)]
    if let Ok(dir) = std::fs::File::open(parent_of(settings_path)) {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// Write `bytes` to a fresh temp sibling of `settings_path` and sync it.
///
/// Why: this is the half of [`publish`] that must be observable on its own — the
/// guarantee that a crash before the rename costs nothing is only testable if a
/// test can stop here. It is also where the per-call name matters: a shared
/// `<path>.tmp` let two writers fill ONE file and rename the splice over the real
/// settings, which is the durable corruption #4077 removed from
/// `write_json_atomic` and must not come back here.
/// What: creates the parent directory, fills `<name>.<pid>.<seq>.tmp` beside
/// `settings_path` (same directory, so the publish is a same-filesystem rename),
/// and `fsync`s it before returning its path. A failed fill removes the partial
/// file rather than leaving litter.
/// Test: `a_crash_between_stage_and_rename_leaves_the_original_intact`,
/// `staged_paths_are_unique_per_call`.
fn stage(settings_path: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    use std::io::Write as _;

    std::fs::create_dir_all(parent_of(settings_path))?;
    let staged = temp_path(settings_path);
    let fill = (|| -> io::Result<()> {
        let mut file = std::fs::File::create(&staged)?;
        file.write_all(bytes)?;
        // Durability of the CONTENT must precede the rename that publishes it.
        file.sync_all()
    })();
    if let Err(err) = fill {
        let _ = std::fs::remove_file(&staged);
        return Err(err);
    }
    Ok(staged)
}

/// A staging path no other live writer can hold.
///
/// What: `<file name>.<pid>.<counter>.tmp`, in `settings_path`'s own directory.
/// The pid separates processes and the counter separates calls within one
/// process; unlike a clock reading it cannot repeat under a coarse timer.
/// Test: `staged_paths_are_unique_per_call`.
fn temp_path(settings_path: &Path) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let mut name = settings_path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    settings_path.with_file_name(name)
}

/// The directory `settings_path` lives in, as a path that can be created.
///
/// What: the parent, or `.` for a bare file name.
fn parent_of(settings_path: &Path) -> &Path {
    match settings_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

/// The one spelling of `settings_path` every process agrees on.
///
/// Why: a lock keyed by the caller's spelling is not a lock. `tm launch` reaches
/// a project through the path the operator typed while the daemon reaches it
/// through a registry entry, and on macOS a temp or `/var` path differs from its
/// `/private/var` resolution — three spellings of one file would take three
/// different sidecars and race anyway. Canonicalising the DIRECTORY (not the
/// file) is what makes them converge while still working for a settings file that
/// does not exist yet.
/// What: creates the parent directory when absent, canonicalises it, and rejoins
/// the file name. Propagates the create or canonicalise failure — never falls
/// back to the raw path, because a fallback is exactly the silent unlocked write
/// this module exists to prevent.
/// Test: `lock_path_is_a_sidecar_of_the_settings_file`,
/// `a_symlinked_directory_resolves_to_one_lock`,
/// `errors_when_the_parent_cannot_be_created`.
fn stable_path(settings_path: &Path) -> io::Result<PathBuf> {
    let file_name = settings_path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} has no file name to lock", settings_path.display()),
        )
    })?;
    let parent = parent_of(settings_path);
    std::fs::create_dir_all(parent)?;
    Ok(parent.canonicalize()?.join(file_name))
}

#[cfg(test)]
#[path = "settings_lock_tests.rs"]
mod tests;
