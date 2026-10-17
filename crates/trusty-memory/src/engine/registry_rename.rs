//! [`PalaceRegistry::rename_palace`]: move a palace to a new id (#9544).
//!
//! Why: a palace id is its directory name, the id inside `palace.json`, and the
//! key of every registry cache. Renaming one by hand leaves the old id dead for
//! every caller still using it and races any open of either id. This is the
//! one primitive that does the move safely; trusty-memory's service, MCP and
//! CLI surfaces call it.
//! What: [`PalaceRegistry::rename_palace`] and its option, outcome and error
//! types. It lives beside `registry.rs` (as a child module) because it must
//! hold the registry's private per-palace open-locks.
//! Test: `registry_rename_tests.rs`.
// #9278: only the server half renames a palace, so the engine-only build
// (`--no-default-features`) compiles this module with no caller.
#![cfg_attr(not(feature = "server"), allow(dead_code))]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use super::PalaceRegistry;
use crate::engine::palace::PalaceId;
use crate::engine::palace_emptiness::{
    LegacyProbe, PalaceNotEmpty, check_palace_empty, conservative_legacy_probe,
};
use crate::engine::store::kg_redb::KgStoreRedb;
use crate::engine::store::palace_format::{KG_FILE, mirror_to_preserve};
use crate::engine::store::palace_store::PalaceStore;
use crate::engine::store::vector::vector_db_is_open;
use trusty_common::palace_alias::{
    AliasChange, AliasUndo, PalaceAliasStore, try_alias_target_if_absent,
};

/// Default bound on waiting for an open-lock or for a cached handle to go idle.
pub(crate) const DEFAULT_RENAME_BUSY_WAIT: Duration = Duration::from_secs(2);

/// Gap between release attempts while a handle is still referenced.
const RELEASE_POLL: Duration = Duration::from_millis(20);

/// Directory under the data root that holds replaced rename targets.
const TRASH_DIR: &str = ".trash";

/// Caller choices for [`PalaceRegistry::rename_palace`].
///
/// What: `replace_empty` lets an existing, empty target be moved to the trash;
/// `legacy_probe` decides whether a target's legacy data makes it non-empty
/// (default: [`conservative_legacy_probe`]); `busy_wait` bounds every wait.
pub(crate) struct RenameOptions<'a> {
    /// Replace an existing target that is empty (moved to `<root>/.trash/`).
    pub(crate) replace_empty: bool,
    /// Legacy-data check for the target; see [`LegacyProbe`].
    pub(crate) legacy_probe: &'a LegacyProbe,
    /// Bound on each open-lock wait and on the release poll.
    pub(crate) busy_wait: Duration,
}

impl Default for RenameOptions<'static> {
    fn default() -> Self {
        Self {
            replace_empty: false,
            legacy_probe: &conservative_legacy_probe,
            busy_wait: DEFAULT_RENAME_BUSY_WAIT,
        }
    }
}

/// What a completed [`PalaceRegistry::rename_palace`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RenameOutcome {
    /// The id the palace had.
    pub(crate) old: PalaceId,
    /// The id the palace has now.
    pub(crate) new: PalaceId,
    /// Where an empty target was moved, when `replace_empty` replaced one.
    pub(crate) trashed_target: Option<PathBuf>,
    /// `true` when this call finished a rename an earlier call left partway.
    pub(crate) resumed: bool,
    /// The alias keys this call changed (`old -> new` and every retarget).
    pub(crate) alias_changes: Vec<AliasChange>,
    /// `true` when `palace.json`'s display name was `old` and became `new`.
    pub(crate) name_rewritten: bool,
}

