//! Ratchet: every palace-scoped redb open in this crate routes through the
//! bounded page-cache builder (#7106).
//!
//! Why: trusty-common carries the same ratchet, but it scans trusty-common's
//! own tree only. The #7106 fix ALSO changed a palace-scoped open in this crate
//! — `console_metrics::disk_stats::open_read_only`, which surveys every palace's
//! redb files on a console poll — and reverting that one line to a bare
//! `ReadOnlyDatabase::open` would have passed every gate in the workspace while
//! restoring redb's 1 GiB per-database page-cache ceiling for each file it
//! touches. A ratchet that stops at a crate boundary is a ratchet with a hole
//! the size of the daemon.
//!
//! What: walks this crate's `src/` for a direct redb open — `Database::create(`,
//! `Database::open(`, `ReadOnlyDatabase::open(`, `Database::builder(`,
//! `Builder::new(` — outside test files, outside `#[cfg(test)]` module bodies,
//! and outside [`ALLOWED`]. Any hit fails naming the file and line.
//!
//! The builder spellings are matched because a builder that never calls
//! `set_cache_size` takes the same 1 GiB default; matching only the two
//! constructors would let the identical regression back in under a different
//! name. A bare `::builder(` is deliberately not matched — it hits
//! `reqwest::Client::builder()` elsewhere in the workspace, and a ratchet that
//! cries wolf earns an allowlist row instead of a fix.
//!
//! Deliberately source-level, not behavioural: redb exposes no way to read a
//! database's configured cache size back, so the ceiling cannot be asserted from
//! a handle. The scan is the only mechanical statement of the invariant.
//!
//! Test: `every_palace_redb_open_in_trusty_memory_is_bounded`.

use std::path::{Path, PathBuf};

/// Source files allowed to call redb directly.
///
/// `commands/kuzu_migrate.rs` opens a FOREIGN `store.redb` written by
/// kuzu-memory, not a palace file: it is a one-shot CLI import, not the
/// long-lived daemon, and the palace cache ceiling has no meaning for it.
/// Nothing else in this tree may open redb outside
/// `trusty_common::redb_cache`.
const ALLOWED: &[&str] = &["src/commands/kuzu_migrate.rs"];

/// Trees scanned, relative to the crate root.
const SCANNED: &[&str] = &["src"];

/// Direct-open spellings that bypass the bounded builder.
const BYPASS_PATTERNS: &[&str] = &[
    "Database::create(",
    "Database::open(",
    "ReadOnlyDatabase::open(",
    "Database::builder(",
    "Builder::new(",
];

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

/// A file whose own name marks it as test-only, per this repo's cap convention
/// (`sloc-cap.md`): a test that opens redb deliberately is not a production
/// bypass.
fn is_test_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name == "tests.rs"
        || name.ends_with("_test.rs")
        || name.ends_with("_tests.rs")
        || path.components().any(|c| c.as_os_str() == "tests")
}

/// Bypass hits in one file, as `path:line: text`.
///
/// Why: the brace-depth walk over inline `#[cfg(test)] mod … {` bodies is the
/// same region rule `check_line_cap.sh` applies, kept in a helper so the test
/// body reads as the assertion it is.
/// What: skips comment lines and `#[cfg(test)]` module bodies, then matches
/// [`BYPASS_PATTERNS`].
/// Test: `every_palace_redb_open_in_trusty_memory_is_bounded`.
fn bypasses_in(rel: &str, text: &str) -> Vec<String> {
    let mut hits = Vec::new();
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
        if BYPASS_PATTERNS.iter().any(|p| line.contains(p)) {
            hits.push(format!("{rel}:{}: {}", idx + 1, line.trim()));
        }
    }
    hits
}

/// Why (#7106): one unbounded palace open is enough to bring redb's 1 GiB
/// ceiling back for that file, and it would look like every other open in
/// review.
/// What: walks this crate's sources and fails on any direct redb open outside
/// [`ALLOWED`].
/// Test: this test.
#[test]
fn every_palace_redb_open_in_trusty_memory_is_bounded() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for tree in SCANNED {
        let path = root.join(tree);
        assert!(
            path.exists(),
            "scan target {tree:?} does not exist — the paths in SCANNED are stale, \
             and a ratchet that scans nothing passes for the wrong reason"
        );
        if path.is_dir() {
            collect_rs(&path, &mut files);
        } else {
            files.push(path);
        }
    }
    assert!(!files.is_empty(), "scan found no sources under {SCANNED:?}");

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
        bypasses.extend(bypasses_in(&rel, &text));
    }

    assert!(
        bypasses.is_empty(),
        "#7106: these palace-scoped redb opens bypass `trusty_common::redb_cache` \
         and therefore take redb's 1 GiB per-database page-cache ceiling:\n  {}",
        bypasses.join("\n  ")
    );
}
