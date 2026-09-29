//! End-to-end regression for #8687: `index remove X` never depends on an
//! unrelated index Y's residency, and it cleans X's allowlist row.
//!
//! Why: `GET /indexes` listed resident indexes only and a cold-parked index's
//! status answers `503 index_not_resident`. A parked X could not be resolved,
//! and an X already deleted through MCP left its `allowlist.toml` row behind,
//! because the command aborted before its cleanup.
//! What: a real router whose indexes are cold-parked in its cold store, then the
//! compiled `trusty-search index remove <PATH>` against it. The router process
//! points `TRUSTY_DATA_DIR` at a temp dir before any request, so its
//! `indexes.toml` rewrite never reaches a real registry; the subprocess gets a
//! fake `HOME`, and the operator's real allowlist is proven unchanged. The
//! parked-aware lookups are the shared `commands::explicit_target` ones, so
//! the PATH-plus-flag shape and `reindex` are driven here too.
//! Test: `cargo test -p trusty-search --test index_remove_residency_8687`

#[path = "support/test_daemon.rs"]
mod test_daemon;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use tokio::sync::RwLock;

use trusty_search::allowlist::{AllowlistConfig, AllowlistEntry};
use trusty_search::core::indexer::CodeIndexer;
use trusty_search::core::registry::{IndexHandle, IndexId, IndexRegistry};
use trusty_search::service::persistence::PersistedIndex;
use trusty_search::service::server::{build_router_on, SearchAppState};

/// One scratch `TRUSTY_DATA_DIR` for this test binary, set before any router
/// handles a request.
fn isolate_router_data_dir() {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    let dir = DIR.get_or_init(|| tempfile::tempdir().expect("router data dir"));
    // SAFETY: every test sets the same value, before its router starts.
    unsafe { std::env::set_var("TRUSTY_DATA_DIR", dir.path()) };
}

type Log = Arc<Mutex<Vec<(String, String)>>>;

async fn log_requests(State(log): State<Log>, req: Request, next: Next) -> Response {
    let entry = (req.method().to_string(), req.uri().path().to_string());
    log.lock().expect("log").push(entry);
    next.run(req).await
}

/// Serve a router with `parked` registered only in the cold store.
async fn spawn_daemon(parked: &[(&str, &Path)]) -> (String, Arc<SearchAppState>, Log) {
    spawn_daemon_with(parked, &[], None).await
}

/// [`spawn_daemon`] plus bare `resident` indexes, and — when `failing_status`
/// names one — a `GET /indexes/<id>/status` that always answers `500`.
async fn spawn_daemon_with(
    parked: &[(&str, &Path)],
    resident: &[(&str, &Path)],
    failing_status: Option<&'static str>,
) -> (String, Arc<SearchAppState>, Log) {
    isolate_router_data_dir();
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    let entries = parked
        .iter()
        .map(|(id, root)| PersistedIndex::new(id.to_string(), root.to_path_buf()))
        .collect();
    state.cold_store.register_cold_entries(entries);
    for (id, root) in resident {
        let indexer = CodeIndexer::new(*id, root.to_string_lossy().into_owned());
        state.registry.register(IndexHandle::bare(
            IndexId::new(*id),
            Arc::new(RwLock::new(indexer)),
            root.to_path_buf(),
        ));
    }
    let log: Log = Arc::default();
    let mut app = build_router_on(
        Arc::clone(&state),
        trusty_common::server::SelfOrigins::default(),
    );
    if let Some(id) = failing_status {
        let status_path = format!("/indexes/{id}/status");
        app = app.layer(middleware::from_fn(move |req: Request, next: Next| {
            let fail = req.method() == Method::GET && req.uri().path() == status_path;
            async move {
                if fail {
                    return (StatusCode::INTERNAL_SERVER_ERROR, "status chaos").into_response();
                }
                next.run(req).await
            }
        }));
    }
    let app = app.layer(middleware::from_fn_with_state(log.clone(), log_requests));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), state, log)
}

/// The allowlist file the subprocess resolves under `fake_home`.
fn fake_allowlist(fake_home: &Path) -> PathBuf {
    let config = if cfg!(target_os = "macos") {
        fake_home.join("Library").join("Application Support")
    } else {
        fake_home.to_path_buf()
    };
    config.join("trusty-search").join("allowlist.toml")
}

