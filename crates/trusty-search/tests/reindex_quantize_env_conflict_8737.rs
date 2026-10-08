//! End-to-end regression for issue #8737: `reindex`, `quantize` and
//! `index relocate` must never act on a target read from `TRUSTY_INDEX` alone,
//! and `reindex` must never let `TRUSTY_INDEX` (or `-i`) re-point an index at
//! an explicit PATH that belongs to a different index.
//!
//! Why: before the fix all three took their target purely from `cli.index`,
//! which clap folds together with `TRUSTY_INDEX`. `reindex <PATH>` then POSTed
//! `{root_path: PATH}` to the env-named index — rebasing a live index onto an
//! unrelated root and overwriting its corpus — the same class as #8175.
//! What: serves the real socket router with two bare, resident indexes (no
//! embedder, no walk) behind a recording proxy (#9214: the CLI reaches the
//! daemon over its socket only). The proxy logs every method and its params,
//! optionally refuses `search.index.status`, and refuses every reindex,
//! quantize and relocate so the real handlers never run. The compiled
//! `trusty-search` binary is spawned against it with `TRUSTY_SEARCH_SOCKET`,
//! `TRUSTY_DATA_DIR`, `HOME` and `XDG_CONFIG_HOME` pinned to tempdirs, the
//! inherited `TRUSTY_INDEX` removed, and a scratch working directory.
//! Assertions read the call log, not just the exit code.
//!
//! Test: `cargo test -p trusty-search --test integration reindex_quantize_env_conflict_8737::`

use crate::test_daemon;
use std::path::{Path, PathBuf};

use std::process::Command;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::RwLock;
use trusty_common::uds::server::RpcError;

use trusty_search::core::indexer::CodeIndexer;
use trusty_search::core::registry::{IndexHandle, IndexId, IndexRegistry};
use trusty_search::service::rpc::error::{CODE_NOT_FOUND, CODE_UNAVAILABLE};
use trusty_search::service::server::SearchAppState;

use crate::real_allowlist_guard;
use crate::socket_daemon::{serve_logged, LoggedDaemon};
use real_allowlist_guard::RealAllowlistGuard;

const INDEX_A: &str = "idx-a-8737";
const INDEX_B: &str = "idx-b-8737";

/// The methods that rebuild, re-quantize or relocate an index; the proxy
/// answers each with a refusal so the real handlers never run.
const BLOCKED: &[&str] = &[
    "search.index.reindex",
    "search.index.quantize",
    "search.index.relocate",
];

/// Serve the real socket router with indexes A and B behind a recording
/// proxy. `status_chaos` makes `search.index.status` refuse with that code —
/// the call the PATH-vs-id agreement check needs — for every index, or only
/// for the one id given.
async fn spawn_daemon(
    root_a: &Path,
    root_b: &Path,
    status_chaos: Option<(i64, Option<&'static str>)>,
) -> LoggedDaemon {
    let registry = IndexRegistry::new();
    for (id, root) in [(INDEX_A, root_a), (INDEX_B, root_b)] {
        let indexer = CodeIndexer::new(id, root.to_string_lossy().into_owned());
        registry.register(IndexHandle::bare(
            IndexId::new(id),
            Arc::new(RwLock::new(indexer)),
            root.to_path_buf(),
        ));
    }
    serve_logged(
        Arc::new(SearchAppState::new(registry)),
        move |method, params: &Value| {
            if BLOCKED.contains(&method) {
                return Some(Err(RpcError::internal("fixture_blocks_mutation")));
            }
            let (code, only_id) = status_chaos?;
            let hit = method == "search.index.status"
                && only_id.is_none_or(|id| params["index_id"] == id);
            hit.then(|| Err(RpcError::new(code, "chaos_status_test")))
        },
    )
    .await
}

/// Per-test scratch state: two index roots, the data dir the CLI runs
/// against, a fake home, and a working directory outside any repo.
struct Scratch {
    root_a: tempfile::TempDir,
    root_b: tempfile::TempDir,
    data_dir: tempfile::TempDir,
    home: tempfile::TempDir,
    cwd: tempfile::TempDir,
}

impl Scratch {
    fn new() -> Self {
        let t = || tempfile::tempdir().expect("tempdir");
        Self {
            root_a: t(),
            root_b: t(),
            data_dir: t(),
            home: t(),
            cwd: t(),
        }
    }

    /// `trusty-search <args>` against the daemon on `socket`, with every path
    /// the CLI could write pinned to a tempdir and no inherited `TRUSTY_INDEX`.
    fn command(&self, socket: &Path, args: &[&str]) -> Command {
        let mut cmd = test_daemon::command();
        cmd.args(args)
            .env("TRUSTY_DATA_DIR", self.data_dir.path())
            .env("TRUSTY_SEARCH_SOCKET", socket)
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path())
            .env_remove("TRUSTY_DATA_DIR_OVERRIDE")
            .env_remove("TRUSTY_INDEX")
            .current_dir(self.cwd.path());
        cmd
    }
}

