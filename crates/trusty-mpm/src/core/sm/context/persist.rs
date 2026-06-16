//! Atomic persistence for the SM conversation state file (DOC-14 §7.4).
//!
//! Why: §7.4 requires the live conversation (`compressed_context` + verbatim
//! `recent_rounds` + counters) to persist to a daemon state file at
//! `~/.trusty-mpm/sm/conversation-<conv_id>.json` so a conversation survives a
//! daemon restart intact (the connection-safe restart convention, CLAUDE.md
//! #534). The write must be ATOMIC — a crash mid-write must never leave a
//! truncated, unparseable state file — so we write to a temp file in the same
//! directory and rename it over the target (rename is atomic on the same
//! filesystem). The storage ROOT is injectable so tests use a `tempdir` instead
//! of touching the real home directory, mirroring SM-4's pattern.
//! What: [`ConversationStore`] owns a root directory and maps a `conv_id` to its
//! JSON path; [`ConversationStore::save`] serialises an [`SmConversation`] via
//! write-tmp-then-rename, and [`ConversationStore::load`] reconstructs it. A
//! missing file loads as a fresh conversation (so first-touch is not an error).
//! Errors are a structured `thiserror` enum (library convention).
//! Test: `persist_tests.rs` covers round-trip identity, atomic overwrite, the
//! missing-file → fresh path, conv-id filename sanitisation, and a malformed-file
//! error.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::model::SmConversation;

/// Process-wide monotonic counter that uniquifies temp-file names per `save`.
///
/// Why: two `save` calls for the SAME `conv_id` in the same process (and within
/// the same nanosecond, or on a coarse clock) would otherwise generate identical
/// temp paths and race — one write could clobber the other's temp file mid-flight
/// and produce a torn read before either rename. A monotonic counter combined
/// with the wall-clock nanos guarantees a distinct temp path for every call.
/// What: incremented once per `save`; the value is mixed into the temp filename.
/// Test: `concurrent_saves_use_distinct_temp_paths` (distinct paths) and
/// `concurrent_saves_same_conv_id_do_not_corrupt` (no torn file).
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Subdirectory under the data root that holds SM conversation state files.
///
/// Why: §7.4 nests state files under `~/.trusty-mpm/sm/`; isolating the segment
/// keeps it consistent with the SM memory subtree and easy to change.
/// What: the `"sm"` path segment joined under the injected root.
/// Test: `store_path_is_under_sm_subdir`.
pub const SM_STATE_SUBDIR: &str = "sm";

/// Structured errors for conversation persistence (library → `thiserror`).
///
/// Why: SM-5 is library code; per workspace convention I/O and (de)serialisation
/// failures surface as a typed, matchable error rather than `unwrap()`/`panic!`.
/// A failed save must not crash the daemon — the caller logs and proceeds.
/// What: distinguishes filesystem failures (`Io`) from JSON (de)serialisation
/// failures (`Serde`), each tagged with the `conv_id` for actionable logs.
/// Test: `load_rejects_malformed_file` asserts the `Serde` variant; happy-path
/// tests assert `Ok`.
#[derive(Debug, thiserror::Error)]
pub enum ConversationStoreError {
    /// A filesystem operation (mkdir / write / rename / read) failed.
    #[error("conversation '{conv_id}' state-file I/O failed: {source}")]
    Io {
        /// The conversation id the operation targeted.
        conv_id: String,
        /// The underlying filesystem error.
        source: std::io::Error,
    },

    /// Serialising or deserialising the conversation JSON failed.
    #[error("conversation '{conv_id}' state-file (de)serialisation failed: {source}")]
    Serde {
        /// The conversation id the operation targeted.
        conv_id: String,
        /// The underlying serde_json error.
        source: serde_json::Error,
    },
}

/// Result alias for conversation-store operations.
pub type ConversationStoreResult<T> = std::result::Result<T, ConversationStoreError>;

/// Atomic, root-injectable store for SM conversation state files (§7.4).
///
/// Why: centralises the path layout and the write-tmp-then-rename atomicity in
/// one tested type, and makes the storage root a constructor argument so tests
/// never write to `~/.trusty-mpm`. The daemon (SM-7) builds it once from the real
/// data dir; tests build it from a `tempdir`.
/// What: holds the `<root>/sm/` directory under which each conversation persists
/// as `conversation-<conv_id>.json`. The `conv_id` is sanitised into a safe
/// filename so an adversarial id can't escape the directory.
/// Test: `persist_tests.rs`.
#[derive(Debug, Clone)]
pub struct ConversationStore {
    /// `<data_root>/sm/` — the directory holding all conversation state files.
    dir: PathBuf,
}

impl ConversationStore {
    /// Build a store rooted at `data_root` (the SM data directory).
    ///
    /// Why: the daemon passes the real `~/.trusty-mpm` while tests pass a
    /// `tempdir`; the store derives its `sm/` subdir from whichever root it is
    /// given, so the same code path is exercised in both.
    /// What: joins [`SM_STATE_SUBDIR`] under `data_root` and stores it. The
    /// directory is created lazily on the first `save`, not here, so merely
    /// constructing a store performs no I/O.
    /// Test: `store_path_is_under_sm_subdir`, `save_creates_sm_subdir`.
    pub fn new(data_root: impl Into<PathBuf>) -> Self {
        Self {
            dir: data_root.into().join(SM_STATE_SUBDIR),
        }
    }

