//! Reading and writing `project-map.toml` (#9169 PR-B1).
//!
//! Why: the map is persisted state that pins and parks real indexes, so a bad
//! read must never become an empty map that a later write publishes over the
//! operator's pins — the #4317/#4871 registry failure. Every writer, in every
//! process, takes the same two locks the index registry uses (#5344).
//! What: [`load`] fails closed on anything but a clean schema-1 parse or a
//! missing file; [`update`] runs one read-modify-write under the in-process
//! mutex and the cross-process `.lock` sidecar, then publishes atomically;
//! [`quarantine_and_rebuild`] moves a file aside for `project rebuild-map`.
//! Test: `core::project_map::tests` (`project_map_tests.rs`).

use std::path::{Path, PathBuf};

use super::project_map::{ProjectMap, ProjectMapError, PROJECT_MAP_FILE, PROJECT_MAP_SCHEMA};

/// Orders this process's own map writers; the `.lock` sidecar orders the rest.
static MAP_WRITE_LOCK: std::sync::LazyLock<std::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

/// The map path that sits next to `registry_path` (`indexes.toml`).
pub fn map_path_beside(registry_path: &Path) -> PathBuf {
    registry_path.with_file_name(PROJECT_MAP_FILE)
}

/// Read the map at `path`.
///
/// Why: fail closed (#9169) — the file is never re-derived behind the
/// operator's back.
/// What: `Ok(None)` only when the file does not exist. Any other read error,
/// a parse error (which covers a truncated file), and a schema other than
/// [`PROJECT_MAP_SCHEMA`] are errors naming the path and the remedy.
/// Test: `a_corrupt_truncated_or_future_map_fails_closed_and_keeps_its_bytes`,
/// `a_missing_map_reads_as_none`.
pub fn load(path: &Path) -> Result<Option<ProjectMap>, ProjectMapError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ProjectMapError::Read {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    let map: ProjectMap = toml::from_str(&text).map_err(|e| ProjectMapError::Parse {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    if map.schema != PROJECT_MAP_SCHEMA {
        return Err(ProjectMapError::Schema {
            path: path.to_path_buf(),
            found: map.schema,
        });
    }
    Ok(Some(map))
}

/// Run one read-modify-write of the map at `path` under both locks.
///
/// Why: the map has the registry's writer set — the daemon's RPC handlers and
/// the operator CLI in another process — so it needs the registry's
/// discipline: the locks span the load AND the save, or a concurrent write is
/// lost (#4871).
/// What: takes `MAP_WRITE_LOCK` (a poisoned guard is recovered: the data is
/// the file, not the guard), then the `.lock` sidecar via
/// [`trusty_common::file_lock::with_exclusive_lock`]; a lock that cannot be
/// taken is [`ProjectMapError::Lock`], never a bypass. Under them: [`load`]
/// (fail closed), `f` with the current map (`None` when absent), and — when
/// `f` returns a map — one atomic publish. `f` returning `None` writes
/// nothing.
/// Test: `concurrent_map_writers_lose_no_entries`,
/// `a_held_cross_process_lock_blocks_a_writer_until_released`.
pub fn update<T>(
    path: &Path,
    f: impl FnOnce(Option<ProjectMap>) -> Result<(Option<ProjectMap>, T), ProjectMapError>,
) -> Result<T, ProjectMapError> {
    let _guard = MAP_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let locked = trusty_common::file_lock::with_exclusive_lock(path, || {
        let (next, out) = f(load(path)?)?;
        if let Some(map) = next {
            save_unlocked(path, &map)?;
        }
        Ok(out)
    });
    locked.map_err(|source| ProjectMapError::Lock {
        path: path.to_path_buf(),
        source,
    })?
}

/// Publish `map` at `path` — the caller holds both locks.
///
/// What: serialises schema 1 and replaces the file with
/// [`trusty_common::atomic_file::write_atomic`], so a crash leaves the old
/// bytes or the new, never a mix.
fn save_unlocked(path: &Path, map: &ProjectMap) -> Result<(), ProjectMapError> {
    let write_err = |message: String| ProjectMapError::Write {
        path: path.to_path_buf(),
        message,
    };
    let body = toml::to_string_pretty(map).map_err(|e| write_err(e.to_string()))?;
    trusty_common::atomic_file::write_atomic(path, body.as_bytes())
        .map_err(|e| write_err(e.to_string()))
}

/// Move the map at `path` aside to `<path>.corrupt-<unix>` under both locks,
/// then run `rebuild` against the now-missing file.
///
/// Why: `project rebuild-map` is the operator's only way past a fail-closed
/// map, and the bad bytes are evidence, so they are kept, never deleted.
/// What: renames an existing file (`None` when there was none), then calls
/// `rebuild` with no map and publishes what it returns, all inside one
/// critical section so no writer lands between the move and the rebuild.
/// Test: `quarantine_moves_the_file_aside_and_publishes_the_rebuild`.
pub fn quarantine_and_rebuild(
    path: &Path,
    now_unix: u64,
    rebuild: impl FnOnce() -> Result<ProjectMap, ProjectMapError>,
) -> Result<(Option<PathBuf>, ProjectMap), ProjectMapError> {
    let _guard = MAP_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let locked = trusty_common::file_lock::with_exclusive_lock(path, || {
        let moved = match std::fs::symlink_metadata(path) {
            Ok(_) => {
                let mut name = path.file_name().unwrap_or_default().to_os_string();
                name.push(format!(".corrupt-{now_unix}"));
                let aside = path.with_file_name(name);
                std::fs::rename(path, &aside).map_err(|e| ProjectMapError::Write {
                    path: path.to_path_buf(),
                    message: format!("could not move it aside to {}: {e}", aside.display()),
                })?;
                Some(aside)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(ProjectMapError::Read {
                    path: path.to_path_buf(),
                    source,
                })
            }
        };
        let map = rebuild()?;
        save_unlocked(path, &map)?;
        Ok((moved, map))
    });
    locked.map_err(|source| ProjectMapError::Lock {
        path: path.to_path_buf(),
        source,
    })?
}