// #8900: stamped by `test_daemon::command` and bounded here, so a daemon the
// CLI auto-starts can neither outlive the run nor hold this call open.
fn run(mut cmd: Command) -> (i32, String) {
    let out = test_daemon::run_bounded(&mut cmd, std::time::Duration::from_secs(120));
    (out.code.unwrap_or(-1), out.combined)
}

/// Assert an env-only invocation refused with NO call at all.
fn assert_env_only_refusal(code: i32, output: &str, daemon: &LoggedDaemon) {
    assert_ne!(code, 0, "env-only target must refuse; output:\n{output}");
    assert!(
        output.contains(INDEX_B) && output.contains("TRUSTY_INDEX"),
        "the refusal must name the id and its source; output:\n{output}"
    );
    assert!(
        daemon.snapshot().is_empty(),
        "an env-only refusal happens before any call: {:?}",
        daemon.snapshot()
    );
}

/// Every reindex the CLI sent, as `(index_id, root_path)`.
fn reindex_posts(daemon: &LoggedDaemon) -> Vec<(String, String)> {
    daemon
        .mutations()
        .into_iter()
        .map(|(method, params)| {
            assert_eq!(method, "search.index.reindex", "{params}");
            (
                params["index_id"].as_str().unwrap_or_default().to_string(),
                params["body"]["root_path"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// A socket path nothing serves, for the daemon-down arm.
fn absent_socket(s: &Scratch) -> PathBuf {
    s.data_dir.path().join("absent.sock")
}

// `multi_thread` everywhere: the blocking `Command::output()` holds one worker
// while the socket servers need another to keep accepting connections.

/// Core regression: PATH names A, `TRUSTY_INDEX` names B. Pre-fix, `reindex`
/// sent a reindex of B with `{root_path: A}` — rebasing B onto A's root.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reindex_path_a_with_env_b_refuses_and_sends_no_reindex() {
    let s = Scratch::new();
    let guard = RealAllowlistGuard::capture();
    let daemon = spawn_daemon(s.root_a.path(), s.root_b.path(), None).await;

    let mut cmd = s.command(&daemon.socket, &["reindex"]);
    cmd.arg(s.root_a.path()).env("TRUSTY_INDEX", INDEX_B);
    let (code, output) = run(cmd);

    assert_ne!(
        code, 0,
        "PATH=A vs TRUSTY_INDEX=B must refuse; output:\n{output}"
    );
    assert!(
        output.contains(INDEX_A) && output.contains(INDEX_B),
        "the refusal must name both values; output:\n{output}"
    );
    assert!(
        daemon.mutations().is_empty(),
        "no reindex may be sent on a mismatch: {:?}",
        daemon.mutations()
    );
    guard.assert_unchanged("reindex_path_a_with_env_b_refuses_and_sends_no_reindex");
}

/// `reindex` with only `TRUSTY_INDEX` refuses before any request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reindex_env_only_refuses_with_no_requests() {
    let s = Scratch::new();
    let guard = RealAllowlistGuard::capture();
    let daemon = spawn_daemon(s.root_a.path(), s.root_b.path(), None).await;

    let mut cmd = s.command(&daemon.socket, &["reindex"]);
    cmd.env("TRUSTY_INDEX", INDEX_B);
    let (code, output) = run(cmd);

    assert_env_only_refusal(code, &output, &daemon);
    guard.assert_unchanged("reindex_env_only_refuses_with_no_requests");
}

/// `quantize` with only `TRUSTY_INDEX` refuses before any request — pre-fix it
/// sent the dry-run and then the real conversion to the env-named index.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quantize_env_only_refuses_with_no_requests() {
    let s = Scratch::new();
    let guard = RealAllowlistGuard::capture();
    let daemon = spawn_daemon(s.root_a.path(), s.root_b.path(), None).await;

    let mut cmd = s.command(&daemon.socket, &["quantize", "--to", "f16", "--yes"]);
    cmd.env("TRUSTY_INDEX", INDEX_B);
    let (code, output) = run(cmd);

    assert_env_only_refusal(code, &output, &daemon);
    guard.assert_unchanged("quantize_env_only_refuses_with_no_requests");
}

/// `index relocate` rewrites an index's root, so an env-only target refuses
/// before any request (and before it approves the destination).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relocate_env_only_refuses_with_no_requests() {
    let s = Scratch::new();
    let guard = RealAllowlistGuard::capture();
    let daemon = spawn_daemon(s.root_a.path(), s.root_b.path(), None).await;

    let mut cmd = s.command(&daemon.socket, &["index", "relocate", "--to"]);
    cmd.arg(s.root_a.path()).env("TRUSTY_INDEX", INDEX_B);
    let (code, output) = run(cmd);

    assert_env_only_refusal(code, &output, &daemon);
    guard.assert_unchanged("relocate_env_only_refuses_with_no_requests");
}

/// A real `--index A` from an unrelated working directory reindexes A at its
/// REGISTERED root. Pre-fix the root came from CWD detection, rebasing A.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reindex_flag_alone_targets_the_registered_root() {
    let s = Scratch::new();
    let guard = RealAllowlistGuard::capture();
    let daemon = spawn_daemon(s.root_a.path(), s.root_b.path(), None).await;

    let (_code, output) = run(s.command(&daemon.socket, &["reindex", "--index", INDEX_A]));

    let expected = (
        INDEX_A.to_string(),
        s.root_a.path().to_string_lossy().into_owned(),
    );
    assert_eq!(
        reindex_posts(&daemon),
        vec![expected],
        "exactly one reindex, of A at A's registered root; output:\n{output}"
    );
    guard.assert_unchanged("reindex_flag_alone_targets_the_registered_root");
}

