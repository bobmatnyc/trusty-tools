//! Cross-process exclusive advisory lock around a whole-file critical section.
//!
//! Why: [`crate::json_rmw`] already owned this lock, but only for JSON
//! documents it also serialises and publishes itself. `trusty-search`'s
//! `indexes.toml` is a TOML registry with its own loader, its own
//! `skip_serializing_if` shape, and its own fail-closed parse contract, so it
//! cannot route through `json_rmw::update` — yet it has the identical failure
//! mode: several independent PROCESSES (the daemon, `trusty-search prune`,
//! `trusty-search prune-orphans`) each run load → mutate → save-the-whole-file,
//! and a write landing between another writer's load and its save is discarded
//! with both callers reporting success (#5344). Extracting the lock here means
//! there is still exactly ONE implementation of the critical section; `json_rmw`
//! now calls it rather than owning it.
//! What: [`with_exclusive_lock`] runs a closure while holding an exclusive
//! `flock(2)`-style advisory lock on a `<path>.lock` sidecar, releasing it by
//! RAII on every exit path including a panic. [`lock_path`] names that sidecar.
//! Test: `cargo test -p trusty-common --features unconditional-only --
//! file_lock::tests`.
//!
//! # Contract
//!
//! - **Serialisation.** The lock is held by the open file description, so it
//!   serialises separate PROCESSES and separate threads that each call
//!   [`with_exclusive_lock`], on Unix and Windows alike.
//! - **Advisory, not mandatory.** A process that writes the guarded file
//!   without going through this entry point is not blocked. Every writer of a
//!   given file must use it.
//! - **Never fail open.** A lock that cannot be created or acquired is an
//!   `Err`; the closure never runs. Proceeding unlocked is the lost-update bug
//!   this module exists to remove.
//! - **Not reentrant.** Nesting two [`with_exclusive_lock`] calls on the same
//!   path self-deadlocks: the second acquisition uses a different descriptor.
//! - **Blocking.** Acquisition blocks the calling thread. Async callers must
//!   run it on a blocking-safe thread (e.g. `tokio::task::spawn_blocking`).
//!
//! [`with_exclusive_lock`]: crate::file_lock::with_exclusive_lock
//! [`lock_path`]: crate::file_lock::lock_path

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

/// Sidecar lock-file path for `path`.
///
/// Why: locking the guarded file itself would mean opening it for write before
/// we know whether the update will succeed, and the lock would be lost across
/// the `rename` that publishes a new version (the renamed-over inode, and any
/// lock on it, is discarded). A stable sidecar survives every publish.
/// What: appends `.lock` to the file name, keeping it in the same directory.
/// Test: `lock_path_is_a_sidecar`.
pub fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    path.with_file_name(name)
}

/// Run `f` while holding the exclusive cross-process lock guarding `path`.
///
/// Why: see the module docs — this is the one place the load → mutate → save
/// critical section is made safe against writers in other processes.
/// What: creates (if needed) and opens the [`lock_path`] sidecar, blocks until
/// the exclusive advisory lock is available, runs `f`, and releases the lock by
/// RAII. `f`'s own return value — commonly a `Result` — passes through
/// untouched; the `Err` returned here is only ever a lock-acquisition failure,
/// so a caller can never confuse "could not lock" with "the work failed".
/// Test: `with_exclusive_lock_serialises_separate_descriptors`,
/// `with_exclusive_lock_releases_on_panic`, `with_exclusive_lock_unopenable_errors`.
pub fn with_exclusive_lock<R>(path: &Path, f: impl FnOnce() -> R) -> std::io::Result<R> {
    let lock = lock_path(path);
    if let Some(parent) = lock.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock)?;
    let mut rw = fd_lock::RwLock::new(lock_file);
    // Blocking exclusive acquisition. Failure is an error, never a bypass.
    let _guard = rw.write()?;
    Ok(f())
}

#[cfg(test)]
#[path = "file_lock_tests.rs"]
mod tests;