/// Why a [`PalaceRegistry::rename_palace`] did not complete.
///
/// Why: callers map these to different answers — not found, conflict, retry
/// later, server fault — so each class is its own variant.
/// What: `NotFound`; the conflict class (`SourceIsAlias`, `InvalidTarget`,
/// `TargetIsAlias`, `TargetExists`, `TargetNotEmpty`); `Busy`; `Io`. See
/// `Self::is_conflict` (test-only, so no doc link).
/// Test: `rename_palace_refuses_a_missing_source`,
/// `rename_palace_refuses_an_alias_source`,
/// `rename_palace_refuses_a_nonempty_target`,
/// `rename_palace_refuses_a_referenced_handle_and_leaves_state_unchanged`,
/// `rename_palace_failed_move_rolls_back_alias_keys`.
// #9278 T3a: every struct variant is `non_exhaustive` too.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PalaceRenameError {
    /// No palace has the source id.
    #[error("palace {0:?} not found")]
    NotFound(String),
    /// The source id is an alias; rename the palace it points at instead.
    #[error("palace id {alias:?} is an alias of palace {target:?}; rename {target:?} instead")]
    #[non_exhaustive]
    SourceIsAlias {
        /// The requested source id.
        alias: String,
        /// The palace it resolves to.
        target: String,
    },
    /// The new id is the old one or is not a valid palace id.
    #[error("cannot rename to {new:?}: {reason}")]
    #[non_exhaustive]
    InvalidTarget {
        /// The requested new id.
        new: String,
        /// Why it was refused.
        reason: String,
    },
    /// The new id is a live alias of a palace other than the source.
    #[error("{new:?} is an alias of palace {target:?}; remove that alias first")]
    #[non_exhaustive]
    TargetIsAlias {
        /// The requested new id.
        new: String,
        /// The palace it resolves to.
        target: String,
    },
    /// The new id is an empty palace and `replace_empty` was not set.
    #[error("palace {new:?} already exists (it is empty; pass replace_empty to replace it)")]
    #[non_exhaustive]
    TargetExists {
        /// The requested new id.
        new: String,
    },
    /// The new id is a palace that is not empty.
    #[error("palace {new:?} already exists and is not empty: {reason}")]
    #[non_exhaustive]
    TargetNotEmpty {
        /// The requested new id.
        new: String,
        /// Why it is not empty.
        #[source]
        reason: PalaceNotEmpty,
    },
    /// A lock or a cached handle of either id stayed held past the wait bound.
    #[error("palace {palace:?} is busy ({detail}); retry when it is idle")]
    #[non_exhaustive]
    Busy {
        /// The id that stayed busy.
        palace: String,
        /// What was held.
        detail: String,
    },
    /// A filesystem or alias-file step failed.
    #[error("{context}: {source}")]
    #[non_exhaustive]
    Io {
        /// The step that failed, and what was rolled back.
        context: String,
        /// The underlying error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
}

impl PalaceRenameError {
    /// Whether this refusal is a conflict with existing state (HTTP 409 class).
    #[cfg(test)]
    pub(crate) fn is_conflict(&self) -> bool {
        matches!(
            self,
            Self::SourceIsAlias { .. }
                | Self::InvalidTarget { .. }
                | Self::TargetIsAlias { .. }
                | Self::TargetExists { .. }
                | Self::TargetNotEmpty { .. }
        )
    }

    fn io(
        context: impl Into<String>,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self::Io {
            context: context.into(),
            source: source.into(),
        }
    }
}

type RenameResult<T> = Result<T, PalaceRenameError>;

/// Moves one directory to another path; [`std::fs::rename`] in production.
type Mover<'a> = &'a dyn Fn(&Path, &Path) -> std::io::Result<()>;

