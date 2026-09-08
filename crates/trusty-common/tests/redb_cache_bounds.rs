//! Ratchet: every palace-scoped redb open routes through the bounded
//! page-cache builder (#7106).
//!
//! Why: the fix for #7106 is only as good as its coverage. `redb::Builder`'s
//! 1 GiB default `cache_size` is per `Database`, so ONE store that goes back to
//! a bare `Database::create` reinstates the unbounded ceiling for its file, and
//! nothing at the call site says so — the daemon just grows again. A unit test
//! on the helper cannot catch that, because the helper still works; what has to
//! be pinned is that nothing bypasses it.
//!
//! What: scans the memory-core source tree plus the shared `redb_open` recovery
//! path for a direct redb open outside `#[cfg(test)]` bodies and outside the one
//! module allowed to call redb directly (`redb_cache`). Any hit is a bypass and
//! fails the test naming the file and line.
//!
//! The scan covers the BUILDER path too (`Database::builder(`,
//! `Builder::new(`), not just `Database::create` / `open`: a builder that never
//! calls `set_cache_size` takes the same 1 GiB default, so matching only the
//! two constructors would let the identical regression through by a different
//! spelling. A bare `::builder(` is deliberately NOT matched — it hits
//! `reqwest::Client::builder()` in this same tree, and a ratchet that cries wolf
//! gets an allowlist row instead of a fix.
//!
//! Scope: this crate only. `trusty-memory` has its own copy of this ratchet
//! (`crates/trusty-memory/tests/redb_cache_bounds.rs`) covering its own sources
//! and its own allowlist — a scan that reached across the crate boundary from
//! here would go silently inert the day that crate moved, which is the exact
//! failure this test exists to prevent.
//!
//! Deliberately source-level, not behavioural: redb exposes no way to read a
//! database's configured cache size back, so the ceiling cannot be asserted
//! from a handle. The scan is the only mechanical statement of the invariant.
//!
//! Test: `every_memory_core_redb_open_is_bounded`.

#![cfg(feature = "memory-core")]

use std::path::{Path, PathBuf};

/// Source files allowed to call `redb::Database::{create,open}` directly.
///
/// `redb_cache` IS the bounded builder, so it necessarily calls redb itself.
/// Nothing else in these trees may.
const ALLOWED: &[&str] = &["src/redb_cache.rs"];

/// Trees scanned for bypasses, relative to the crate root.
const SCANNED: &[&str] = &["src/memory_core", "src/redb_open.rs", "src/redb_cache.rs"];

fn collect_rs(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// A file whose own name marks it as test-only.
///
/// The cap convention in this repo (`sloc-cap.md`) treats `tests.rs`,
/// `*_test.rs`, `*_tests.rs` and anything under a `/tests/` segment as test
/// code; a test that holds a redb lock deliberately is not a production bypass.
fn is_test_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name == "tests.rs"
        || name.ends_with("_test.rs")
        || name.ends_with("_tests.rs")
        || path.components().any(|c| c.as_os_str() == "tests")
}

/// Why (#7106): one unbounded open is enough to bring the 1 GiB ceiling back
/// for that file, and it would look like every other open in review.
/// What: walks the memory-core and shared-recovery sources, skips test files
/// and `#[cfg(test)]` module bodies, and fails on any remaining direct redb
/// open — constructor or builder — outside [`ALLOWED`].
/// Test: this test.
#[test]
fn every_memory_core_redb_open_is_bounded() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for tree in SCANNED {
        let path = root.join(tree);
        if path.is_dir() {
            collect_rs(&path, &mut files);
        } else if path.is_file() {
            files.push(path);
        }
    }
    assert!(
        !files.is_empty(),
        "scan found no sources — the paths in SCANNED are stale"
    );

    let mut bypasses = Vec::new();
    for file in files {
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        if ALLOWED.contains(&rel.as_str()) || is_test_file(&file) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        // Track the brace depth of an inline `#[cfg(test)] mod … {` body so a
        // test module's deliberate lock-holder opens are not counted, matching
        // how `check_line_cap.sh` skips the same regions.
        let mut in_cfg_test = false;
        let mut depth: i32 = 0;
        let mut pending_cfg_test = false;
        for (idx, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if in_cfg_test {
                depth += line.matches('{').count() as i32;
                depth -= line.matches('}').count() as i32;
                if depth <= 0 {
                    in_cfg_test = false;
                }
                continue;
            }
            if trimmed.starts_with("#[cfg(test)]") {
                pending_cfg_test = true;
                continue;
            }
            if pending_cfg_test {
                pending_cfg_test = false;
                if trimmed.starts_with("mod ") && line.contains('{') {
                    in_cfg_test = true;
                    depth = line.matches('{').count() as i32 - line.matches('}').count() as i32;
                    continue;
                }
            }
            if trimmed.starts_with("//") {
                continue;
            }
            if line.contains("Database::create(")
                || line.contains("Database::open(")
                || line.contains("ReadOnlyDatabase::open(")
                || line.contains("Database::builder(")
                || line.contains("Builder::new(")
            {
                bypasses.push(format!("{rel}:{}: {}", idx + 1, line.trim()));
            }
        }
    }

    assert!(
        bypasses.is_empty(),
        "#7106: these palace-scoped redb opens bypass \
         `trusty_common::redb_cache::create_palace_db` and therefore take redb's \
         1 GiB per-database page-cache ceiling:\n  {}",
        bypasses.join("\n  ")
    );
}
