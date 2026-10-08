//! End-to-end regression for issue #8175: `index remove <PATH>` must never
//! let `TRUSTY_INDEX` silently win over — or silently lose to — an explicit
//! PATH argument that names a DIFFERENT index.
//!
//! Why: on 2026-09-26, `trusty-search index remove <scratch-path> --yes
//! --json` deleted the live `trusty-tools-4e2cf878` project index (about
//! 236k chunks) instead of the scratch index the PATH named, because the
//! shell also exported `TRUSTY_INDEX=trusty-tools-4e2cf878` and the CLI's
//! `-i`/`--index` global flag folds that env value in indistinguishably from
//! a real flag. The pre-fix `handle_index_remove` used
//! `explicit_index_id` — which is `cli.index`, i.e. exactly this folded value
//! — UNCONDITIONALLY whenever it was `Some`, skipping the PATH lookup
//! entirely. This file proves the fix: given a PATH naming index A and
//! `TRUSTY_INDEX` naming a DIFFERENT index B, the command refuses and BOTH
//! indexes are still registered afterward.
//!
//! What: serves the real socket router with two bare, resident indexes
//! registered directly via `IndexHandle::bare` (no embedder, no walk — issue
//! #8175's Constraints section forbids touching any live registered index, so
//! this never runs against the operator's own daemon), behind a recording
//! proxy (#9214: the CLI reaches the daemon over its socket only). The real
//! compiled `trusty-search` binary is spawned with `TRUSTY_SEARCH_SOCKET`
//! naming that proxy, as `index remove <root-of-A>` with
//! `TRUSTY_INDEX=<id of B>` in its environment. Asserts the process exits
//! non-zero, the message names both ids, and a follow-up index list still
//! names both.
//!
//! Fix-up (issue #8175, code-critic WARN): every spawned subprocess also gets
//! `HOME`/`XDG_CONFIG_HOME` pointed at a per-test fake home
//! ([`remove_command`]), and [`RealAllowlistGuard`] asserts the operator's
//! REAL `~/Library/Application Support/trusty-search/allowlist.toml` is
//! byte-for-byte and mtime-for-mtime unchanged across every run — see that
//! guard's doc comment for why this file specifically was at risk. Two
//! error-arm (Fail-Open Check) tests are added: an absent-socket "daemon down"
//! and an unavailable-during-agreement-check daemon must both refuse with NO
//! delete ever sent, proven against the call log the proxy records, not just
//! against the exit code.
//!
//! Test: `cargo test -p trusty-search --test integration index_remove_env_conflict_8175::`

use crate::test_daemon;
use std::path::Path;

use std::process::Command;
use std::sync::Arc;

use tokio::sync::RwLock;
use trusty_common::uds::server::RpcError;

use trusty_search::core::indexer::CodeIndexer;
use trusty_search::core::registry::{IndexHandle, IndexId, IndexRegistry};
use trusty_search::service::rpc::error::CODE_UNAVAILABLE;
use trusty_search::service::server::SearchAppState;

use crate::socket_daemon::{serve_logged, LoggedDaemon};

const INDEX_A: &str = "idx-a-8175";
const INDEX_B: &str = "idx-b-8175";

// ─── Real-allowlist guard (fix-up: code-critic WARN) ───────────────────────
// #8737: shared with `reindex_quantize_env_conflict_8737.rs`.
use crate::real_allowlist_guard;
use real_allowlist_guard::RealAllowlistGuard;

// ─── Call log + chaos (fix-up: error-arm tests) ────────────────────────────

/// Whether the daemon received any delete.
///
/// Why: the error-arm tests must prove NO delete reached the daemon, not
/// merely that the process exited non-zero — a refusal that happened to also
/// return a bad exit code for an unrelated reason would pass an exit-code-only
/// assertion.
fn contains_delete(daemon: &LoggedDaemon) -> bool {
    daemon
        .snapshot()
        .iter()
        .any(|(m, _)| m == "search.index.delete")
}

/// Serve the real socket router with two independent, resident, bare indexes
/// registered — no embedder, no walk, no disk corpus, so nothing here can
/// touch a real operator index — behind a recording proxy. `chaos` makes the
/// proxy refuse every `search.index.status` as unavailable, simulating a
/// daemon that cannot serve the per-index status lookup `find_index_by_path`
/// needs to resolve PATH → id during the agreement check (issue #8175 fix-up,
/// error-arm test (b)).
async fn spawn_daemon_with_two_indexes(root_a: &Path, root_b: &Path, chaos: bool) -> LoggedDaemon {
    let registry = IndexRegistry::new();
    for (id, root) in [(INDEX_A, root_a), (INDEX_B, root_b)] {
        let indexer = CodeIndexer::new(id, root.to_string_lossy().into_owned());
        registry.register(IndexHandle::bare(
            IndexId::new(id),
            Arc::new(RwLock::new(indexer)),
            root.to_path_buf(),
        ));
    }
    serve_logged(Arc::new(SearchAppState::new(registry)), move |method, _| {
        (chaos && method == "search.index.status")
            .then(|| Err(RpcError::new(CODE_UNAVAILABLE, "chaos_503_test")))
    })
    .await
}