impl PalaceRegistry {
    /// Rename palace `old` to `new` under `data_root`.
    ///
    /// Why (#9544): see the module docs. The old id must keep answering, so the
    /// rename leaves `old -> new` in the alias map; every alias of `old` follows.
    /// What: resolves strictly — an unreadable alias file is an error, never
    /// "no alias". Refuses `new == old` or an invalid `new`, a missing `old`
    /// (`NotFound`), an `old` that is an alias, a `new` that is a live alias of
    /// a palace other than `old` (`new` aliasing `old` — a reversed rename — is
    /// allowed), and an existing `new` that is not empty, or that is empty
    /// without `replace_empty`, and a source in a newer on-disk format (`Io`,
    /// before any write). Takes the open-locks of the ids `old` and `new`
    /// resolve to, deduplicated and in sorted order, then releases the cached
    /// handles of those ids and of the literal `old` and `new` if nothing else
    /// references them, and waits for their redb files to close; a lock,
    /// handle or open file still held after `busy_wait` is
    /// `Busy` with nothing changed. Then, in order: the alias write
    /// ([`PalaceAliasStore::rename_target_with_undo`]); for `replace_empty`, the empty
    /// target moves to `<root>/.trash/<new>-replaced-<UTC>/`; the directory
    /// move `<root>/<old>` -> `<root>/<new>`; and `palace.json` is rewritten
    /// (id and data dir; the name only when it was `old`). A failed trash or
    /// move puts the trash back and undoes the alias keys it wrote.
    ///
    /// Resume: re-running the same rename after a crash finishes it. With the
    /// alias written and `<root>/<old>` still present it does the move; with
    /// `<root>/<old>` gone and `<root>/<new>/palace.json` still naming `old` it
    /// rewrites the id.
    ///
    /// Blocking and synchronous (file locks, directory moves, a palace open);
    /// async callers run it on `spawn_blocking`. It touches only this
    /// registry's caches: a caller holding other per-palace state (write
    /// locks, chat-session handles) drops it first.
    /// Test: `rename_palace_moves_dir_and_rewrites_palace_json`,
    /// `rename_palace_refuses_a_nonempty_target`,
    /// `rename_palace_refuses_an_alias_source`,
    /// `rename_palace_refuses_a_missing_source`,
    /// `rename_palace_replace_empty_trashes_the_target`,
    /// `rename_palace_refuses_a_referenced_handle_and_leaves_state_unchanged`,
    /// `rename_palace_resumes_after_alias_written_but_dir_unmoved`,
    /// `rename_palace_resumes_after_dir_moved_but_id_unwritten`,
    /// `rename_palace_failed_move_rolls_back_alias_keys`,
    /// `rename_palace_refuses_a_newer_format_source_and_changes_nothing`,
    /// `rename_palace_allows_the_reverse_rename`,
    /// `rename_palace_refuses_a_target_with_chat_sessions`,
    /// `rename_palace_waits_for_a_dropped_handle_to_close_its_files`,
    /// `rename_palace_replace_empty_waits_for_the_targets_dropped_writer`,
    /// `rename_palace_refuses_while_a_detached_vector_job_holds_the_database`.
    pub(crate) fn rename_palace(
        &self,
        data_root: &Path,
        old: &str,
        new: &str,
        opts: &RenameOptions<'_>,
    ) -> RenameResult<RenameOutcome> {
        self.rename_palace_with(data_root, old, new, opts, &|from, to| {
            std::fs::rename(from, to)
        })
    }

    /// [`Self::rename_palace`] with the directory mover injected, so a test
    /// can fail the move after the alias write.
    pub(crate) fn rename_palace_with(
        &self,
        data_root: &Path,
        old: &str,
        new: &str,
        opts: &RenameOptions<'_>,
        mover: Mover<'_>,
    ) -> RenameResult<RenameOutcome> {
        let (old, new) = (old.trim(), new.trim());
        if old == new {
            return Err(invalid(new, "it is the palace's current id"));
        }
        if !trusty_common::palace_id_is_valid(new) {
            return Err(invalid(
                new,
                "not a valid palace id ([a-z0-9][a-z0-9-]{0,62})",
            ));
        }
        // #9544: `open_palace` locks the id a request resolves to, so holding
        // the resolved ids of `old` and `new` makes a concurrent open of either
        // wait. Sorted and deduplicated: a reversed rename (or a resumed one)
        // resolves both names to one palace, and taking that mutex twice would
        // wait on ourselves.
        let mut keys = vec![canonical(data_root, old)?, canonical(data_root, new)?];
        keys.sort_unstable();
        keys.dedup();
        let mutexes: Vec<Arc<Mutex<()>>> = keys
            .iter()
            .map(|k| {
                self.open_locks
                    .entry(PalaceId::new(k.clone()))
                    .or_insert_with(|| Arc::new(Mutex::new(())))
                    .clone()
            })
            .collect();
        let mut guards = Vec::with_capacity(mutexes.len());
        for (key, mutex) in keys.iter().zip(&mutexes) {
            guards.push(
                mutex
                    .try_lock_for(opts.busy_wait)
                    .ok_or_else(|| busy(key, "its open-lock is held"))?,
            );
        }
        // #9544: the handle cache keys by the id in `palace.json`, not the
        // directory. Half-done (dir at `new`, json id still `old`), a handle
        // of that palace sits under `old`, which no longer resolves to a lock
        // key; release the literal ids too.
        let mut cached = keys.clone();
        cached.extend([old.to_string(), new.to_string()]);
        cached.sort_unstable();
        cached.dedup();
        self.release_all(&cached, opts.busy_wait)?;
        // #9945: a released handle's KG writer task drops its store only when
        // a runtime worker next polls it; a reopen racing it gets a snapshot.
        await_files_closed(data_root, &cached, opts.busy_wait)?;
        let outcome = self.rename_locked(data_root, old, new, opts, mover)?;
        // #9544: drop per-id caches the old id no longer owns.
        let old_id = PalaceId::new(old);
        self.gaps_cache.remove(&old_id);
        self.unopenable.remove(&old_id);
        Ok(outcome)
    }

