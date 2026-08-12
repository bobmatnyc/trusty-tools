//! Source-level guard keeping every spawned `tcode` child hermetic against a
//! live trusty-search daemon (#3036, #3195).
//!
//! Why: `trusty_common::search_index` refuses daemon writes while
//! `running_under_test_harness()` holds (#4255), but that answers for the
//! RUNNING process. `env!("CARGO_BIN_EXE_tcode")` resolves to
//! `target/<profile>/tcode`, outside `deps/`, so a child this suite spawns
//! looks exactly like a user's real invocation: it warms its `--project` into
//! whatever trusty-search daemon it discovers, and on a developer machine
//! that is the operator's live one. The daemon then registers the test's
//! `$TMPDIR/.tmpXXXXXX` fixture and writes `.gitignore` and
//! `.trusty-search/{index.redb,hnsw.usearch,hnsw.keys.json,schema_version.json}`
//! back INSIDE the sandbox, at a moment nobody controls — the warm-up is
//! detached and the daemon walks the tree asynchronously. Whichever
//! before/after diff assertion is open when those files land fails, which is
//! why a different `run_task` test broke each run, why only a machine with a
//! daemon saw it, and why `--test-threads=1` looked green.
//!
//! `tests/support/mod.rs` set `TRUSTY_TEST_HARNESS=1` on the two children it
//! owns, but 28 call sites across 24 test functions built their own `Command`
//! and inherited none of it. Fixing those call sites alone would leave the
//! next one to the author's memory — the same bet that produced #3036, then
//! #3195. This guard makes naming the binary outside the shared helper a test
//! failure instead.
//!
//! What: reads this crate's own `tests/` sources and fails if any file other
//! than `tests/support/mod.rs` names the `CARGO_BIN_EXE_tcode` env var in
//! code. Lexical rather than runtime because a raw `Command::new` has no hook
//! to gate — the only moment the mistake is observable is in the source.
//! Test: `no_test_spawns_the_tcode_binary_unguarded`,
//! `only_the_exact_support_mod_path_is_exempt`.

use std::path::{Path, PathBuf};

/// The one file allowed to name the binary, as path COMPONENTS relative to
/// `tests/`: the shared guarded constructor and the two async spawners beside
/// it.
///
/// Components rather than a string suffix on purpose. `str::ends_with` does
/// not respect path boundaries, so a `tests/cli_support/mod.rs` would have
/// been silently exempted too — an accidental, innocuous-looking way to add
/// the next unguarded spawn while this guard still reported green.
const SPAWN_ENTRY_POINT: [&str; 2] = ["support", "mod.rs"];

/// Every test source spawns `tcode` through `support::tcode_command` (or the
/// `StdioSession`/`HttpDaemon` helpers beside it), never by naming the
/// binary itself.
///
/// A violation is not cosmetic: the child skips `trusty_common`'s
/// test-harness gate and mutates the operator's live trusty-search registry,
/// writing daemon storage into the fixture directory whose contents another
/// test is asserting on.
#[test]
fn no_test_spawns_the_tcode_binary_unguarded() {
    let tests_dir = crate_tests_dir();
    let offenders = unguarded_spawn_sites(&tests_dir, spawn_needle());

    assert!(
        offenders.is_empty(),
        "these test sites spawn the `tcode` binary without the ambient-daemon \
         guard, so the child registers its fixture directory in the operator's \
         LIVE trusty-search daemon and the daemon writes `.trusty-search/` back \
         into the sandbox (#3036, #3195). Call `support::tcode_command()` \
         instead — it sets `TRUSTY_TEST_HARNESS=1` on the child:\n{}",
        offenders.join("\n")
    );
}