/// Base `Command` for `trusty-search index remove` against the daemon on
/// `socket`, pre-wired with `TRUSTY_DATA_DIR` and — fix-up, issue #8175 —
/// `HOME`/`XDG_CONFIG_HOME` pointed at `fake_home`, so
/// `AllowlistConfig::default_path()` can never resolve to the operator's real
/// config directory even on the code path that performs a real delete. See
/// [`RealAllowlistGuard`].
fn remove_command(socket: &Path, data_dir: &Path, fake_home: &Path) -> Command {
    let mut cmd = test_daemon::command();
    cmd.args(["index", "remove"])
        .env("TRUSTY_SEARCH_SOCKET", socket)
        .env("TRUSTY_DATA_DIR", data_dir)
        .env("HOME", fake_home)
        .env("XDG_CONFIG_HOME", fake_home);
    cmd
}

// #8900: stamped by `test_daemon::command` and bounded here, so a daemon the
// CLI auto-starts can neither outlive the run nor hold this call open.
fn run(mut cmd: Command) -> (i32, String) {
    let out = test_daemon::run_bounded(&mut cmd, std::time::Duration::from_secs(120));
    (out.code.unwrap_or(-1), out.combined)
}

/// The currently-registered ids, read past the proxy so the read is not
/// recorded.
async fn list_indexes(daemon: &LoggedDaemon) -> Vec<String> {
    let body = daemon
        .upstream()
        .call("search.indexes.list", serde_json::json!({}))
        .await
        .expect("search.indexes.list");
    body["indexes"]
        .as_array()
        .expect("indexes array")
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

/// Core regression: PATH names A, `TRUSTY_INDEX` names B — the command must
/// refuse and touch neither.
///
/// Why: this is the exact incident shape. On pre-#8175 `main.rs` /
/// `handle_index_remove`, `cli.index` (folding `TRUSTY_INDEX`) was used
/// unconditionally whenever `Some`, so this same invocation would have
/// resolved straight to index B via `find_index_by_id` and deleted it with no
/// refusal — silently ignoring the PATH argument naming A entirely.
///
/// `multi_thread` is required: the blocking `Command::output()` call below
/// occupies one worker thread for the whole subprocess run, and the test
/// daemon's socket servers need a second one free to keep accepting
/// connections while that call is in flight.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remove_with_path_a_and_env_b_refuses_and_touches_neither() {
    let root_a = tempfile::tempdir().expect("root A");
    let root_b = tempfile::tempdir().expect("root B");
    let data_dir = tempfile::tempdir().expect("scratch TRUSTY_DATA_DIR");
    let fake_home = tempfile::tempdir().expect("fake HOME");
    let guard = RealAllowlistGuard::capture();

    let daemon = spawn_daemon_with_two_indexes(root_a.path(), root_b.path(), false).await;

    let before = list_indexes(&daemon).await;
    assert!(before.contains(&INDEX_A.to_string()) && before.contains(&INDEX_B.to_string()));

    let mut cmd = remove_command(&daemon.socket, data_dir.path(), fake_home.path());
    cmd.arg(root_a.path())
        .arg("--yes")
        .env("TRUSTY_INDEX", INDEX_B);
    let (code, output) = run(cmd);

    assert_ne!(
        code, 0,
        "PATH=A vs TRUSTY_INDEX=B must refuse rather than pick one; output:\n{output}"
    );
    assert!(
        output.contains(INDEX_A) && output.contains(INDEX_B),
        "the refusal must name both conflicting values; output:\n{output}"
    );

    let after = list_indexes(&daemon).await;
    assert!(
        after.contains(&INDEX_A.to_string()),
        "index A must survive a refused remove; still registered: {after:?}"
    );
    assert!(
        after.contains(&INDEX_B.to_string()),
        "index B must survive a refused remove; still registered: {after:?}"
    );
    guard.assert_unchanged("remove_with_path_a_and_env_b_refuses_and_touches_neither");
}

/// `index remove` with NO PATH argument, only `TRUSTY_INDEX`, must also
/// refuse — a destructive verb never resolves its target from the
/// environment alone (issue #8175's "Wanted" list, second bullet).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remove_with_no_argument_and_env_only_refuses() {
    let root_a = tempfile::tempdir().expect("root A");
    let root_b = tempfile::tempdir().expect("root B");
    let data_dir = tempfile::tempdir().expect("scratch TRUSTY_DATA_DIR");
    let fake_home = tempfile::tempdir().expect("fake HOME");
    let guard = RealAllowlistGuard::capture();

    let daemon = spawn_daemon_with_two_indexes(root_a.path(), root_b.path(), false).await;

    // No PATH argument at all — cwd of the spawned process is irrelevant
    // because the refusal must fire before any path resolution happens.
    let mut cmd = remove_command(&daemon.socket, data_dir.path(), fake_home.path());
    cmd.arg("--yes")
        .env("TRUSTY_INDEX", INDEX_B)
        .current_dir(std::env::temp_dir());
    let (code, output) = run(cmd);

    assert_ne!(
        code, 0,
        "TRUSTY_INDEX alone, with no PATH/-i, must refuse; output:\n{output}"
    );
    assert!(
        output.contains(INDEX_B),
        "the refusal must name the TRUSTY_INDEX id it declined to touch: {output}"
    );

    let after = list_indexes(&daemon).await;
    assert!(
        after.contains(&INDEX_A.to_string()) && after.contains(&INDEX_B.to_string()),
        "both indexes must survive; still registered: {after:?}"
    );
    guard.assert_unchanged("remove_with_no_argument_and_env_only_refuses");
}

