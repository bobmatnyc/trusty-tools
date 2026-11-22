//! One lock discipline for the process-global `$HOME` (#6089).
//!
//! Why: `$HOME` is a process-global, and `cargo test` runs unit tests on a
//! multi-threaded executor, so two tests that redirect it stomp on each other.
//! This crate's convention is a single process-wide mutex, `test_env::HOME_LOCK`
//! — but `#[serial_test::serial]` is a SECOND, disjoint lock over the same
//! global, and a test guarded only by that one interleaves freely with every
//! HOME_LOCK holder. `assistants::tests::home_tests` did exactly that and
//! `okg_store_path_matches_the_owners_spelling` failed once in four full-suite
//! runs, comparing paths under two different tempdirs (#6089). The same
//! mechanism was fixed once for `llm::http::tests` (#3952) and came back here,
//! because that fix was a point repair and nothing checked the rest of the
//! crate. `python_skill`'s `hold_home_for_uv` states the crate-wide property in
//! prose — every HOME mutation sits under HOME_LOCK — and relies on it for
//! hermeticity; this file is what makes that statement checkable instead of
//! hopeful.
//! What: scans this crate's own `src/` for files that mutate `$HOME` and
//! asserts each also acquires `HOME_LOCK`. File granularity, deliberately: it
//! needs no Rust parser, and it is the granularity at which the convention has
//! actually been violated.
//! Test: this file IS the test.

use std::path::{Path, PathBuf};

/// Text that mutates the process-global `$HOME`.
///
/// Blind spot: these are literal substrings, so a mutation routed through a
/// generic helper that takes the variable NAME as a parameter is not seen
/// (#6089).
const HOME_MUTATIONS: &[&str] = &[
    "set_var(\"HOME\"",
    "remove_var(\"HOME\"",
    "EnvVarGuard::set(\"HOME\"",
    "EnvVarGuard::clear(\"HOME\"",
];

/// Text that acquires the crate's one `$HOME` lock.
const HOME_LOCK_ACQUISITIONS: &[&str] = &["HOME_LOCK", "lock_home"];

/// Every `.rs` file under `dir`.
fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Why: the two disjoint locks are invisible at any single call site — a test
/// author reads `#[serial]` on the test above theirs and copies it, which is
/// how #3952 came back as #6089. The invariant has to be asserted somewhere
/// that sees the whole crate.
/// What: fails naming every file that mutates `$HOME` without acquiring
/// `HOME_LOCK`. `#[serial]` is not an accepted substitute: it is a different
/// mutex, so it excludes only other `#[serial]` tests and none of the ~30 files
/// that hold `HOME_LOCK`. A `#[serial]` test may keep that attribute for a
/// second global it also guards — it just has to take `HOME_LOCK` too.
#[test]
fn every_home_mutating_source_file_takes_the_home_lock() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_sources(&src, &mut files);
    assert!(
        !files.is_empty(),
        "no sources found under {}",
        src.display()
    );

    let mut offenders: Vec<String> = Vec::new();
    for file in &files {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        let mutates = HOME_MUTATIONS.iter().any(|pat| text.contains(pat));
        let locks = HOME_LOCK_ACQUISITIONS.iter().any(|pat| text.contains(pat));
        if mutates && !locks {
            offenders.push(
                file.strip_prefix(&src)
                    .unwrap_or(file)
                    .display()
                    .to_string(),
            );
        }
    }
    offenders.sort();

    assert!(
        offenders.is_empty(),
        "these files mutate $HOME without acquiring test_env::HOME_LOCK, so they \
         race every test that does hold it (#6089): {offenders:?}"
    );
}