/// PATH alone targets the index registered at PATH, not the CWD's; PATH plus
/// an agreeing `TRUSTY_INDEX` proceeds to the same index.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reindex_path_alone_or_agreeing_targets_the_path_index() {
    let s = Scratch::new();
    let guard = RealAllowlistGuard::capture();
    let daemon = spawn_daemon(s.root_a.path(), s.root_b.path(), None).await;

    let mut path_only = s.command(&daemon.socket, &["reindex"]);
    path_only.arg(s.root_a.path());
    let (_, out_path_only) = run(path_only);
    let mut agreeing = s.command(&daemon.socket, &["reindex"]);
    agreeing.arg(s.root_a.path()).env("TRUSTY_INDEX", INDEX_A);
    let (_, out_agreeing) = run(agreeing);

    let expected = (
        INDEX_A.to_string(),
        s.root_a.path().to_string_lossy().into_owned(),
    );
    assert_eq!(
        reindex_posts(&daemon),
        vec![expected.clone(), expected],
        "both runs must reindex A at A's root; outputs:\n{out_path_only}\n{out_agreeing}"
    );
    guard.assert_unchanged("reindex_path_alone_or_agreeing_targets_the_path_index");
}

// ─── Error arms: the agreement check cannot run ────────────────────────────

/// PATH A and `--index A` AGREE, but every status lookup refuses
/// (unavailable or not found), so agreement cannot be confirmed: refuse, with
/// no reindex sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reindex_path_and_flag_when_status_fails_sends_no_reindex() {
    for code in [CODE_UNAVAILABLE, CODE_NOT_FOUND] {
        let s = Scratch::new();
        let guard = RealAllowlistGuard::capture();
        let daemon = spawn_daemon(s.root_a.path(), s.root_b.path(), Some((code, None))).await;

        let mut cmd = s.command(&daemon.socket, &["reindex", "--index", INDEX_A]);
        cmd.arg(s.root_a.path());
        let (exit, output) = run(cmd);

        assert_ne!(
            exit, 0,
            "{code}: unconfirmed agreement must refuse; output:\n{output}"
        );
        assert!(
            daemon
                .snapshot()
                .iter()
                .any(|(m, _)| m == "search.index.status"),
            "{code}: the agreement check must have been attempted: {:?}",
            daemon.snapshot()
        );
        assert!(
            daemon.mutations().is_empty(),
            "{code}: no reindex may be sent: {:?}",
            daemon.mutations()
        );
        guard.assert_unchanged("reindex_path_and_flag_when_status_fails_sends_no_reindex");
    }
}