/// Only the exact path `tests/support/mod.rs` is exempt — a sibling whose
/// directory merely ENDS in `support` is still caught.
///
/// Why this is pinned: the exemption used `str::ends_with`, which ignores
/// path boundaries. Under that version a `tests/cli_support/mod.rs` was
/// exempted by accident, which is precisely the un-guarded spawn this module
/// exists to make impossible.
#[test]
fn only_the_exact_support_mod_path_is_exempt() {
    let needle = spawn_needle();
    let unguarded = format!("    let out = Command::new(env!(\"{needle}\"))");
    let root = tempfile::tempdir().expect("fixture root");

    for dir in ["support", "cli_support", "support/nested"] {
        std::fs::create_dir_all(root.path().join(dir)).expect("mkdir fixture");
        std::fs::write(root.path().join(dir).join("mod.rs"), &unguarded).expect("write fixture");
    }
    // A guarded caller must never be reported, wherever it lives.
    std::fs::write(
        root.path().join("guarded_e2e.rs"),
        "    let out = support::tcode_command()",
    )
    .expect("write guarded fixture");

    let offenders = unguarded_spawn_sites(root.path(), needle);
    let root_prefix = format!("{}/", root.path().display());
    let mut flagged: Vec<String> = offenders
        .iter()
        .map(|o| {
            o.trim_start_matches(&root_prefix)
                .split(':')
                .next()
                .unwrap_or(o)
                .to_string()
        })
        .collect();
    flagged.sort();

    assert_eq!(
        flagged,
        vec!["cli_support/mod.rs", "support/nested/mod.rs"],
        "only `<tests>/support/mod.rs` may be exempt; a directory that merely \
         ends in `support`, and a `support/` nested deeper, must both be \
         caught. Got: {offenders:?}"
    );
}

/// The guard is only meaningful if it actually found the `tests/` tree — an
/// empty scan would pass vacuously after any directory rename.
#[test]
fn the_spawn_guard_actually_scans_the_test_sources() {
    let tests_dir = crate_tests_dir();
    let sources = rust_sources(&tests_dir);

    assert!(
        sources.len() > 5,
        "expected this crate's `tests/` tree; found {} sources under {}",
        sources.len(),
        tests_dir.display()
    );
    assert!(
        sources
            .iter()
            .any(|(p, _)| is_spawn_entry_point(p, &tests_dir)),
        "the one allowed spawn entry point `tests/{}` was not found — the \
         exemption in `no_test_spawns_the_tcode_binary_unguarded` would be \
         checking a path that no longer exists",
        SPAWN_ENTRY_POINT.join("/")
    );
}

/// The env var whose appearance in a test source means "this spawns the real
/// binary", split so this guard never matches its own source.
fn spawn_needle() -> &'static str {
    concat!("CARGO_BIN_EXE", "_tcode")
}

/// This crate's `tests/` directory.
fn crate_tests_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests")
}

/// `"<path>:<lineno>: <line>"` for every place under `tests_dir` that names
/// the binary outside the one exempt entry point.
fn unguarded_spawn_sites(tests_dir: &Path, needle: &str) -> Vec<String> {
    let mut offenders = Vec::new();
    for (path, source) in rust_sources(tests_dir) {
        if is_spawn_entry_point(&path, tests_dir) {
            continue;
        }
        for (lineno, line) in source.lines().enumerate() {
            // Prose may still name it; only code is a spawn.
            if line.contains(needle) && !line.trim_start().starts_with("//") {
                offenders.push(format!(
                    "{}:{}: {}",
                    path.display(),
                    lineno + 1,
                    line.trim()
                ));
            }
        }
    }
    offenders
}

/// Is `path` exactly `<tests_dir>/support/mod.rs`?
///
/// Compares path COMPONENTS, so `cli_support/mod.rs` and
/// `support/nested/mod.rs` are both non-exempt.
fn is_spawn_entry_point(path: &Path, tests_dir: &Path) -> bool {
    path.strip_prefix(tests_dir).is_ok_and(|relative| {
        relative
            .components()
            .map(|c| c.as_os_str())
            .eq(SPAWN_ENTRY_POINT.iter().map(std::ffi::OsStr::new))
    })
}

/// Every `.rs` file under `dir`, recursively, as `(path, contents)`.
fn rust_sources(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(rust_sources(&path));
        } else if path.extension().is_some_and(|e| e == "rs")
            && let Ok(source) = std::fs::read_to_string(&path)
        {
            found.push((path, source));
        }
    }
    found
}