    /// Release every cached handle of `keys`, polling until `wait` expires.
    ///
    /// Never pops a referenced handle ([`Self::release_if_unreferenced`]); a
    /// handle still referenced at the deadline is `Busy`.
    fn release_all(&self, keys: &[String], wait: Duration) -> RenameResult<()> {
        let deadline = Instant::now() + wait;
        for key in keys {
            let id = PalaceId::new(key.clone());
            while self.handles.lock().contains(&id) && !self.release_if_unreferenced(&id) {
                if Instant::now() >= deadline {
                    return Err(busy(key, "an open handle is still in use"));
                }
                std::thread::sleep(RELEASE_POLL);
            }
        }
        Ok(())
    }

    /// The rename itself, run with both open-locks held and no cached handle.
    fn rename_locked(
        &self,
        data_root: &Path,
        old: &str,
        new: &str,
        opts: &RenameOptions<'_>,
        mover: Mover<'_>,
    ) -> RenameResult<RenameOutcome> {
        let (old_dir, new_dir) = (data_root.join(old), data_root.join(new));
        let aliased_to_new = PalaceAliasStore::load_aliases(data_root)
            .map_err(|e| PalaceRenameError::io("read the palace alias map", e))?
            .get(old)
            .is_some_and(|t| t == new);
        let mut outcome = RenameOutcome {
            old: PalaceId::new(old),
            new: PalaceId::new(new),
            trashed_target: None,
            resumed: false,
            alias_changes: Vec::new(),
            name_rewritten: false,
        };
        if !present(&old_dir)? {
            // #9544 resume: moved, but `palace.json` still names `old`.
            if aliased_to_new && present(&new_dir)? && palace_id_at(&new_dir)? == old {
                outcome.resumed = true;
                outcome.name_rewritten = rewrite_palace_json(&new_dir, old, new)?;
                return Ok(outcome);
            }
            return Err(match strict_alias(data_root, old)? {
                Some(target) => PalaceRenameError::SourceIsAlias {
                    alias: old.to_string(),
                    target,
                },
                None => PalaceRenameError::NotFound(old.to_string()),
            });
        }
        outcome.resumed = aliased_to_new;
        // #9544: `save_palace` refuses a newer-format palace, so check before
        // anything moves; otherwise the rename sticks half-done.
        mirror_to_preserve(&old_dir, old)
            .map_err(|e| PalaceRenameError::io(format!("rename palace {old:?}"), e))?;
        let replace = if present(&new_dir)? {
            self.check_target(&new_dir, new, opts)?;
            // #9945: the check opened and dropped the target; its KG writer
            // keeps `kg.redb` cached until polled. Moving the source onto the
            // same path first would serve the trashed target's store as `new`.
            // Still before the alias write, so a timeout changes nothing.
            await_files_closed(data_root, &[new.to_string()], opts.busy_wait)?;
            true
        } else {
            match strict_alias(data_root, new)? {
                Some(target) if target != old => {
                    return Err(PalaceRenameError::TargetIsAlias {
                        new: new.to_string(),
                        target,
                    });
                }
                _ => false,
            }
        };

        let undo = PalaceAliasStore::rename_target_with_undo(data_root, old, new)
            .map_err(|e| PalaceRenameError::io("write the palace alias map", e))?;
        if replace {
            let trash = trash_path(data_root, new);
            let moved = trash
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| mover(&new_dir, &trash));
            if let Err(e) = moved {
                let rolled = rollback(data_root, &undo, None, mover);
                return Err(PalaceRenameError::io(
                    format!(
                        "move empty target {} to the trash; {rolled}",
                        new_dir.display()
                    ),
                    e,
                ));
            }
            outcome.trashed_target = Some(trash);
        }
        if let Err(e) = mover(&old_dir, &new_dir) {
            let restore = outcome
                .trashed_target
                .as_deref()
                .map(|t| (t, new_dir.as_path()));
            let rolled = rollback(data_root, &undo, restore, mover);
            return Err(PalaceRenameError::io(
                format!(
                    "move {} to {}; {rolled}",
                    old_dir.display(),
                    new_dir.display()
                ),
                e,
            ));
        }
        outcome.alias_changes = undo.changes;
        outcome.name_rewritten = rewrite_palace_json(&new_dir, old, new)?;
        Ok(outcome)
    }

    /// Refuse an existing target unless it is empty and `replace_empty` is set.
    fn check_target(
        &self,
        new_dir: &Path,
        new: &str,
        opts: &RenameOptions<'_>,
    ) -> RenameResult<()> {
        let not_empty = |reason| PalaceRenameError::TargetNotEmpty {
            new: new.to_string(),
            reason,
        };
        let palace = PalaceStore::load_palace(new_dir)
            .map_err(|e| not_empty(PalaceNotEmpty::Unconfirmed(format!("{e:#}"))))?;
        let handle = self
            .open_handle(&palace)
            .map_err(|e| not_empty(PalaceNotEmpty::Unconfirmed(format!("{e:#}"))))?;
        let verdict = check_palace_empty(&handle, new_dir, opts.legacy_probe);
        drop(handle);
        verdict.map_err(not_empty)?;
        if !opts.replace_empty {
            return Err(PalaceRenameError::TargetExists {
                new: new.to_string(),
            });
        }
        Ok(())
    }
}

