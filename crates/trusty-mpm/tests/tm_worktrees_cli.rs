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

/// Seed the ingest-failure sidecar under `home` and return its path.
fn seed_failure(home: &Path, sticky: bool) -> PathBuf {
    let ledger = WorktreeLedger::under_home(home);
    let path = trusty_mpm::core::worktree_ledger::hook_ingest::ingest_failure_path(&ledger);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body = serde_json::json!({
        "ts": "2026-10-16T00:00:00Z",
        "error": "hook ingest could not record `git worktree add` of /x",
        "failures": 1,
        "sticky": sticky,
    });
    std::fs::write(&path, body.to_string()).unwrap();
    path
}

/// #8994 S2b criterion 6: a Bash-recorder failure stays until `tm worktrees`
/// backfills cleanly; a plain S2a record, and a backfill that could not read
/// the registry, leave it alone.
#[test]
fn tm_worktrees_clears_only_a_sticky_failure_and_only_after_a_clean_backfill() {
    let home = tempfile::tempdir().unwrap();
    let plain = seed_failure(home.path(), false);
    worktrees_json(home.path(), &["--no-size"]);
    assert!(plain.exists(), "a non-sticky record is not this command's");

    let sticky = seed_failure(home.path(), true);
    let registry = home
        .path()
        .join(".trusty-mpm/project-registry/projects.json");
    std::fs::create_dir_all(registry.parent().unwrap()).unwrap();
    std::fs::write(&registry, b"{torn").unwrap();
    let report = worktrees_json(home.path(), &["--no-size"]);
    assert!(
        report["backfill"]["registry_error"].is_string(),
        "{report:#}"
    );
    assert!(sticky.exists(), "an unread registry repaired nothing");

    std::fs::remove_file(&registry).unwrap();
    worktrees_json(home.path(), &["--no-size"]);
    assert!(!sticky.exists(), "a clean backfill repairs the missed tree");
}

/// `git` with a commit identity, for fixture commits.
fn git_id(dir: &Path, args: &[&str]) {
    let mut full = vec![
        "-c",
        "user.email=ci@test.invalid",
        "-c",
        "user.name=CI",
        "-c",
        "commit.gpgsign=false",
    ];
    full.extend_from_slice(args);
    git(dir, &full);
}

/// #8994 S2c criteria 1 and 5: `tm worktrees` shows each tree's PR and lists
/// a tree whose branch is gone from the remote, with nothing unpushed, as a
/// stale candidate — and deletes nothing.
#[test]
fn tm_worktrees_shows_the_pr_and_lists_stale_candidates() {
    let home = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(home.path()).unwrap();
    let remote = root.join("remote.git");
    std::fs::create_dir_all(&remote).unwrap();
    git(&remote, &["init", "--bare", "--initial-branch=main"]);
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "--initial-branch=main"]);
    git_id(&repo, &["commit", "--allow-empty", "-m", "init"]);
    git(
        &repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&repo, &["push", "-q", "origin", "main"]);
    let ledger = WorktreeLedger::under_home(home.path());
    let mut trees = Vec::new();
    for name in ["gone", "linked"] {
        let wt = repo.join(".worktrees").join(name);
        let branch = format!("wt/{name}");
        git(
            &repo,
            &["worktree", "add", "-b", &branch, wt.to_str().unwrap()],
        );
        git(&wt, &["push", "-q", "-u", "origin", &branch]);
        let mut e = created(&wt, repo.to_str().unwrap());
        if let EventKind::Created { branch: b, .. } = &mut e.kind {
            *b = Some(branch);
        }
        ledger.append(&e).unwrap();
        trees.push(wt);
    }
    git(&trees[0], &["push", "-q", "origin", "--delete", "wt/gone"]);
    ledger
        .append(&LedgerEvent::now(
            trees[1].clone(),
            EventKind::PrLinked { pr: 31 },
        ))
        .unwrap();

    let report = worktrees_json(home.path(), &["--no-size"]);

    let rows = report["trees"].as_array().expect("trees");
    let row = |p: &Path| {
        rows.iter()
            .find(|r| r["path"] == p.to_str().unwrap())
            .unwrap_or_else(|| panic!("{} listed: {report:#}", p.display()))
    };
    assert_eq!(row(&trees[1])["pr"], 31, "{report:#}");
    assert!(row(&trees[1])["stale"].is_null(), "{report:#}");
    assert_eq!(
        row(&trees[0])["stale"]["reason"],
        "branch_gone",
        "{report:#}"
    );
    let candidates = report["stale_candidates"].as_array().expect("candidates");
    assert_eq!(candidates.len(), 1, "{report:#}");
    assert_eq!(candidates[0]["path"], trees[0].to_str().unwrap());
    assert!(trees.iter().all(|t| t.is_dir()), "nothing is deleted");

    let text = common::tm_command_in(home.path())
        .args(["worktrees", "--no-size"])
        .output()
        .expect("spawn tm");
    let stdout = String::from_utf8_lossy(&text.stdout);
    assert!(stdout.contains("PR #31"), "{stdout}");
    assert!(stdout.contains("stale candidate"), "{stdout}");
}

