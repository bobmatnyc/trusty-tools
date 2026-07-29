//! Cross-PROCESS concurrency tests for the `projects.json` registry store.
//!
//! Why: `ProjectStore` is written by several independent `tm` processes (the
//! daemon, `tm project …` CLI invocations, MCP tool calls). Its upsert path is a
//! read-modify-write of the WHOLE file, so two processes that interleave
//! read/read/write/write silently lose one of the two entries with no error
//! surfaced to either caller. An in-process `Mutex`/`RwLock` cannot detect that;
//! only a cross-process test can. These tests therefore spawn REAL child
//! processes (re-exec of this same test binary) rather than threads, so the
//! failure mode they observe is exactly the shipping one.
//! What: two multi-process scenarios — a lost-update test where N children each
//! write a disjoint set of project names and every name must survive, and a
//! crash-safety test where a writing child is SIGKILLed mid-write and the file
//! must still parse with its pre-existing entries intact. The `…_child_…`
//! functions are `#[ignore]`d helpers: they are not tests in their own right,
//! they are the child-process entry points re-invoked via
//! `<test-binary> --ignored --exact <name>`.
//! Test: `projects_json_multiprocess_upsert_no_lost_updates`,
//! `projects_json_survives_killed_writer`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use trusty_mpm::project::record::Project;
use trusty_mpm::project::store::ProjectStore;

/// Data directory the child should point its `ProjectStore` at.
const DIR_ENV: &str = "TM_PROJECTS_RACE_DIR";
/// Per-child name prefix, so each child writes a disjoint key set.
const TAG_ENV: &str = "TM_PROJECTS_RACE_TAG";

/// Number of concurrent child writer processes.
const CHILDREN: usize = 4;
/// Upserts performed by each child.
const WRITES_PER_CHILD: usize = 24;

/// Longest we will wait for children to line up on the start barrier or exit.
const CHILD_TIMEOUT: Duration = Duration::from_secs(60);

/// Build a minimal valid [`Project`] with the given name.
fn project(name: &str) -> Project {
    Project {
        name: name.to_string(),
        repo_url: format!("https://github.com/owner/{name}"),
        default_branch: "main".to_string(),
        stack_hint: None,
        tags: vec![],
        description: None,
        gh_user: None,
        gh_account: None,
        github: None,
        commit_name: None,
        commit_email: None,
        worktree: None,
    }
}

/// Read the data directory / tag a child was launched with, or `None` when this
/// process is not acting as a child (so a bare `--ignored` run is a clean no-op).
fn child_env() -> Option<(PathBuf, String)> {
    let dir = std::env::var(DIR_ENV).ok()?;
    let tag = std::env::var(TAG_ENV).ok()?;
    Some((PathBuf::from(dir), tag))
}

/// Spawn one child process running the named `#[ignore]`d helper test.
fn spawn_child(helper: &str, dir: &Path, tag: &str) -> Child {
    Command::new(std::env::current_exe().expect("current_exe"))
        .args(["--ignored", "--exact", "--nocapture", helper])
        .env(DIR_ENV, dir)
        .env(TAG_ENV, tag)
        .spawn()
        .unwrap_or_else(|e| panic!("spawn child {helper}: {e}"))
}

