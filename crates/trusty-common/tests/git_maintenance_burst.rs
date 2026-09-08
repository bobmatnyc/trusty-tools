//! Integration test: a burst of concurrent git operations through
//! `trusty_common::git` triggers zero `git maintenance run --auto` spawns —
//! the #7171 issue closure criterion, not just correct argv (the unit tests
//! in `src/git.rs` already cover that).
//!
//! Why: the incident was 41 detached `git maintenance run --auto` repacks
//! against one shared object store from ~25 worktrees. Nothing before this
//! file drove real concurrent git traffic through the builder and checked
//! what git itself decided to do.
//! What: builds a bare origin, a `base` clone, and 2 linked worktrees sharing
//! `base`'s object store, then runs a burst of 10 concurrent fetch+commit
//! task sets through `trusty_common::git::command_in`, each with `GIT_TRACE`
//! captured to its own file. Asserts none of the 10 traces contain git's own
//! `"maintenance run"` invocation line — the reliable signal, since it is
//! git's own record of what it decided to run, not a `ps` snapshot that can
//! miss a child that has already exited. A concurrent `ps` poll for the
//! burst's duration is a second, best-effort (racy) corroboration, and the
//! shared object store is checked afterward for a `tmp_pack_*` or
//! `maintenance.lock` repack artifact.
//! Test: this file IS the test —
//! `burst_of_concurrent_ops_spawns_no_maintenance_run`.
//!
//! Manually verified the OPPOSITE once, by hand: swapping
//! `trusty_common::git::command_in` in [`fetch_and_commit_via_builder`] for a
//! bare `Command::new("git").arg("-C").arg(dir)` and re-running this exact
//! test made ALL 10 trace files contain `"maintenance run"` and the test
//! failed with `traces show a spawn attempt: [10 paths]` — see the PR body
//! for the pasted failure. Confirms independently what a standalone
//! `GIT_TRACE=1 git fetch origin` also showed against this fixture's shape
//! (`maintenance.auto` forced `true`, no `-c` override): git 2.54.0 prints
//! `trace: run_command: git maintenance run --auto --no-quiet --detach`. That
//! bare-`Command::new` variant is deliberately NOT part of this committed
//! suite — proving the negative once is enough; the suite itself only
//! exercises the fix.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// Run a plain (unbuilt) `git -C <dir> <args>` for FIXTURE SETUP only — never
/// for anything this test asserts about. Returns `false` on any failure.
fn git_ok(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A bare origin, a `base` clone with one commit pushed, and `worktree_count`
/// linked worktrees off `base` sharing its object store — the shape of the
/// #7171 incident (many worktrees, one shared `objects/`).
struct Fixture {
    root: tempfile::TempDir,
    base: PathBuf,
    worktrees: Vec<PathBuf>,
}

/// Build the fixture, or `None` when `git` is unavailable on this runner —
/// mirrors the established pattern in
/// `session_manager::decommission_worktree_tests` (trusty-mpm).
fn build_fixture(worktree_count: usize) -> Option<Fixture> {
    let root = tempfile::tempdir().ok()?;
    let origin = root.path().join("origin.git");
    let base = root.path().join("base");
    if !git_ok(root.path(), &["init", "--bare", "-q", origin.to_str()?]) {
        return None;
    }
    if !Command::new("git")
        .args(["clone", "-q"])
        .arg(&origin)
        .arg(&base)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return None;
    }
    git_ok(&base, &["config", "user.email", "t@example.invalid"]);
    git_ok(&base, &["config", "user.name", "T"]);
    std::fs::write(base.join("f.txt"), "seed\n").ok()?;
    git_ok(&base, &["add", "f.txt"]);
    git_ok(&base, &["commit", "-q", "-m", "seed"]);
    git_ok(&base, &["push", "-q", "origin", "HEAD:main"]);

    let mut worktrees = Vec::new();
    for i in 0..worktree_count {
        let wt = root.path().join(format!("wt-{i}"));
        let branch = format!("wt-{i}");
        let ok = git_ok(
            &base,
            &["worktree", "add", "-q", "-b", &branch, wt.to_str()?, "HEAD"],
        );
        if !ok {
            return None;
        }
        git_ok(&wt, &["config", "user.email", "t@example.invalid"]);
        git_ok(&wt, &["config", "user.name", "T"]);
        worktrees.push(wt);
    }
    Some(Fixture {
        root,
        base,
        worktrees,
    })
}