    /// The on-disk path for a given conversation id.
    ///
    /// Why: callers (and tests) need to assert/locate the exact state file; the
    /// path also drives `save`/`load`. Filename derivation is centralised here so
    /// sanitisation is applied uniformly.
    /// What: returns `<root>/sm/conversation-<sanitised-conv_id>.json`.
    /// Test: `store_path_is_under_sm_subdir`, `conv_id_is_sanitised`.
    pub fn path_for(&self, conv_id: &str) -> PathBuf {
        self.dir
            .join(format!("conversation-{}.json", sanitise_conv_id(conv_id)))
    }

    /// Atomically persist a conversation to its state file (§7.4).
    ///
    /// Why: a daemon crash mid-write must never corrupt the state file; an atomic
    /// rename guarantees readers see either the old or the new content, never a
    /// partial write. This is the contract the connection-safe restart relies on.
    /// What: ensures the `sm/` directory exists, serialises `conv` to pretty JSON,
    /// writes it to a sibling temp file whose name is unique per call
    /// (`<target>.tmp.<pid>.<nanos>.<counter>`), flushes+closes it, then renames
    /// the temp file over the target. The per-call uniquifier means two concurrent
    /// saves for the same `conv_id` use different temp files and cannot corrupt
    /// each other. All failures map to a typed [`ConversationStoreError`].
    /// Test: `save_then_load_round_trips`, `save_is_atomic_overwrite`,
    /// `save_creates_sm_subdir`, `concurrent_saves_use_distinct_temp_paths`,
    /// `concurrent_saves_same_conv_id_do_not_corrupt`.
    pub fn save(&self, conv_id: &str, conv: &SmConversation) -> ConversationStoreResult<()> {
        let io_err = |source: std::io::Error| ConversationStoreError::Io {
            conv_id: conv_id.to_string(),
            source,
        };

        std::fs::create_dir_all(&self.dir).map_err(io_err)?;

        let json =
            serde_json::to_vec_pretty(conv).map_err(|source| ConversationStoreError::Serde {
                conv_id: conv_id.to_string(),
                source,
            })?;

        let target = self.path_for(conv_id);
        // Same-directory temp file → rename is atomic on the same filesystem.
        // The temp name is unique PER CALL (pid + wall-clock nanos + a process
        // monotonic counter) so two concurrent saves for the same conv_id can
        // never share a temp path and clobber each other before the rename.
        let tmp = self.dir.join(format!(
            "conversation-{}.tmp.{}.{}.{}",
            sanitise_conv_id(conv_id),
            std::process::id(),
            unique_temp_token(),
            TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&tmp, &json).map_err(io_err)?;
        std::fs::rename(&tmp, &target).map_err(|source| {
            // Best-effort cleanup of the temp file on a failed rename.
            let _ = std::fs::remove_file(&tmp);
            ConversationStoreError::Io {
                conv_id: conv_id.to_string(),
                source,
            }
        })?;
        Ok(())
    }

    /// Load a conversation from its state file, or a fresh one if absent (§7.4).
    ///
    /// Why: on resume the daemon reloads the hot working buffer; a conversation
    /// that has never been persisted must load as empty rather than erroring, so
    /// the first message of a brand-new `conv_id` is not a failure.
    /// What: if the state file does not exist, returns
    /// [`SmConversation::new`]. Otherwise reads and deserialises it. A present but
    /// malformed file is a hard [`ConversationStoreError::Serde`] (corruption the
    /// operator should see, not silently discard).
    /// Test: `save_then_load_round_trips`, `load_missing_is_fresh`,
    /// `load_rejects_malformed_file`.
    pub fn load(&self, conv_id: &str) -> ConversationStoreResult<SmConversation> {
        let path = self.path_for(conv_id);
        match std::fs::read(&path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|source| ConversationStoreError::Serde {
                    conv_id: conv_id.to_string(),
                    source,
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SmConversation::new()),
            Err(source) => Err(ConversationStoreError::Io {
                conv_id: conv_id.to_string(),
                source,
            }),
        }
    }

    /// The directory holding all SM conversation state files (read-only accessor).
    ///
    /// Why: tests assert the layout; the engine may want it for diagnostics.
    /// What: returns the `<root>/sm/` path.
    /// Test: `store_path_is_under_sm_subdir`.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// Wall-clock nanoseconds since the Unix epoch, for temp-file uniqueness.
///
/// Why: combined with the pid and a process-monotonic counter, the nanosecond
/// timestamp makes a temp filename overwhelmingly unlikely to collide across
/// processes; the counter handles the (common) case where two same-process saves
/// land in the same nanosecond on a coarse clock.
/// What: returns `SystemTime::now()` as nanos since the epoch, falling back to `0`
/// if the clock is somehow before the epoch (the counter still guarantees
/// per-call uniqueness within a process, so a `0` fallback is safe).
/// Test: exercised indirectly by `concurrent_saves_use_distinct_temp_paths`.
fn unique_temp_token() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Sanitise a conversation id into a safe single-path-segment filename stem.
///
/// Why: a `conv_id` is operator/caller-supplied; without sanitisation a value
/// like `../../etc/x` or one containing path separators could write outside the
/// `sm/` directory. Restricting to a conservative character set makes path
/// traversal impossible by construction.
/// What: keeps ASCII alphanumerics, `-`, and `_`; replaces every other byte with
/// `_`. An empty result falls back to `"default"` so there is always a filename.
/// Test: `conv_id_is_sanitised`.
fn sanitise_conv_id(conv_id: &str) -> String {
    let cleaned: String = conv_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "default".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
#[path = "persist_tests.rs"]
mod tests;
