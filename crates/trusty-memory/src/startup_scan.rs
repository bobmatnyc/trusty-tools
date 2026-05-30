//! Startup-time single-pass pin-file scanner.
//!
//! Why: the doctor-command path calls `scan_project_dirs_for_pin` once *per
//! palace id*, which means N palace-id passes over the same directory tree.
//! At daemon startup we do not yet know which palaces will be loaded, so we
//! want a single readdir pass that maps every pin file found to its palace id
//! — building the complete `palace_id → project_path` map in one sweep.
//! This feeds `AppState::pin_project_map` so hot-path handlers can look up
//! a project's filesystem location by palace id without any further I/O.
//! What: exports `scan_pin_map`, a pure, synchronous, best-effort function
//! that walks one level under each of the standard search roots and returns a
//! `HashMap<String, PathBuf>` of discovered `palace_id → project_path`
//! entries. Opening palaces is strictly forbidden here — this is metadata
//! discovery only.
//! Test: see unit-test section at the bottom of this file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::project_root::{read_project_pin, PIN_FILE_REL};

/// Return the canonical search roots used for startup pin discovery.
///
/// Why: the set of roots must match the set used by the doctor-command path
/// (`audit_palaces` in `commands/doctor.rs`) so palace resolution is
/// consistent whether a user runs `trusty-memory doctor` or queries the
/// in-memory map built at daemon start. Defining it here (and re-using from
/// doctor) eliminates the risk of the two lists diverging.
/// What: resolves `$HOME` via `dirs::home_dir` and returns the four standard
/// roots: `~/Projects`, `~/Developer`, `~/Code`, and `~` itself. If
/// `home_dir()` returns `None` (unusual — usually implies a missing HOME env
/// var), the returned `Vec` is empty and the caller contributes nothing to the
/// map.
/// Test: `scan_pin_map_missing_root_dir_contributes_nothing` — passes a
/// non-existent path as the sole search root and asserts an empty map.
pub fn default_search_dirs() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    vec![
        home.join("Projects"),
        home.join("Developer"),
        home.join("Code"),
        home.clone(),
    ]
}

/// Walk `search_dirs` one level deep and build a `palace_id → project_path` map.
///
/// Why: a single-pass scan lets the daemon build its pin-discovery map once
/// at startup — O(#search_dirs × #projects_per_dir) readdir operations —
/// instead of O(#palaces × #projects) with the per-id approach used by the
/// doctor path. The map is consumed by `AppState::pin_project_map` so
/// handlers can resolve a palace id to a project path cheaply without any
/// further filesystem I/O. IMPORTANT: this function must never open a palace
/// (`PalaceHandle::open`), create any directory, or write any file; it is
/// strictly scan-only.
/// What: for each `search_dir` that exists, calls `read_dir` one level and
/// for each subdirectory entry reads `<entry>/.trusty-tools/trusty-memory.yaml`.
/// Successful parses contribute `pin.palace → entry_path` to the returned
/// map. Entries without a pin file, entries that are not directories, and all
/// I/O / YAML parse errors are silently skipped with a `tracing::debug!` log.
/// Only the FIRST occurrence of a given palace id is recorded; if two project
/// directories claim the same id, the one encountered first (filesystem
/// readdir order) wins and the collision is logged at `warn!`.
/// Returns immediately with an empty map if `search_dirs` is empty.
/// Test: `scan_pin_map_two_pins_found`, `scan_pin_map_skips_corrupt_yaml`,
/// `scan_pin_map_one_level_only`, `scan_pin_map_missing_root_dir_contributes_nothing`.
pub fn scan_pin_map(search_dirs: &[PathBuf]) -> HashMap<String, PathBuf> {
    let mut map: HashMap<String, PathBuf> = HashMap::new();

    for search_dir in search_dirs {
        scan_one_root(search_dir, &mut map);
    }

    map
}

