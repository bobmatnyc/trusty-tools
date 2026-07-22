//! Content-hash stamping for the bundled agent set (#3556).
//!
//! Why: the deployed copy of the bundled roster at
//! `$HOME/.trusty-agents/agents/` used to be written exactly once and never
//! revisited, so a binary upgrade that changed a bundled template (e.g. a
//! new tool grant on the base `assistant` agent) never reached a machine
//! that already had the old file deployed. A stable, deterministic hash of
//! the embedded bundle's actual bytes — persisted alongside the deployed
//! files — lets `ensure_bundled_agents_deployed` detect "the binary's
//! template set changed since we last deployed" automatically, with no
//! manually-bumped version constant to forget.
//! What: [`compute`] hashes an order-independent set of `(path, bytes)`
//! pairs into a stable hex digest; [`read`]/[`write`] persist that digest
//! to a fixed `.bundled-stamp` file inside the deployed agents directory.
//! Test: `stamp_stable_regardless_of_input_order`,
//! `stamp_changes_when_content_changes`,
//! `stamp_changes_when_a_path_changes`, `read_missing_stamp_is_none`,
//! `write_then_read_round_trips` (this file's own `#[cfg(test)] mod tests`).

use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

/// Fixed filename for the persisted bundle stamp, written as a sibling of
/// the deployed agent files inside the target agents directory.
const STAMP_FILE_NAME: &str = ".bundled-stamp";

/// Compute a stable, order-independent hex digest over a set of embedded
/// `(relative path, file bytes)` pairs.
///
/// Why: `rust-embed`'s iteration order is not a contract callers should rely
/// on being stable across builds/platforms — sorting by path before hashing
/// makes the resulting stamp depend only on the actual (path, content) set,
/// not on iteration order.
/// What: sorts `entries` by path, then folds each pair's path bytes and
/// content bytes (each NUL-terminated, so `("a", "bc")` cannot collide with
/// `("ab", "c")`) into a single SHA-256 hasher, returning the lowercase hex
/// digest.
/// Test: `stamp_stable_regardless_of_input_order`,
/// `stamp_changes_when_content_changes`, `stamp_changes_when_a_path_changes`.
pub(super) fn compute(mut entries: Vec<(String, Vec<u8>)>) -> String {
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = Sha256::new();
    for (path, bytes) in &entries {
        hasher.update(path.as_bytes());
        hasher.update([0u8]);
        hasher.update(bytes);
        hasher.update([0u8]);
    }
    format!("{:x}", hasher.finalize())
}

/// Read the previously-persisted stamp from `target_dir/.bundled-stamp`.
///
/// Why: a missing stamp (fresh `target_dir`, or one deployed before #3556)
/// must be treated as "stale" by the caller so the very next
/// `ensure_bundled_agents_deployed` run establishes a baseline — this
/// returns `None` rather than an `Err` so that "no stamp yet" is
/// indistinguishable from any other first-run state.
/// What: reads the file as UTF-8 and trims surrounding whitespace; any I/O
/// error (including "file does not exist") collapses to `None`.
/// Test: `read_missing_stamp_is_none`, `write_then_read_round_trips`.
pub(super) fn read(target_dir: &Path) -> Option<String> {
    std::fs::read_to_string(target_dir.join(STAMP_FILE_NAME))
        .ok()
        .map(|s| s.trim().to_string())
}

/// Persist `value` as the current stamp at `target_dir/.bundled-stamp`.
///
/// Why: callers write the new stamp only AFTER a successful reprovision
/// pass, so a mid-refresh failure (e.g. a read-only file partway through)
/// leaves the OLD stamp in place — the next run is correctly treated as
/// still stale and retries, rather than silently believing a partial
/// refresh completed. Routed through `crate::state_writer::atomic_write`
/// (#3556 code-critic follow-up, HIGH) rather than a plain `std::fs::write`
/// so concurrent `tagent` processes (`delegate_to_agent` spawns one per
/// delegation) can never observe a torn stamp file, and a crash mid-write
/// leaves the PRIOR stamp intact (via tmp-file + rename) instead of a
/// half-written one that would falsely read as "not stale" on the next run.
/// What: writes `value` verbatim (no trailing newline) to the stamp file.
/// `atomic_write` creates `target_dir` itself if it doesn't already exist,
/// so this has no ordering dependency on another write happening first.
/// Test: `write_then_read_round_trips`.
pub(super) fn write(target_dir: &Path, value: &str) -> Result<()> {
    let path = target_dir.join(STAMP_FILE_NAME);
    crate::state_writer::atomic_write(&path, value.as_bytes())
        .with_context(|| format!("failed to write bundled-agent stamp to {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the hash must depend on the (path, content) SET, not the order
    /// `entries` happens to be passed in — `rust-embed` gives no ordering
    /// guarantee across builds/platforms.
    /// Test: itself.
    #[test]
    fn stamp_stable_regardless_of_input_order() {
        let a = vec![
            ("a.toml".to_string(), b"one".to_vec()),
            ("b.toml".to_string(), b"two".to_vec()),
        ];
        let b = vec![
            ("b.toml".to_string(), b"two".to_vec()),
            ("a.toml".to_string(), b"one".to_vec()),
        ];
        assert_eq!(compute(a), compute(b));
    }

    /// Why: the whole point of stamping is detecting a content change —
    /// changing one file's bytes must change the stamp.
    /// Test: itself.
    #[test]
    fn stamp_changes_when_content_changes() {
        let before = vec![("a.toml".to_string(), b"one".to_vec())];
        let after = vec![("a.toml".to_string(), b"ONE-CHANGED".to_vec())];
        assert_ne!(compute(before), compute(after));
    }

    /// Why: a NUL-separated join without this test could still theoretically
    /// collide two different (path, content) sets that concatenate to the
    /// same bytes; pin that renaming a path (holding content shape similar)
    /// also changes the stamp.
    /// Test: itself.
    #[test]
    fn stamp_changes_when_a_path_changes() {
        let before = vec![("a.toml".to_string(), b"content".to_vec())];
        let after = vec![("renamed.toml".to_string(), b"content".to_vec())];
        assert_ne!(compute(before), compute(after));
    }

    /// Why: a fresh (or pre-#3556) deploy target has no stamp file at all —
    /// callers must treat that as "stale", not error out.
    /// Test: itself.
    #[test]
    fn read_missing_stamp_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(read(tmp.path()), None);
    }

    /// Why: the persisted format must round-trip exactly what was written.
    /// Test: itself.
    #[test]
    fn write_then_read_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "abc123").unwrap();
        assert_eq!(read(tmp.path()), Some("abc123".to_string()));
    }
}