/// `InvalidTarget` for `new`.
fn invalid(new: &str, reason: &str) -> PalaceRenameError {
    PalaceRenameError::InvalidTarget {
        new: new.to_string(),
        reason: reason.to_string(),
    }
}

/// Wait until no store in this process holds a redb file of a palace in `keys`.
///
/// Why (#9945): dropping the last handle aborts its KG writer task, but the
/// task's store clone — and with it redb's lock on `kg.redb` — is dropped only
/// when a runtime worker next polls the task. Moving the directory before then
/// leaves the file open under its old path, and the next open of the new id
/// gets `DatabaseAlreadyOpen` and falls back to a read-only snapshot.
/// What: polls the KG cache for `<root>/<key>/kg.redb` and the live vector
/// `Database` of `<root>/<key>/index.usearch.redb` (alive while any clone of
/// its store is, even in a detached blocking job); a file still open after
/// `wait` is `Busy`, with nothing changed. Also run on the `replace_empty`
/// target after `check_target` opened it.
/// Test: `rename_palace_waits_for_a_dropped_handle_to_close_its_files`,
/// `rename_palace_replace_empty_waits_for_the_targets_dropped_writer`,
/// `rename_palace_refuses_while_a_detached_vector_job_holds_the_database`.
fn await_files_closed(data_root: &Path, keys: &[String], wait: Duration) -> RenameResult<()> {
    let deadline = Instant::now() + wait;
    for key in keys {
        let dir = data_root.join(key);
        let (kg, vectors) = (dir.join(KG_FILE), dir.join("index.usearch"));
        while KgStoreRedb::is_cached(&kg) || vector_db_is_open(&vectors) {
            if Instant::now() >= deadline {
                return Err(busy(key, "its store files are still open in this process"));
            }
            std::thread::sleep(RELEASE_POLL);
        }
    }
    Ok(())
}

