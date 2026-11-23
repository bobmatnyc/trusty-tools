//! `tm project unadopt` is ledger-only and never reports a failed write (#10592).
//!
//! Why: `tm project init` on a legacy checkout adopted every old worktree into
//! `project-registry/worktrees.json`, and no command took a record back out.
//! What: runs the real binary under a scratch `$HOME` against a seeded ledger
//! and a real worktree folder.
//! Test: `cargo test -p trusty-mpm --test integration project_unadopt_10592`.

use crate::common;

use std::path::{Path, PathBuf};

/// The ledger file the CLI reads under `home`.
fn ledger(home: &Path) -> PathBuf {
    home.join(".trusty-mpm")
        .join("project-registry")
        .join("worktrees.json")
}

/// Seed `home`'s ledger with one record per path; returns the ledger bytes.
fn seed(home: &Path, paths: &[&Path]) -> Vec<u8> {
    let rows: Vec<serde_json::Value> = paths
        .iter()
        .map(|p| {
            serde_json::json!({
                "path": p, "project": "legacy", "checkout": p.parent(),
                "branch": "old", "adopted_at": "2026-10-31T00:58:00Z"
            })
        })
        .collect();
    let file = ledger(home);
    std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
    let bytes = serde_json::to_vec_pretty(&rows).expect("encode");
    std::fs::write(&file, &bytes).expect("write ledger");
    bytes
}

/// Run `tm project unadopt <args>` under `home`; returns (success, stdout, stderr).
fn unadopt(home: &Path, args: &[&str]) -> (bool, String, String) {
    let out = common::tm_command_in(home)
        .args(["--url", "http://127.0.0.1:1", "project", "unadopt"])
        .args(args)
        .output()
        .expect("spawn tm");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The ledger's recorded paths.
fn recorded(home: &Path) -> Vec<String> {
    let raw = std::fs::read(ledger(home)).expect("read ledger");
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&raw).expect("parse ledger");
    rows.iter()
        .map(|r| r["path"].as_str().expect("path").to_string())
        .collect()
}

/// Two real worktree folders, canonical, with a file in the first.
fn trees() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().expect("trees");
    let a = dir.path().join("wt-a");
    let b = dir.path().join("wt-b");
    std::fs::create_dir_all(&a).expect("mkdir a");
    std::fs::create_dir_all(&b).expect("mkdir b");
    std::fs::write(a.join("work.txt"), b"keep me").expect("write");
    let a = std::fs::canonicalize(a).expect("canon a");
    let b = std::fs::canonicalize(b).expect("canon b");
    (dir, a, b)
}

#[test]
fn project_unadopt_removes_the_record_and_keeps_the_folder_10592() {
    let home = tempfile::tempdir().expect("home");
    let (_trees, a, b) = trees();
    seed(home.path(), &[&a, &b]);

    let (ok, stdout, stderr) = unadopt(home.path(), &[a.to_str().expect("utf8")]);

    assert!(ok, "stdout={stdout} stderr={stderr}");
    assert!(stdout.contains("removed"), "{stdout}");
    assert_eq!(recorded(home.path()), vec![b.display().to_string()]);
    assert_eq!(
        std::fs::read(a.join("work.txt")).expect("folder kept"),
        b"keep me"
    );
}

#[test]
fn project_unadopt_dry_run_writes_nothing_10592() {
    let home = tempfile::tempdir().expect("home");
    let (_trees, a, _b) = trees();
    let before = seed(home.path(), &[&a]);

    let (ok, stdout, stderr) = unadopt(home.path(), &["--dry-run", a.to_str().expect("utf8")]);

    assert!(ok, "stdout={stdout} stderr={stderr}");
    assert!(stdout.contains("would remove"), "{stdout}");
    assert_eq!(std::fs::read(ledger(home.path())).expect("read"), before);
}

/// A worktree folder already deleted is still unadopted when named by a
/// relative path or through a symlinked parent (#10592 code-critic finding 1).
#[test]
fn project_unadopt_matches_a_deleted_folder_by_relative_and_symlinked_path_10592() {
    let home = tempfile::tempdir().expect("home");
    let root = tempfile::tempdir().expect("root");
    let real = root.path().join("real");
    std::fs::create_dir_all(real.join("wt-rel")).expect("mkdir rel");
    std::fs::create_dir_all(real.join("wt-link")).expect("mkdir link");
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    let rel = std::fs::canonicalize(real.join("wt-rel")).expect("canon rel");
    let via = std::fs::canonicalize(real.join("wt-link")).expect("canon link");
    seed(home.path(), &[&rel, &via]);
    std::fs::remove_dir(&rel).expect("delete rel");
    std::fs::remove_dir(&via).expect("delete link");

    let out = common::tm_command_in(home.path())
        .current_dir(&link)
        .args(["--url", "http://127.0.0.1:1", "project", "unadopt"])
        .arg("wt-rel")
        .arg(link.join("sub").join("..").join("wt-link"))
        .output()
        .expect("spawn tm");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        recorded(home.path()).is_empty(),
        "both records must leave the ledger: stdout={stdout} stderr={stderr}"
    );
    assert!(out.status.success(), "stdout={stdout} stderr={stderr}");
}

/// A real run naming a path with no record exits non-zero (#10592 finding 2).
#[test]
fn project_unadopt_fails_when_a_path_matches_no_record_10592() {
    let home = tempfile::tempdir().expect("home");
    let (_trees, a, b) = trees();
    seed(home.path(), &[&a]);

    let (ok, stdout, stderr) = unadopt(home.path(), &[b.to_str().expect("utf8")]);

    assert!(!ok, "an unmatched path must fail: stdout={stdout}");
    assert!(stdout.contains("not adopted"), "{stdout}");
    assert!(stderr.contains("matched no ledger record"), "{stderr}");
    assert_eq!(recorded(home.path()), vec![a.display().to_string()]);
}

/// Fail-Open Check: a corrupt ledger fails the command, prints no removal,
/// and is left byte-for-byte.
#[test]
fn project_unadopt_fails_on_a_corrupt_ledger_10592() {
    let home = tempfile::tempdir().expect("home");
    let (_trees, a, _b) = trees();
    seed(home.path(), &[&a]);
    std::fs::write(ledger(home.path()), b"{ not json").expect("corrupt");

    let (ok, stdout, stderr) = unadopt(home.path(), &[a.to_str().expect("utf8")]);

    assert!(!ok, "a corrupt ledger must fail: stdout={stdout}");
    assert!(!stdout.contains("removed"), "{stdout}");
    assert!(stderr.contains("ledger unchanged"), "{stderr}");
    assert_eq!(
        std::fs::read(ledger(home.path())).expect("read"),
        b"{ not json"
    );
}
