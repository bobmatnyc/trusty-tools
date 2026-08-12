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
//! owns, but 26 tests built their own `Command` and inherited none of it.
//! Fixing those 26 call sites alone would leave the 27th to the next author's
//! memory — the same bet that produced #3036, then #3195. This guard makes
//! naming the binary outside the shared helper a test failure instead.
//!
//! What: reads this crate's own `tests/` sources and fails if any file other
//! than `tests/support/mod.rs` names the `CARGO_BIN_EXE_tcode` env var in
//! code. Lexical rather than runtime because a raw `Command::new` has no hook
//! to gate — the only moment the mistake is observable is in the source.
//! Test: this module IS the guard.

/// The one file allowed to name the binary: the shared, guarded constructor
/// and the two async spawners beside it.
const SPAWN_ENTRY_POINT: &str = "support/mod.rs";

/// Every test source spawns `tcode` through `support::tcode_command` (or the
/// `StdioSession`/`HttpDaemon` helpers beside it), never by naming the
/// binary itself.
///
/// A violation is not cosmetic: the child skips
/// `trusty_common`'s test-harness gate and mutates the operator's live
/// trusty-search registry, writing daemon storage into the fixture directory
/// whose contents another test is asserting on.
#[test]
fn no_test_spawns_the_tcode_binary_unguarded() {
    // Split so this guard does not match itself.
    let needle = concat!("CARGO_BIN_EXE", "_tcode");
    let tests_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");

    let mut offenders = Vec::new();
    for (path, source) in rust_sources(&tests_dir) {
        if path.ends_with(SPAWN_ENTRY_POINT) {
            continue;
        }
        for (lineno, line) in source.lines().enumerate() {
            // Prose may still name it; only code is a spawn.
            if line.contains(needle) && !line.trim_start().starts_with("//") {
                offenders.push(format!("{path}:{}: {}", lineno + 1, line.trim()));
            }
        }
    }

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

/// The guard is only meaningful if it actually found the `tests/` tree — an
/// empty scan would pass vacuously after any directory rename.
#[test]
fn the_spawn_guard_actually_scans_the_test_sources() {
    let tests_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let sources = rust_sources(&tests_dir);

    assert!(
        sources.len() > 5,
        "expected this crate's `tests/` tree; found {} sources under {}",
        sources.len(),
        tests_dir.display()
    );
    assert!(
        sources.iter().any(|(p, _)| p.ends_with(SPAWN_ENTRY_POINT)),
        "the one allowed spawn entry point `{SPAWN_ENTRY_POINT}` was not found — \
         the exemption in `no_test_spawns_the_tcode_binary_unguarded` would be \
         checking a path that no longer exists"
    );
}

/// Every `.rs` file under `dir`, recursively, as `(display path, contents)`.
fn rust_sources(dir: &std::path::Path) -> Vec<(String, String)> {
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
            found.push((path.display().to_string(), source));
        }
    }
    found
}