/// `Busy` for `palace`.
fn busy(palace: &str, detail: &str) -> PalaceRenameError {
    PalaceRenameError::Busy {
        palace: palace.to_string(),
        detail: detail.to_string(),
    }
}

/// The live alias target of `id`, strictly: a read error is an error.
fn strict_alias(data_root: &Path, id: &str) -> RenameResult<Option<String>> {
    try_alias_target_if_absent(data_root, id)
        .map_err(|e| PalaceRenameError::io(format!("resolve palace id {id:?}"), e))
}

/// The id a request for `id` reaches, strictly.
fn canonical(data_root: &Path, id: &str) -> RenameResult<String> {
    Ok(strict_alias(data_root, id)?.unwrap_or_else(|| id.to_string()))
}

/// Whether `dir/palace.json` exists; a probe that cannot tell is an error.
fn present(dir: &Path) -> RenameResult<bool> {
    PalaceStore::metadata_present(dir)
        .map_err(|e| PalaceRenameError::io(format!("probe {}", dir.display()), e))
}

/// The id recorded in `dir/palace.json`.
fn palace_id_at(dir: &Path) -> RenameResult<String> {
    PalaceStore::load_palace(dir)
        .map(|p| p.id.0)
        .map_err(|e| PalaceRenameError::io(format!("read {}/palace.json", dir.display()), e))
}

/// Point `new_dir/palace.json` at `new`; returns whether the name changed too.
///
/// A failure here leaves the move done; re-running the rename finishes it.
fn rewrite_palace_json(new_dir: &Path, old: &str, new: &str) -> RenameResult<bool> {
    let fail = |e| {
        PalaceRenameError::io(
            format!(
                "rewrite {}/palace.json (re-run the rename to finish it)",
                new_dir.display()
            ),
            e,
        )
    };
    let mut palace = PalaceStore::load_palace(new_dir).map_err(fail)?;
    let name_rewritten = palace.name == old;
    palace.id = PalaceId::new(new);
    if name_rewritten {
        palace.name = new.to_string();
    }
    palace.data_dir = new_dir.to_path_buf();
    PalaceStore::save_palace(&palace).map_err(fail)?;
    Ok(name_rewritten)
}

/// `<root>/.trash/<new>-replaced-<UTC>`, suffixed `-N` if that exists.
///
/// Never `-reclaim`: the reclaim purge deletes those after 7 days.
fn trash_path(data_root: &Path, new: &str) -> PathBuf {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    let base = data_root
        .join(TRASH_DIR)
        .join(format!("{new}-replaced-{stamp}"));
    let mut candidate = base.clone();
    let mut n = 1u32;
    while !matches!(candidate.try_exists(), Ok(false)) && n < 1000 {
        candidate = PathBuf::from(format!("{}-{n}", base.display()));
        n += 1;
    }
    candidate
}

/// Undo a partial rename: restore a trashed target, then the alias keys.
/// Returns a sentence naming what was and was not rolled back.
fn rollback(
    data_root: &Path,
    undo: &AliasUndo,
    restore: Option<(&Path, &Path)>,
    mover: Mover<'_>,
) -> String {
    let mut parts = Vec::new();
    if let Some((trash, target)) = restore {
        match mover(trash, target) {
            Ok(()) => parts.push("the replaced target was restored".to_string()),
            Err(e) => parts.push(format!(
                "the replaced target stays at {} ({e})",
                trash.display()
            )),
        }
    }
    match PalaceAliasStore::undo(data_root, undo) {
        Ok(skipped) if skipped.is_empty() => {
            parts.push("the alias keys were rolled back".to_string())
        }
        Ok(skipped) => parts.push(format!(
            "the alias keys were rolled back except {skipped:?}, which another writer changed"
        )),
        Err(e) => parts.push(format!("the alias rollback failed ({e:#})")),
    }
    parts.join("; ")
}

#[cfg(test)]
#[path = "registry_rename_tests.rs"]
mod tests;
