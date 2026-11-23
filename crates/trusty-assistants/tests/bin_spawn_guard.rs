//! Every binary spawn goes through `tests/common` (#11632).
//!
//! Why: a test that spawned `ta` with the real `$HOME` moved the operator's
//! assistant homes. `common::ta`/`common::tagent` always set a temporary home;
//! this guard keeps a new spawn from bypassing them, and keeps every test
//! binary under the process-start HOME sandbox `common` installs.
//! What: fails when a non-comment line outside `tests/common/mod.rs` names a
//! Cargo binary-path variable, or when a top-level `tests/*.rs` target does not
//! declare `mod common;`.
//! Test: this file IS the test.

use std::path::{Path, PathBuf};

mod common;

/// The Cargo binary-path variable prefix, split so this file does not match
/// itself.
const BIN_PATH_VAR: &str = concat!("CARGO_BIN", "_EXE_");

/// The one file allowed to name a binary path.
const HELPER: &str = "tests/common/mod.rs";

/// Each non-comment line of `body` that names a binary path, as `rel:line`.
fn violations(rel: &str, body: &str) -> Vec<String> {
    if rel == HELPER {
        return Vec::new();
    }
    body.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
        .filter(|(_, line)| line.contains(BIN_PATH_VAR))
        .map(|(n, _)| format!("{rel}:{}", n + 1))
        .collect()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir)
        .expect("read dir")
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Why: a spawn that names the binary path directly skips the temporary home.
/// Test: itself.
#[test]
fn binary_spawns_go_through_the_helper() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    rust_files(&root.join("tests"), &mut files);
    assert!(files.len() > 100, "scanned too few files: {}", files.len());
    let found: Vec<String> = files
        .iter()
        .flat_map(|path| {
            let rel = path
                .strip_prefix(root)
                .expect("under the crate")
                .to_string_lossy()
                .replace('\\', "/");
            let body = std::fs::read_to_string(path).expect("read source");
            violations(&rel, &body)
        })
        .collect();
    assert!(
        found.is_empty(),
        "spawn the binary through `common::ta`/`common::tagent` instead: {found:?}"
    );
}

/// Why: a test binary without `mod common;` runs without the process-start
/// HOME sandbox.
/// Test: itself.
#[test]
fn every_test_target_installs_the_home_sandbox() {
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let missing: Vec<String> = std::fs::read_dir(&tests)
        .expect("read tests/")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "rs"))
        .filter(|p| {
            let body = std::fs::read_to_string(p).expect("read test");
            !body.lines().any(|l| l.trim() == "mod common;")
        })
        .map(|p| p.display().to_string())
        .collect();
    assert!(missing.is_empty(), "add `mod common;` to: {missing:?}");
}

/// Why: the scan must be able to fail, and must not flag a comment.
/// Test: itself.
#[test]
fn a_direct_spawn_is_flagged_and_a_comment_is_not() {
    let planted = format!(
        "// {BIN_PATH_VAR}ta in a comment\nlet c = Command::new(env!(\"{BIN_PATH_VAR}ta\"));\n"
    );
    assert_eq!(
        violations("tests/planted.rs", &planted),
        vec!["tests/planted.rs:2"]
    );
    assert!(violations(HELPER, &planted).is_empty());
}

/// Why: this binary's own `$HOME` is the sandbox, not the real home, and no
/// path-override variable in `common::SANDBOX_SCRUB` survives to redirect it.
/// Test: itself.
#[test]
fn this_test_binary_runs_under_the_sandbox() {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    assert!(
        home.starts_with(std::env::temp_dir()) && home.is_dir(),
        "HOME is not a temp dir: {}",
        home.display()
    );
    let set: Vec<&str> = common::SANDBOX_SCRUB
        .iter()
        .copied()
        .filter(|key| std::env::var_os(key).is_some())
        .collect();
    assert!(set.is_empty(), "path-override variables still set: {set:?}");
}