/// One fetch+commit task, entirely through the builder under test, with
/// `GIT_TRACE` pointed at `trace_path` so the assertion can read what git
/// itself decided to run.
fn fetch_and_commit_via_builder(worktree: &Path, index: usize, trace_path: &Path) {
    let file = worktree.join(format!("burst-{index}.txt"));
    let _ = std::fs::write(&file, format!("{index}\n"));
    let _ = trusty_common::git::command_in(worktree)
        .env("GIT_TRACE", trace_path)
        .args(["add", "--"])
        .arg(&file)
        .output();
    let _ = trusty_common::git::command_in(worktree)
        .env("GIT_TRACE", trace_path)
        .args(["commit", "-q", "-m"])
        .arg(format!("burst {index}"))
        .output();
    let _ = trusty_common::git::command_in(worktree)
        .env("GIT_TRACE", trace_path)
        .args(["fetch", "origin"])
        .output();
}

#[test]
fn burst_of_concurrent_ops_spawns_no_maintenance_run() {
    const WORKTREES: usize = 2;
    const BURST_SIZE: usize = 10;

    let Some(fixture) = build_fixture(WORKTREES) else {
        return; // no git on this runner
    };
    // Force the pre-#7171 default ON, locally, so this test is meaningful
    // regardless of the runner's own ambient config — some operator machines
    // already carry a manual `maintenance.auto=false` mitigation, which would
    // make the fix look like it worked even with the bug present.
    assert!(git_ok(
        &fixture.base,
        &["config", "--local", "maintenance.auto", "true"]
    ));
    assert!(git_ok(
        &fixture.base,
        &["config", "--local", "gc.auto", "1"]
    ));

    // Best-effort corroboration: poll `ps` for a `git maintenance` line
    // naming this fixture's own tempdir path, for the whole burst window.
    // Racy by construction (a spawned-but-idle child can exit between
    // polls) — the GIT_TRACE assertion below is the reliable half.
    let seen_in_ps = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let root_str = fixture.root.path().to_string_lossy().into_owned();
    let watcher = {
        let seen_in_ps = Arc::clone(&seen_in_ps);
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if let Ok(out) = Command::new("ps").args(["-A", "-o", "command="]).output() {
                    let text = String::from_utf8_lossy(&out.stdout);
                    if text
                        .lines()
                        .any(|l| l.contains("maintenance run") && l.contains(&root_str))
                    {
                        seen_in_ps.store(true, Ordering::Relaxed);
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
        })
    };

    let trace_dir = fixture.root.path().join("traces");
    std::fs::create_dir_all(&trace_dir).expect("mkdir traces");

    let start = Instant::now();
    let handles: Vec<_> = (0..BURST_SIZE)
        .map(|i| {
            let worktree = fixture.worktrees[i % fixture.worktrees.len()].clone();
            let trace_path = trace_dir.join(format!("trace-{i}.log"));
            thread::spawn(move || {
                fetch_and_commit_via_builder(&worktree, i, &trace_path);
                trace_path
            })
        })
        .collect();
    let trace_paths: Vec<PathBuf> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let elapsed = start.elapsed();

    // Give the watcher a moment to catch anything still finishing, then stop.
    thread::sleep(Duration::from_millis(200));
    stop.store(true, Ordering::Relaxed);
    watcher.join().expect("ps watcher thread panicked");

    assert!(
        elapsed < Duration::from_secs(25),
        "burst took {elapsed:?} — too slow for a normal (non-#[ignore]) test"
    );

    // The reliable assertion: git's own trace for every one of the 10 task
    // sets must never show it deciding to run background maintenance.
    let mut offending = Vec::new();
    for path in &trace_paths {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue; // GIT_TRACE writes nothing when there was nothing to trace
        };
        if text.contains("maintenance run") {
            offending.push(path.display().to_string());
        }
    }
    assert!(
        offending.is_empty(),
        "trusty_common::git::command_in's -c maintenance.auto=false failed to suppress \
         background maintenance — traces show a spawn attempt: {offending:?}"
    );

    assert!(
        !seen_in_ps.load(Ordering::Relaxed),
        "ps observed a live `git maintenance` process for this fixture during the burst"
    );

    // No repack-in-progress artifact left behind in the shared object store.
    let objects_dir = fixture.base.join(".git").join("objects");
    if let Ok(entries) = std::fs::read_dir(&objects_dir) {
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            assert!(
                !name.starts_with("tmp_pack_") && name != "maintenance.lock",
                "shared object store carries a repack artifact after the burst: {name}"
            );
        }
    }
}
