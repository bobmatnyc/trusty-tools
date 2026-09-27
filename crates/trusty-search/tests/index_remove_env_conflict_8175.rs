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
//! What: serves a real axum router (`build_router`) with two bare, resident
//! indexes registered directly via `IndexHandle::bare` (no embedder, no
//! walk — issue #8175's Constraints section forbids touching any live
//! registered index, so this never runs against the operator's own daemon).
//! The router's address is written to the isolated `TRUSTY_DATA_DIR`'s
//! `http_addr` discovery file, then the real compiled `trusty-search` binary
//! is spawned as `index remove <root-of-A>` with `TRUSTY_INDEX=<id of B>` in
//! its environment. Asserts the process exits non-zero, the message names
//! both ids, and a follow-up `GET /indexes` still lists both.
//!
//! Test: `cargo test -p trusty-search --test index_remove_env_conflict_8175`

use std::process::Command;
use std::sync::Arc;

use tokio::sync::RwLock;

use trusty_search::core::indexer::CodeIndexer;
use trusty_search::core::registry::{IndexHandle, IndexId, IndexRegistry};
use trusty_search::service::server::{build_router, SearchAppState};

const INDEX_A: &str = "idx-a-8175";
const INDEX_B: &str = "idx-b-8175";

/// Serve a real daemon router with two independent, resident, bare indexes
/// registered — no embedder, no walk, no disk corpus, so nothing here can
/// touch a real operator index. Returns the base URL.
async fn spawn_daemon_with_two_indexes(
    root_a: &std::path::Path,
    root_b: &std::path::Path,
) -> String {
    let registry = IndexRegistry::new();
    for (id, root) in [(INDEX_A, root_a), (INDEX_B, root_b)] {
        let indexer = CodeIndexer::new(id, root.to_string_lossy().into_owned());
        registry.register(IndexHandle::bare(
            IndexId::new(id),
            Arc::new(RwLock::new(indexer)),
            root.to_path_buf(),
        ));
    }
    let state = SearchAppState::new(registry);
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

/// Point the CLI's daemon discovery at `base` inside an isolated
/// `TRUSTY_DATA_DIR`, so the spawned `trusty-search` process finds the test
/// router instead of any real daemon on the machine.
fn write_discovery_file(data_dir: &std::path::Path, base: &str) {
    std::fs::create_dir_all(data_dir).expect("create scratch TRUSTY_DATA_DIR");
    let addr = base.trim_start_matches("http://");
    std::fs::write(data_dir.join("http_addr"), addr).expect("write http_addr discovery file");
}

/// Run `trusty-search index remove <path_a>` with `TRUSTY_INDEX=idx-b-8175`
/// against the isolated test daemon, and return `(exit_code, combined_output)`.
fn run_remove(data_dir: &std::path::Path, path_a: &std::path::Path) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_trusty-search"))
        .args(["index", "remove"])
        .arg(path_a)
        .arg("--yes")
        .env("TRUSTY_DATA_DIR", data_dir)
        .env("TRUSTY_INDEX", INDEX_B)
        .output()
        .expect("spawn trusty-search index remove");
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), combined)
}

/// `GET /indexes` against the test router — the list of currently-registered
/// ids.
async fn list_indexes(base: &str) -> Vec<String> {
    let body: serde_json::Value = reqwest::get(format!("{base}/indexes"))
        .await
        .expect("GET /indexes")
        .json()
        .await
        .expect("parse /indexes body");
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
/// router's `axum::serve` task needs a second one free to keep accepting
/// connections while that call is in flight.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remove_with_path_a_and_env_b_refuses_and_touches_neither() {
    let root_a = tempfile::tempdir().expect("root A");
    let root_b = tempfile::tempdir().expect("root B");
    let data_dir = tempfile::tempdir().expect("scratch TRUSTY_DATA_DIR");

    let base = spawn_daemon_with_two_indexes(root_a.path(), root_b.path()).await;
    write_discovery_file(data_dir.path(), &base);

    let before = list_indexes(&base).await;
    assert!(before.contains(&INDEX_A.to_string()) && before.contains(&INDEX_B.to_string()));

    let (code, output) = run_remove(data_dir.path(), root_a.path());

    assert_ne!(
        code, 0,
        "PATH=A vs TRUSTY_INDEX=B must refuse rather than pick one; output:\n{output}"
    );
    assert!(
        output.contains(INDEX_A) && output.contains(INDEX_B),
        "the refusal must name both conflicting values; output:\n{output}"
    );

    let after = list_indexes(&base).await;
    assert!(
        after.contains(&INDEX_A.to_string()),
        "index A must survive a refused remove; still registered: {after:?}"
    );
    assert!(
        after.contains(&INDEX_B.to_string()),
        "index B must survive a refused remove; still registered: {after:?}"
    );
}

/// `index remove` with NO PATH argument, only `TRUSTY_INDEX`, must also
/// refuse — a destructive verb never resolves its target from the
/// environment alone (issue #8175's "Wanted" list, second bullet).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remove_with_no_argument_and_env_only_refuses() {
    let root_a = tempfile::tempdir().expect("root A");
    let root_b = tempfile::tempdir().expect("root B");
    let data_dir = tempfile::tempdir().expect("scratch TRUSTY_DATA_DIR");

    let base = spawn_daemon_with_two_indexes(root_a.path(), root_b.path()).await;
    write_discovery_file(data_dir.path(), &base);

    // No PATH argument at all — cwd of the spawned process is irrelevant
    // because the refusal must fire before any path resolution happens.
    let out = Command::new(env!("CARGO_BIN_EXE_trusty-search"))
        .args(["index", "remove", "--yes"])
        .env("TRUSTY_DATA_DIR", data_dir.path())
        .env("TRUSTY_INDEX", INDEX_B)
        .current_dir(std::env::temp_dir())
        .output()
        .expect("spawn trusty-search index remove");
    let mut output = String::from_utf8_lossy(&out.stdout).into_owned();
    output.push_str(&String::from_utf8_lossy(&out.stderr));

    assert_ne!(
        out.status.code().unwrap_or(-1),
        0,
        "TRUSTY_INDEX alone, with no PATH/-i, must refuse; output:\n{output}"
    );
    assert!(
        output.contains(INDEX_B),
        "the refusal must name the TRUSTY_INDEX id it declined to touch: {output}"
    );

    let after = list_indexes(&base).await;
    assert!(
        after.contains(&INDEX_A.to_string()) && after.contains(&INDEX_B.to_string()),
        "both indexes must survive; still registered: {after:?}"
    );
}