/// Scan a single search root one level deep, inserting discovered pins into `map`.
///
/// Why: extracted from `scan_pin_map` to keep the per-root logic testable in
/// isolation and to make the borrow / mutability structure clear. The caller
/// accumulates across multiple roots.
/// What: calls `read_dir(search_dir)`; if the root does not exist or cannot
/// be read, returns immediately (no error propagated). For each directory
/// entry, attempts to read the pin file via `read_project_pin`. Successful
/// pins are inserted into `map` unless the id is already present (collision
/// warning). Non-directory entries and any I/O / parse errors are skipped
/// with a `tracing::debug!` log.
/// Test: covered transitively by `scan_pin_map_*` tests.
fn scan_one_root(search_dir: &Path, map: &mut HashMap<String, PathBuf>) {
    let entries = match std::fs::read_dir(search_dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!(
                dir = %search_dir.display(),
                "startup scan: cannot read dir ({e}); skipping"
            );
            return;
        }
    };

    for entry in entries.flatten() {
        let candidate = entry.path();

        // Only descend into directories.
        if !candidate.is_dir() {
            continue;
        }

        // Attempt to read the pin file; any error is silently skipped.
        let pin = match read_project_pin(&candidate) {
            Ok(Some(p)) => p,
            Ok(None) => continue, // no pin file — common case
            Err(e) => {
                tracing::debug!(
                    path = %candidate.join(PIN_FILE_REL).display(),
                    "startup scan: skipping unreadable/corrupt pin file ({e})"
                );
                continue;
            }
        };

        // Record the mapping; warn on collision (two projects claim same id).
        let palace_id = pin.palace;
        if let Some(existing) = map.get(&palace_id) {
            tracing::warn!(
                palace_id = %palace_id,
                first  = %existing.display(),
                second = %candidate.display(),
                "startup scan: duplicate palace id claimed by two projects; keeping first"
            );
        } else {
            tracing::debug!(
                palace_id = %palace_id,
                project   = %candidate.display(),
                "startup scan: discovered pin"
            );
            map.insert(palace_id, candidate);
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_root::{write_project_pin, ProjectPin, PIN_SCHEMA_VERSION};
    use std::fs;

    /// Helper: write a minimal pin file claiming `palace_id` under
    /// `project_dir/.trusty-tools/trusty-memory.yaml`.
    fn write_pin(project_dir: &Path, palace_id: &str) {
        let pin = ProjectPin {
            schema_version: PIN_SCHEMA_VERSION,
            palace: palace_id.to_string(),
            note: None,
        };
        write_project_pin(project_dir, &pin).expect("write_pin test helper");
    }

    /// Why: the core contract — two projects each with a pin file must both
    /// appear in the returned map, keyed by their palace id.
    /// What: create a temp tree with search_root/alpha-proj (pin: alpha) and
    /// search_root/beta-proj (pin: beta); assert the map contains exactly
    /// {alpha → alpha-proj, beta → beta-proj}.
    /// Test: itself.
    #[test]
    fn scan_pin_map_two_pins_found() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        let alpha_dir = root.join("alpha-proj");
        let beta_dir = root.join("beta-proj");
        fs::create_dir_all(&alpha_dir).unwrap();
        fs::create_dir_all(&beta_dir).unwrap();

        write_pin(&alpha_dir, "alpha");
        write_pin(&beta_dir, "beta");

        // A third subdirectory with no pin file — must be skipped silently.
        let no_pin_dir = root.join("no-pin-proj");
        fs::create_dir_all(&no_pin_dir).unwrap();

        let map = scan_pin_map(&[root.to_path_buf()]);

        assert_eq!(map.len(), 2, "expected exactly 2 entries; got: {map:?}");

        // Canonicalize paths on both sides to handle macOS /private symlinks.
        let actual_alpha = fs::canonicalize(map.get("alpha").expect("alpha")).unwrap();
        let actual_beta = fs::canonicalize(map.get("beta").expect("beta")).unwrap();
        let expected_alpha = fs::canonicalize(&alpha_dir).unwrap();
        let expected_beta = fs::canonicalize(&beta_dir).unwrap();
        assert_eq!(actual_alpha, expected_alpha);
        assert_eq!(actual_beta, expected_beta);
    }

    /// Why: a corrupt YAML file must not crash the scanner or block other
    /// entries — it must be silently skipped.
    /// What: create a temp dir with a valid pin project and a project whose
    /// pin file is syntactically invalid YAML; assert the valid project
    /// appears in the map and the corrupt one does not.
    /// Test: itself.
    #[test]
    fn scan_pin_map_skips_corrupt_yaml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        let good_dir = root.join("good-proj");
        let bad_dir = root.join("bad-proj");
        fs::create_dir_all(&good_dir).unwrap();
        fs::create_dir_all(&bad_dir).unwrap();

        write_pin(&good_dir, "good-palace");

        // Write a corrupt pin file (invalid YAML).
        let trusty_dir = bad_dir.join(".trusty-tools");
        fs::create_dir_all(&trusty_dir).unwrap();
        fs::write(
            trusty_dir.join("trusty-memory.yaml"),
            b"palace: \x00\nbroken: [",
        )
        .unwrap();

        let map = scan_pin_map(&[root.to_path_buf()]);

        assert_eq!(map.len(), 1, "only the valid pin must be recorded");
        assert!(
            map.contains_key("good-palace"),
            "good-palace must be present"
        );
        // No entry for bad-proj
        assert!(
            !map.values().any(|p| p.ends_with("bad-proj")),
            "bad-proj must not appear in the map"
        );
    }

    /// Why: the scan must be bounded to ONE level under each search root; a
    /// pin nested two levels deep must NOT be discovered (prevents scanning
    /// deep trees whose I/O cost grows quadratically).
    /// What: create search_root/outer-proj/inner-proj with a pin file; assert
    /// the map is empty (inner-proj is two levels below the search root).
    /// Test: itself.
    #[test]
    fn scan_pin_map_one_level_only() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        let outer = root.join("outer-proj");
        let inner = outer.join("inner-proj");
        fs::create_dir_all(&inner).unwrap();

        // inner-proj has a pin, but it's two levels below root.
        write_pin(&inner, "deep-palace");

        // outer-proj has no pin.

        let map = scan_pin_map(&[root.to_path_buf()]);

        assert!(
            map.is_empty(),
            "two-level-deep pin must not be discovered; got: {map:?}"
        );
    }

    /// Why: a non-existent or inaccessible search root must not cause a panic
    /// or an error — it just contributes zero entries to the map.
    /// What: pass a path that does not exist; assert `scan_pin_map` returns
    /// an empty map without panicking.
    /// Test: itself.
    #[test]
    fn scan_pin_map_missing_root_dir_contributes_nothing() {
        let map = scan_pin_map(&[PathBuf::from("/tmp/nonexistent-trusty-scan-test-dir-xyz")]);
        assert!(
            map.is_empty(),
            "missing root must yield empty map; got: {map:?}"
        );
    }

    /// Why: an empty `search_dirs` slice is a valid (degenerate) input — the
    /// caller may have no home dir. The scanner must return an empty map.
    /// Test: itself.
    #[test]
    fn scan_pin_map_empty_search_dirs() {
        let map = scan_pin_map(&[]);
        assert!(map.is_empty());
    }

    /// Why: when multiple search roots are supplied, pins from ALL roots must
    /// appear in the combined map (multi-root accumulation).
    /// What: create two separate temp roots each containing one pinned project;
    /// assert both pins appear in the final map.
    /// Test: itself.
    #[test]
    fn scan_pin_map_accumulates_across_roots() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root1 = tmp.path().join("root1");
        let root2 = tmp.path().join("root2");

        let proj1 = root1.join("proj-one");
        let proj2 = root2.join("proj-two");
        fs::create_dir_all(&proj1).unwrap();
        fs::create_dir_all(&proj2).unwrap();

        write_pin(&proj1, "palace-one");
        write_pin(&proj2, "palace-two");

        let map = scan_pin_map(&[root1, root2]);

        assert_eq!(map.len(), 2);
        assert!(map.contains_key("palace-one"));
        assert!(map.contains_key("palace-two"));
    }
}