/// #8737 review: A and B share one root and only A's status is refused.
/// Pre-fix, `find_index_by_path` skipped A and matched B by root path, so
/// both `reindex <root>` and `reindex <root> --index B` POSTed a reindex to B.
/// The lookup must instead fail closed, naming the unreadable A.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reindex_path_refuses_when_a_same_root_index_status_503s() {
    for with_flag in [false, true] {
        let s = Scratch::new();
        let guard = RealAllowlistGuard::capture();
        let chaos = Some((CODE_UNAVAILABLE, Some(INDEX_A)));
        let daemon = spawn_daemon(s.root_a.path(), s.root_a.path(), chaos).await;

        let mut cmd = s.command(&daemon.socket, &["reindex"]);
        cmd.arg(s.root_a.path());
        if with_flag {
            cmd.args(["--index", INDEX_B]);
        }
        let (exit, output) = run(cmd);

        assert_ne!(
            exit, 0,
            "flag={with_flag}: an unreadable same-root index must refuse; output:\n{output}"
        );
        assert!(
            output.contains(&format!("\"{INDEX_A}\"")),
            "flag={with_flag}: the refusal must name the unreadable index; output:\n{output}"
        );
        assert!(
            daemon.mutations().is_empty(),
            "flag={with_flag}: no reindex may be sent: {:?}",
            daemon.mutations()
        );
        guard.assert_unchanged("reindex_path_refuses_when_a_same_root_index_status_503s");
    }
}

/// PATH and `--index` given, daemon down: refuse naming the socket, and the
/// running router — which the CLI is deliberately NOT pointed at — receives
/// nothing.
///
/// A `daemon.lock` naming PID 1 makes the guard wait instead of auto-spawning
/// a real daemon (the `daemon_env_precedence.rs` / #8175 technique); the
/// socket it waits on is one nothing serves.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reindex_path_and_flag_when_daemon_is_down_refuses_with_no_requests() {
    let s = Scratch::new();
    let guard = RealAllowlistGuard::capture();
    let daemon = spawn_daemon(s.root_a.path(), s.root_b.path(), None).await;
    std::fs::write(s.data_dir.path().join("daemon.lock"), b"1").expect("daemon.lock");
    let absent = absent_socket(&s);

    let mut cmd = s.command(&absent, &["reindex", "--index", INDEX_A]);
    cmd.arg(s.root_a.path());
    let (code, output) = run(cmd);

    assert_ne!(code, 0, "a down daemon must refuse; output:\n{output}");
    assert!(
        output.contains(&absent.display().to_string()),
        "the refusal must name the socket; output:\n{output}"
    );
    assert!(
        daemon.snapshot().is_empty(),
        "a down daemon must receive NO call at all: {:?}",
        daemon.snapshot()
    );
    guard.assert_unchanged("reindex_path_and_flag_when_daemon_is_down_refuses_with_no_requests");
}

/// #9214 (#767 rollback): a relocate the daemon refuses withdraws the
/// allowlist approval the CLI granted the new path just before the call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_relocate_withdraws_the_new_paths_approval() {
    let s = Scratch::new();
    let guard = RealAllowlistGuard::capture();
    let daemon = spawn_daemon(s.root_a.path(), s.root_b.path(), None).await;
    // A tempdir is on the hard denylist, which would refuse the approval
    // before the relocate is ever sent; the test process's own HOME is not.
    let parent = dirs::home_dir()
        .expect("HOME")
        .join(".trusty-search-allowlist-tests");
    std::fs::create_dir_all(&parent).expect("approvable parent");
    let dest = tempfile::tempdir_in(&parent).expect("relocate destination");
    let dest_path = std::fs::canonicalize(dest.path()).expect("canonical destination");
    let config = if cfg!(target_os = "macos") {
        s.home.path().join("Library").join("Application Support")
    } else {
        s.home.path().to_path_buf()
    };
    let allowlist = config.join("trusty-search").join("allowlist.toml");

    let mut cmd = s.command(
        &daemon.socket,
        &["index", "relocate", "--index", INDEX_A, "--to"],
    );
    cmd.arg(&dest_path);
    let (code, output) = run(cmd);

    assert_ne!(code, 0, "a refused relocate must fail:\n{output}");
    let relocates: Vec<_> = daemon
        .mutations()
        .into_iter()
        .filter(|(m, _)| m == "search.index.relocate")
        .collect();
    assert_eq!(relocates.len(), 1, "the relocate must be sent: {output}");
    assert!(
        allowlist.exists(),
        "the approval must have been written before the relocate:\n{output}"
    );
    let cfg = trusty_search::allowlist::AllowlistConfig::load_from(&allowlist)
        .expect("load the fake allowlist");
    assert!(
        !cfg.contains(&dest_path),
        "the refused relocate's approval must be withdrawn: {cfg:?}\n{output}"
    );
    guard.assert_unchanged("a_refused_relocate_withdraws_the_new_paths_approval");
}