/// Block until `pred` holds or [`CHILD_TIMEOUT`] elapses.
fn wait_until(what: &str, mut pred: impl FnMut() -> bool) {
    let deadline = Instant::now() + CHILD_TIMEOUT;
    while Instant::now() < deadline {
        if pred() {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("timed out waiting for {what}");
}

/// A single-threaded tokio runtime for driving the async store API.
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build runtime")
}

/// Every child announces readiness here, then blocks on `go`.
fn ready_marker(dir: &Path, tag: &str) -> PathBuf {
    dir.join(format!("ready-{tag}"))
}

/// The start barrier: children spin until the parent creates this file.
fn go_marker(dir: &Path) -> PathBuf {
    dir.join("go")
}

/// Child entry point: upsert [`WRITES_PER_CHILD`] distinct projects.
///
/// Why: each child holds ONE long-lived `ProjectStore` across all its writes,
/// which is exactly how the daemon uses it — so the read-modify-write window
/// under test is the shipping one, not a synthetic one.
/// What: signals readiness, waits on the shared start barrier so all children
/// overlap, then upserts `<tag>-00 … <tag>-23`, yielding briefly between writes
/// to widen the reload→save window across processes.
/// Test: driven by `projects_json_multiprocess_upsert_no_lost_updates`.
#[test]
#[ignore = "child-process helper, driven by projects_json_multiprocess_upsert_no_lost_updates"]
fn multiprocess_child_writer() {
    let Some((dir, tag)) = child_env() else {
        return;
    };
    std::fs::write(ready_marker(&dir, &tag), b"ready").expect("write ready marker");
    let go = go_marker(&dir);
    wait_until("start barrier", || go.exists());

    runtime().block_on(async {
        let mut store = ProjectStore::load(&dir).await.expect("child: load store");
        for i in 0..WRITES_PER_CHILD {
            store
                .upsert(project(&format!("{tag}-{i:02}")))
                .await
                .expect("child: upsert");
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    });
}

/// Child entry point: upsert in a tight loop forever, until killed.
///
/// Why: the crash-safety test needs a writer that is guaranteed to be mid-write
/// when SIGKILL lands; an unbounded loop makes that certain without timing luck.
/// What: signals readiness, then upserts `<tag>-<n>` without pause until the
/// parent kills the process.
/// Test: driven by `projects_json_survives_killed_writer`.
#[test]
#[ignore = "child-process helper, driven by projects_json_survives_killed_writer"]
fn multiprocess_child_hammer() {
    let Some((dir, tag)) = child_env() else {
        return;
    };
    std::fs::write(ready_marker(&dir, &tag), b"ready").expect("write ready marker");

    runtime().block_on(async {
        let mut store = ProjectStore::load(&dir).await.expect("child: load store");
        let mut n: u64 = 0;
        loop {
            store
                .upsert(project(&format!("{tag}-{n}")))
                .await
                .expect("child: upsert");
            n += 1;
        }
    });
}

/// Concurrent writer PROCESSES must not lose each other's `projects.json` entries.
///
/// Why: this is the lost-update defect. `ProjectStore::upsert` reloads the file,
/// mutates its in-memory copy and writes the whole file back. With no
/// cross-process serialisation, two `tm` processes interleaving
/// read/read/write/write drop one entry entirely — and BOTH callers see `Ok`.
/// What: launches [`CHILDREN`] child processes that all release from one barrier
/// and each write [`WRITES_PER_CHILD`] disjoint project names; asserts every one
/// of the `CHILDREN * WRITES_PER_CHILD` names is present afterwards. Fails
/// against the unsynchronised store; passes once the upsert path is guarded.
/// Test: this IS the test.
#[test]
fn projects_json_multiprocess_upsert_no_lost_updates() {
    let dir = TempDir::new().expect("tempdir");
    let tags: Vec<String> = (0..CHILDREN).map(|c| format!("p{c}")).collect();

    let mut children: Vec<Child> = tags
        .iter()
        .map(|tag| spawn_child("multiprocess_child_writer", dir.path(), tag))
        .collect();

    // Real barrier: only release once every child is spun up and polling, so
    // process-startup skew cannot serialise the writers by accident.
    wait_until("all children ready", || {
        tags.iter().all(|t| ready_marker(dir.path(), t).exists())
    });
    std::fs::write(go_marker(dir.path()), b"go").expect("release barrier");

    for (child, tag) in children.iter_mut().zip(&tags) {
        let status = child.wait().expect("wait for child");
        assert!(status.success(), "child {tag} failed: {status}");
    }

    let all = runtime().block_on(async {
        let mut store = ProjectStore::load(dir.path()).await.expect("parent: load");
        store.all().await.expect("parent: all")
    });

    let mut missing: Vec<String> = Vec::new();
    for tag in &tags {
        for i in 0..WRITES_PER_CHILD {
            let name = format!("{tag}-{i:02}");
            if !all.iter().any(|p| p.name == name) {
                missing.push(name);
            }
        }
    }
    assert!(
        missing.is_empty(),
        "lost {} of {} concurrent upserts (file holds {}): {:?}",
        missing.len(),
        CHILDREN * WRITES_PER_CHILD,
        all.len(),
        missing
    );
}

/// A writer killed mid-write must never leave `projects.json` truncated,
/// half-written, or emptied.
///
/// Why: the store publishes by writing a temp file and renaming it over the
/// real path. If concurrent writers share one temp path, or the rename is not
/// the only publish step, a crash can expose a partial file — turning a lost
/// update into total registry loss. An existing valid file must survive any
/// crash point.
/// What: seeds a known project, spawns a child that upserts in a tight loop,
/// SIGKILLs it once it is provably running, then asserts `projects.json` still
/// parses, still contains the seeded record, and is not empty.
/// Test: this IS the test.
#[test]
fn projects_json_survives_killed_writer() {
    let dir = TempDir::new().expect("tempdir");
    let rt = runtime();

    rt.block_on(async {
        let mut store = ProjectStore::load(dir.path()).await.expect("seed load");
        store.upsert(project("seeded")).await.expect("seed upsert");
    });

    let mut child = spawn_child("multiprocess_child_hammer", dir.path(), "hammer");
    wait_until("hammer ready", || {
        ready_marker(dir.path(), "hammer").exists()
    });
    // Let it get well into its write loop, then kill it at an arbitrary point.
    std::thread::sleep(Duration::from_millis(250));
    child.kill().expect("kill hammer child");
    let _ = child.wait();

    // The file must still be readable as valid JSON and retain the seed.
    let all = rt.block_on(async {
        let mut store = ProjectStore::load(dir.path())
            .await
            .expect("projects.json must still parse after a killed writer");
        store.all().await.expect("all after killed writer")
    });
    assert!(
        all.iter().any(|p| p.name == "seeded"),
        "a killed writer destroyed the pre-existing entry; file holds {} record(s)",
        all.len()
    );
}