/// A repository with a bare `origin` and one ledger-recorded tree per name on
/// `wt/<name>`, each pushed and then deleted on the remote (branch gone).
fn gone_branch_trees(root: &Path, home: &Path, names: &[&str]) -> Vec<PathBuf> {
    let remote = root.join("remote.git");
    std::fs::create_dir_all(&remote).unwrap();
    git(&remote, &["init", "--bare", "--initial-branch=main"]);
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "--initial-branch=main"]);
    git_id(&repo, &["commit", "--allow-empty", "-m", "init"]);
    git(
        &repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&repo, &["push", "-q", "origin", "main"]);
    let ledger = WorktreeLedger::under_home(home);
    let mut trees = Vec::new();
    for name in names {
        let wt = repo.join(".worktrees").join(name);
        let branch = format!("wt/{name}");
        git(
            &repo,
            &["worktree", "add", "-b", &branch, wt.to_str().unwrap()],
        );
        git(&wt, &["push", "-q", "-u", "origin", &branch]);
        git(&wt, &["push", "-q", "origin", "--delete", &branch]);
        let mut e = created(&wt, repo.to_str().unwrap());
        if let EventKind::Created { branch: b, .. } = &mut e.kind {
            *b = Some(branch);
        }
        ledger.append(&e).unwrap();
        trees.push(wt);
    }
    trees
}

/// 🔴 #8994 S3a criterion 3 (gate F1): a branch-gone candidate whose tree is
/// detached, or checked out on another branch, is kept by the reclaim plan;
/// one still on its recorded branch is eligible. Nothing is deleted. Fails on
/// 2bcb496dbe, whose report has no `reclaim_plan`.
#[test]
fn tm_worktrees_keeps_a_stale_candidate_whose_head_left_its_branch() {
    let home = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(home.path()).unwrap();
    let trees = gone_branch_trees(&root, home.path(), &["kept-on", "detached", "switched"]);
    git(&trees[1], &["checkout", "-q", "--detach"]);
    git(&trees[2], &["checkout", "-q", "-b", "other-work"]);

    let report = worktrees_json(home.path(), &["--no-size"]);

    let plan = report["reclaim_plan"]
        .as_array()
        .unwrap_or_else(|| panic!("no reclaim_plan in the report: {report:#}"));
    let entry = |p: &Path| {
        plan.iter()
            .find(|r| r["path"] == p.to_str().unwrap())
            .unwrap_or_else(|| panic!("{} planned: {report:#}", p.display()))
    };
    assert_eq!(entry(&trees[0])["action"], "eligible", "{report:#}");
    assert_eq!(entry(&trees[1])["action"], "keep", "{report:#}");
    let detached = entry(&trees[1])["reason"].as_str().unwrap_or_default();
    assert!(detached.contains("detached"), "{report:#}");
    assert_eq!(entry(&trees[2])["action"], "keep", "{report:#}");
    let switched = entry(&trees[2])["reason"].as_str().unwrap_or_default();
    assert!(switched.contains("refs/heads/other-work"), "{report:#}");
    assert!(trees.iter().all(|t| t.is_dir()), "nothing is deleted");
}

/// #8994 S3a criterion 5: `--no-fetch` asks neither the remote nor GitHub, so
/// a branch deleted on the remote is unresolved — not a stale candidate, kept
/// by the plan — and never reported gone.
#[test]
fn tm_worktrees_no_fetch_reports_a_gone_branch_unresolved() {
    let home = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(home.path()).unwrap();
    let trees = gone_branch_trees(&root, home.path(), &["offline"]);

    let report = worktrees_json(home.path(), &["--no-size", "--no-fetch"]);

    let rows = report["trees"].as_array().expect("trees");
    assert_eq!(rows.len(), 1, "{report:#}");
    assert!(rows[0]["stale"].is_null(), "{report:#}");
    let unresolved = rows[0]["unresolved"].to_string();
    assert!(unresolved.contains("--no-fetch"), "{report:#}");
    assert_eq!(
        report["stale_candidates"].as_array().map(Vec::len),
        Some(0),
        "{report:#}"
    );
    let plan = report["reclaim_plan"].as_array().expect("reclaim_plan");
    assert_eq!(plan.len(), 1, "{report:#}");
    assert_eq!(plan[0]["action"], "keep", "{report:#}");
    assert!(trees[0].is_dir());
}