// ─── Error-arm tests (Fail-Open Check) ──────────────────────────────────────

/// (a) PATH and `-i` both given, daemon down (the CLI is pointed at a socket
/// nothing serves): `remove` must exit non-zero naming that socket, and the
/// real daemon — which the CLI is deliberately NOT pointed at — must receive
/// nothing at all, delete included.
///
/// Why: closes the daemon-guard's own "is anything listening?" gap. A
/// `daemon.lock` naming PID 1 (always alive, same trick
/// `daemon_env_precedence.rs` uses) makes the guard take its "already
/// running, waiting" branch instead of auto-spawning a real daemon
/// subprocess — undesirable in a test even when scoped to an isolated
/// `TRUSTY_DATA_DIR`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remove_with_path_and_flag_when_daemon_is_down_refuses_with_no_requests() {
    let root_a = tempfile::tempdir().expect("root A");
    let root_b = tempfile::tempdir().expect("root B");
    let data_dir = tempfile::tempdir().expect("scratch TRUSTY_DATA_DIR");
    let fake_home = tempfile::tempdir().expect("fake HOME");
    let guard = RealAllowlistGuard::capture();

    // A real daemon DOES run, so we can prove it received nothing — but the
    // CLI below is pointed elsewhere, at a socket nothing serves.
    let daemon = spawn_daemon_with_two_indexes(root_a.path(), root_b.path(), false).await;
    let absent = data_dir.path().join("absent.sock");
    // PID 1 (init) is always alive — matches daemon_env_precedence.rs's
    // established trick for making the guard skip auto-spawn.
    std::fs::write(data_dir.path().join("daemon.lock"), b"1").expect("write daemon.lock");

    let mut cmd = remove_command(&absent, data_dir.path(), fake_home.path());
    cmd.arg(root_a.path())
        .arg("--index")
        .arg(INDEX_B)
        .arg("--yes");
    let (code, output) = run(cmd);

    assert_ne!(
        code, 0,
        "a down daemon must refuse, not silently pick a target; output:\n{output}"
    );
    assert!(
        output.contains(&absent.display().to_string()),
        "the refusal must name the socket; output:\n{output}"
    );
    assert!(
        daemon.snapshot().is_empty(),
        "a down daemon must receive NO call at all: {:?}",
        daemon.snapshot()
    );
    guard
        .assert_unchanged("remove_with_path_and_flag_when_daemon_is_down_refuses_with_no_requests");
}

/// (b) PATH and `-i` both given, the daemon is reachable but refuses every
/// per-index status lookup as unavailable — the lookup that resolves PATH to
/// an id during the agreement check. `remove` must exit non-zero with NO
/// delete ever sent.
///
/// Why: the `PathAndId` arm resolves PATH through `find_index_by_path`, which
/// fails closed on any unreadable status (#8737), so the refusal is a hard error
/// before the id-agreement
/// comparison (and therefore before any `DELETE`) is ever reached. This pins
/// that existing propagation against a real refusal over the socket, not just
/// a synthetic `Result::Err` in a unit test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remove_with_path_and_flag_when_status_503s_refuses_with_no_delete() {
    let root_a = tempfile::tempdir().expect("root A");
    let root_b = tempfile::tempdir().expect("root B");
    let data_dir = tempfile::tempdir().expect("scratch TRUSTY_DATA_DIR");
    let fake_home = tempfile::tempdir().expect("fake HOME");
    let guard = RealAllowlistGuard::capture();

    let daemon = spawn_daemon_with_two_indexes(root_a.path(), root_b.path(), true).await;

    let mut cmd = remove_command(&daemon.socket, data_dir.path(), fake_home.path());
    cmd.arg(root_a.path())
        .arg("--index")
        .arg(INDEX_B)
        .arg("--yes");
    let (code, output) = run(cmd);

    assert_ne!(
        code, 0,
        "a 503 during PATH resolution must refuse, not silently pick a target; \
         output:\n{output}"
    );
    let requests = daemon.snapshot();
    assert!(
        !requests.is_empty(),
        "the daemon must have been contacted (indexes.list, index.status)"
    );
    assert!(
        !contains_delete(&daemon),
        "NO delete must ever be sent when PATH resolution is refused: {requests:?}"
    );

    let after = list_indexes(&daemon).await;
    assert!(
        after.contains(&INDEX_A.to_string()) && after.contains(&INDEX_B.to_string()),
        "both indexes must survive; still registered: {after:?}"
    );
    guard.assert_unchanged("remove_with_path_and_flag_when_status_503s_refuses_with_no_delete");
}