fn seed_allowlist(file: &Path, roots: &[&Path]) {
    std::fs::create_dir_all(file.parent().expect("parent")).expect("config dir");
    let mut cfg = AllowlistConfig::default();
    for root in roots {
        cfg.upsert(AllowlistEntry {
            path: root.to_path_buf(),
            name: None,
            exclude: Vec::new(),
            extensions: Vec::new(),
            skip_kg: false,
        });
    }
    cfg.save_to(file).expect("seed allowlist");
}

/// Run `index remove <root> --keep-data` against `base`; returns exit code and output.
fn remove(base: &str, root: &Path, fake_home: &Path) -> (i32, String) {
    let root = root.to_str().expect("utf-8 root");
    cli(base, &["index", "remove", root, "--keep-data"], fake_home)
}

/// Run `trusty-search <args>` against `base`; returns exit code and output.
fn cli(base: &str, args: &[&str], fake_home: &Path) -> (i32, String) {
    let data_dir = tempfile::tempdir().expect("cli data dir");
    std::fs::write(
        data_dir.path().join("http_addr"),
        base.trim_start_matches("http://"),
    )
    .expect("http_addr");
    let cwd = tempfile::tempdir().expect("cli cwd");
    // #8900: stamped and bounded, so a daemon the CLI auto-starts can neither
    // outlive the run nor hold this call open.
    let mut cmd = test_daemon::command();
    cmd.args(args)
        .current_dir(cwd.path())
        .env("TRUSTY_DATA_DIR", data_dir.path())
        .env("HOME", fake_home)
        .env("XDG_CONFIG_HOME", fake_home)
        .env_remove("TRUSTY_INDEX");
    let out = test_daemon::run_bounded(&mut cmd, std::time::Duration::from_secs(120));
    (out.code.unwrap_or(-1), out.combined)
}

fn canonical_tempdir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("root");
    let path = std::fs::canonicalize(dir.path()).expect("canonicalize");
    (dir, path)
}

fn real_allowlist_bytes() -> Option<Vec<u8>> {
    std::fs::read(AllowlistConfig::default_path()).ok()
}

fn allowlisted(file: &Path, root: &Path) -> bool {
    AllowlistConfig::load_from(file)
        .expect("load allowlist")
        .contains(root)
}

/// #8687: X and an unrelated Y are both cold-parked, so both statuses `503`.
/// Removing X resolves it from its parked row, deletes only X, and clears only
/// X's allowlist row.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removing_a_cold_parked_index_beside_a_cold_neighbour_succeeds() {
    let real = real_allowlist_bytes();
    let ((_x, root_x), (_y, root_y)) = (canonical_tempdir(), canonical_tempdir());
    let fake_home = tempfile::tempdir().expect("fake HOME");
    let file = fake_allowlist(fake_home.path());
    seed_allowlist(&file, &[&root_x, &root_y]);
    let (base, state, log) =
        spawn_daemon(&[("idx-y-8687", &root_y), ("idx-x-8687", &root_x)]).await;

    let (code, output) = remove(&base, &root_x, fake_home.path());

    assert_eq!(
        code, 0,
        "removing a parked X must succeed; output:\n{output}"
    );
    let deletes: Vec<_> = log
        .lock()
        .expect("log")
        .iter()
        .filter(|(m, _)| m == "DELETE")
        .map(|(_, p)| p.clone())
        .collect();
    assert_eq!(deletes, vec!["/indexes/idx-x-8687".to_string()], "{output}");
    assert!(
        !allowlisted(&file, &root_x),
        "X's allowlist row must be gone"
    );
    assert!(
        allowlisted(&file, &root_y),
        "Y's allowlist row must survive"
    );
    let y = trusty_search::core::registry::IndexId::new("idx-y-8687");
    assert!(state.cold_store.contains(&y), "Y must still be registered");
    assert_eq!(
        real,
        real_allowlist_bytes(),
        "the real allowlist must be untouched"
    );
}

/// #8687: X was already deleted through another surface; Y is cold-parked.
/// Removing X by PATH clears its stale allowlist row and sends no DELETE.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removing_an_already_deleted_index_clears_its_stale_allowlist_row() {
    let real = real_allowlist_bytes();
    let ((_x, root_x), (_y, root_y)) = (canonical_tempdir(), canonical_tempdir());
    let fake_home = tempfile::tempdir().expect("fake HOME");
    let file = fake_allowlist(fake_home.path());
    seed_allowlist(&file, &[&root_x, &root_y]);
    let (base, _state, log) = spawn_daemon(&[("idx-y2-8687", &root_y)]).await;

    let (code, output) = remove(&base, &root_x, fake_home.path());

    assert_eq!(
        code, 0,
        "a stale row must be cleaned, not refused; output:\n{output}"
    );
    assert!(
        !log.lock().expect("log").iter().any(|(m, _)| m == "DELETE"),
        "nothing is registered for X, so nothing may be deleted"
    );
    assert!(
        !allowlisted(&file, &root_x),
        "X's stale allowlist row must be gone"
    );
    assert!(
        allowlisted(&file, &root_y),
        "Y's allowlist row must survive"
    );
    assert_eq!(
        real,
        real_allowlist_bytes(),
        "the real allowlist must be untouched"
    );
}

