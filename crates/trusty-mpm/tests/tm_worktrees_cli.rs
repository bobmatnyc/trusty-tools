//! `tm worktrees --json` through the built binary on a scratch HOME (#8994).
//!
//! Why: criterion 4 of the slice — the report's count and GiB per project are
//! asserted on the real binary's output, not on an in-process struct.
//! What: seeds `<home>/.trusty-mpm/worktrees.jsonl` and runs `tm worktrees`
//! with `HOME` pointed at the scratch directory; no registry exists, so the
//! backfill appends nothing.
//! Test: `cargo test -p trusty-mpm --test integration tm_worktrees_cli::`.

use std::path::{Path, PathBuf};

use serde_json::Value;
use trusty_mpm::core::worktree_ledger::fold::GIB;
use trusty_mpm::core::worktree_ledger::{EventKind, LedgerEvent, Origin, WorktreeLedger};

use crate::common;

fn created(path: &Path, repo: &str) -> LedgerEvent {
    LedgerEvent::now(
        path.to_path_buf(),
        EventKind::Created {
            repo: PathBuf::from(repo),
            branch: None,
            origin: Origin::TmDaemon,
            session: None,
        },
    )
}

fn worktrees_json(home: &Path, extra: &[&str]) -> Value {
    let out = common::tm_command_in(home)
        .arg("worktrees")
        .arg("--json")
        .args(extra)
        .output()
        .expect("spawn tm");
    assert!(
        out.status.success(),
        "tm worktrees failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("JSON report")
}

#[test]
fn tm_worktrees_json_reports_count_and_gib_per_project() {
    let home = tempfile::tempdir().unwrap();
    let ledger = WorktreeLedger::under_home(home.path());
    let gib = GIB as u64;
    let a = Path::new("/nonexistent-8994/alpha/.worktrees/a");
    let b = Path::new("/nonexistent-8994/alpha/.worktrees/b");
    let c = Path::new("/nonexistent-8994/beta/.worktrees/c");
    for e in [
        created(a, "/nonexistent-8994/alpha"),
        created(b, "/nonexistent-8994/alpha"),
        created(c, "/nonexistent-8994/beta"),
        LedgerEvent::now(a.to_path_buf(), EventKind::Measured { bytes: 2 * gib }),
        LedgerEvent::now(b.to_path_buf(), EventKind::Measured { bytes: gib / 4 }),
        LedgerEvent::now(c.to_path_buf(), EventKind::Measured { bytes: gib }),
        LedgerEvent::now(c.to_path_buf(), EventKind::Removed),
    ] {
        ledger.append(&e).unwrap();
    }

    let report = worktrees_json(home.path(), &["--no-size"]);

    let projects = report["projects"].as_array().expect("projects");
    assert_eq!(
        projects.len(),
        1,
        "beta's only tree was removed: {report:#}"
    );
    assert_eq!(projects[0]["repo"], "/nonexistent-8994/alpha");
    assert_eq!(projects[0]["count"], 2);
    assert_eq!(projects[0]["gib"], 2.25);
    assert_eq!(report["total"]["count"], 2);
    assert_eq!(report["total"]["gib"], 2.25);
    assert!(
        report["measured"].is_null(),
        "--no-size measures nothing: {report:#}"
    );
}

#[test]
fn tm_worktrees_measures_a_live_tree_unless_no_size() {
    let home = tempfile::tempdir().unwrap();
    let tree = home.path().join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::write(tree.join("blob"), vec![1u8; 256 * 1024]).unwrap();
    let key = std::fs::canonicalize(&tree).unwrap();
    WorktreeLedger::under_home(home.path())
        .append(&created(&key, "/nonexistent-8994/gamma"))
        .unwrap();

    let skipped = worktrees_json(home.path(), &["--no-size"]);
    assert_eq!(skipped["projects"][0]["unmeasured"], 1, "{skipped:#}");

    let measured = worktrees_json(home.path(), &[]);
    assert_eq!(measured["measured"], 1, "{measured:#}");
    assert_eq!(measured["projects"][0]["unmeasured"], 0, "{measured:#}");
    assert!(
        measured["projects"][0]["bytes"].as_u64().unwrap() >= 256 * 1024,
        "{measured:#}"
    );
}

/// Run `git -C <dir> <args>`, panicking with git's stderr on failure.
fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("`git {}` could not run: {e}", args.join(" ")));
    assert!(
        out.status.success(),
        "`git {}` failed in {}: {}",
        args.join(" "),
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A one-commit repository at `<root>/repo` with worktrees `a` and `b`,
/// returned canonicalized as the ledger keys them.
fn repo_with_two_worktrees(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "--initial-branch=main"]);
    git(
        &repo,
        &[
            "-c",
            "user.email=ci@test.invalid",
            "-c",
            "user.name=CI",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ],
    );
    for name in ["a", "b"] {
        let wt = repo.join(".worktrees").join(name);
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                &format!("wt/{name}"),
                wt.to_str().unwrap(),
            ],
        );
    }
    let canon = |p: PathBuf| std::fs::canonicalize(p).unwrap();
    (
        canon(repo.clone()),
        canon(repo.join(".worktrees").join("a")),
        canon(repo.join(".worktrees").join("b")),
    )
}

/// #8994 finding 1: a tree removed from disk AND from git leaves both the
/// count and the GiB, because `tm worktrees` records it `removed`.
#[test]
fn tm_worktrees_records_removed_for_a_tree_gone_from_disk_and_git() {
    let home = tempfile::tempdir().unwrap();
    let (repo, a, b) = repo_with_two_worktrees(home.path());
    let ledger = WorktreeLedger::under_home(home.path());
    let gib = GIB as u64;
    let repo_s = repo.to_str().unwrap();
    for e in [
        created(&a, repo_s),
        created(&b, repo_s),
        LedgerEvent::now(a.clone(), EventKind::Measured { bytes: 2 * gib }),
        LedgerEvent::now(b.clone(), EventKind::Measured { bytes: gib / 4 }),
    ] {
        ledger.append(&e).unwrap();
    }
    let before = worktrees_json(home.path(), &["--no-size"]);
    assert_eq!(before["total"]["count"], 2, "{before:#}");
    assert_eq!(before["total"]["gib"], 2.25, "{before:#}");

    git(
        &repo,
        &["worktree", "remove", "--force", a.to_str().unwrap()],
    );
    assert!(!a.exists(), "git removed the directory");

    let after = worktrees_json(home.path(), &["--no-size"]);
    assert_eq!(after["total"]["count"], 1, "{after:#}");
    assert_eq!(after["total"]["gib"], 0.25, "{after:#}");
    assert_eq!(after["projects"][0]["count"], 1, "{after:#}");
    assert_eq!(after["projects"][0]["gib"], 0.25, "{after:#}");
    let removed: Vec<PathBuf> = ledger
        .read()
        .unwrap()
        .events
        .into_iter()
        .filter(|e| e.kind == EventKind::Removed)
        .map(|e| e.path)
        .collect();
    assert_eq!(removed, vec![a], "exactly one removed event, for `a`");

    let again = worktrees_json(home.path(), &["--no-size"]);
    assert_eq!(again["total"]["count"], 1, "{again:#}");
    let removed_again = ledger
        .read()
        .unwrap()
        .events
        .iter()
        .filter(|e| e.kind == EventKind::Removed)
        .count();
    assert_eq!(removed_again, 1, "a second run appends no second removed");
}