/// Every non-GET request the router saw, as `(method, path)`.
fn mutations(log: &Log) -> Vec<(String, String)> {
    log.lock()
        .expect("log")
        .iter()
        .filter(|(m, _)| m != "GET")
        .cloned()
        .collect()
}

/// #8687 via the shared lookups: PATH plus an agreeing `--index` resolves a
/// parked X from its parked row and deletes only X.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removing_a_parked_index_by_path_and_flag_succeeds() {
    let (_x, root_x) = canonical_tempdir();
    let fake_home = tempfile::tempdir().expect("fake HOME");
    seed_allowlist(&fake_allowlist(fake_home.path()), &[&root_x]);
    let (base, _state, log) = spawn_daemon(&[("idx-x3-8687", &root_x)]).await;
    let root = root_x.to_str().expect("utf-8 root");

    let args = [
        "index",
        "remove",
        root,
        "--index",
        "idx-x3-8687",
        "--keep-data",
    ];
    let (code, output) = cli(&base, &args, fake_home.path());

    assert_eq!(
        code, 0,
        "PATH and flag agree on parked X; output:\n{output}"
    );
    assert_eq!(
        mutations(&log),
        vec![("DELETE".to_string(), "/indexes/idx-x3-8687".to_string())],
        "{output}"
    );
}

/// #8687/#8737: `reindex` of a parked index — by flag or by PATH — refuses
/// before any reindex request and says the index is parked, because the
/// daemon's reindex route serves resident indexes only.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reindex_of_a_parked_index_refuses_and_names_it_parked() {
    let (_x, root_x) = canonical_tempdir();
    let fake_home = tempfile::tempdir().expect("fake HOME");
    let (base, _state, log) = spawn_daemon(&[("idx-x4-8687", &root_x)]).await;
    let root = root_x.to_str().expect("utf-8 root");

    for args in [
        vec!["reindex", "--index", "idx-x4-8687"],
        vec!["reindex", root],
    ] {
        let (code, output) = cli(&base, &args, fake_home.path());
        assert_ne!(code, 0, "{args:?}: a parked target must refuse:\n{output}");
        assert!(
            output.contains("idx-x4-8687") && output.contains("parked"),
            "{args:?}: the refusal must name the index and say it is parked:\n{output}"
        );
    }
    assert!(
        mutations(&log).is_empty(),
        "no reindex may be sent: {:?}",
        mutations(&log)
    );
}

/// #8687 fail-closed: a resident index whose status answers `500` (not `404`)
/// could own PATH, so `index remove <PATH>` must refuse naming it, not report
/// PATH unregistered and clear its rows as stale.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreadable_status_refuses_instead_of_reporting_not_registered() {
    let real = real_allowlist_bytes();
    let ((_a, root_a), (_x, root_x)) = (canonical_tempdir(), canonical_tempdir());
    let fake_home = tempfile::tempdir().expect("fake HOME");
    let file = fake_allowlist(fake_home.path());
    seed_allowlist(&file, &[&root_x]);
    let (base, _state, log) =
        spawn_daemon_with(&[], &[("idx-a5-8687", &root_a)], Some("idx-a5-8687")).await;

    let (code, output) = remove(&base, &root_x, fake_home.path());

    assert_ne!(code, 0, "an unreadable status must refuse:\n{output}");
    assert!(
        output.contains("\"idx-a5-8687\""),
        "the refusal must name the unreadable index:\n{output}"
    );
    assert!(
        !output.contains("already removed"),
        "PATH must not be reported unregistered:\n{output}"
    );
    assert!(mutations(&log).is_empty(), "{:?}", mutations(&log));
    assert!(
        allowlisted(&file, &root_x),
        "X's row must survive a refusal"
    );
    assert_eq!(
        real,
        real_allowlist_bytes(),
        "the real allowlist is untouched"
    );
}
