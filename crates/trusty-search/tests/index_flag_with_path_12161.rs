//! Binary-level regression for #12161: `trusty-search index <path> -i <id>`
//! must register under `<id>`, never under the path basename.
//!
//! Why: the unit tests in `commands/index.rs` cannot see the wiring in
//! `main.rs` (clap parsing, `value_source("index")`, the merge call), which is
//! the actual fix. Only the compiled binary exercises it.
//! What: spawns the real `trusty-search` against a recording proxy daemon
//! (#9214: socket only), with `TRUSTY_DATA_DIR`, `HOME` and `XDG_CONFIG_HOME`
//! pointed at scratch dirs so no real daemon, allowlist or Keychain is
//! reached. The proxy answers `search.index.create` with an error after
//! logging it, so the test reads the id the CLI resolved and nothing is
//! registered or indexed. The root sits under `CARGO_TARGET_TMPDIR` because the
//! denylist refuses OS temp dirs; it is pre-approved in the scratch allowlist.
//! Test: `cargo test -p trusty-search --test integration index_flag_with_path_12161::`

use crate::real_allowlist_guard::RealAllowlistGuard;
use crate::socket_daemon::{serve_logged, LoggedDaemon};
use crate::test_daemon;
use std::path::Path;
use std::sync::Arc;
use trusty_common::uds::server::RpcError;
use trusty_search::core::registry::IndexRegistry;
use trusty_search::service::server::SearchAppState;

const CREATE: &str = "search.index.create";

/// A recording daemon whose `search.index.create` stops the CLI after logging.
async fn daemon_stopping_at_create() -> LoggedDaemon {
    serve_logged(
        Arc::new(SearchAppState::new(IndexRegistry::new())),
        |method, _| {
            (method == CREATE).then(|| Err(RpcError::internal("stop after create (#12161)")))
        },
    )
    .await
}

/// Run `trusty-search index <root> <extra...>`; returns (exit code, output).
fn run_index(
    daemon: &LoggedDaemon,
    root: &Path,
    extra: &[&str],
    trusty_index_env: Option<&str>,
    data_dir: &Path,
    fake_home: &Path,
) -> (i32, String) {
    let mut cmd = test_daemon::command();
    cmd.arg("index")
        .arg(root)
        .args(extra)
        .env("TRUSTY_SEARCH_SOCKET", &daemon.socket)
        .env("TRUSTY_DATA_DIR", data_dir)
        .env("HOME", fake_home)
        .env("XDG_CONFIG_HOME", fake_home)
        .env_remove("TRUSTY_INDEX")
        .current_dir(fake_home);
    if let Some(v) = trusty_index_env {
        cmd.env("TRUSTY_INDEX", v);
    }
    let out = test_daemon::run_bounded(&mut cmd, std::time::Duration::from_secs(120));
    (out.code.unwrap_or(-1), out.combined)
}

/// The `id` of the first `search.index.create` the daemon saw, if any.
fn created_id(daemon: &LoggedDaemon) -> Option<String> {
    daemon
        .snapshot()
        .into_iter()
        .find(|(m, _)| m == CREATE)
        .and_then(|(_, p)| p["id"].as_str().map(str::to_string))
}

/// A project root the denylist accepts, pre-approved in `data_dir`'s allowlist.
fn approved_root(data_dir: &Path) -> tempfile::TempDir {
    let root = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("root under target tmp");
    let canon = root.path().canonicalize().expect("canonical root");
    let toml = format!("[[index]]\npath = {:?}\n", canon.to_string_lossy());
    std::fs::write(data_dir.join("allowlist.toml"), toml).expect("write scratch allowlist");
    root
}

/// `-i a --name b` is refused before any daemon call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_typed_index_flag_that_differs_from_name_is_refused_12161() {
    let root = tempfile::tempdir().expect("root");
    let (data_dir, fake_home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let guard = RealAllowlistGuard::capture();
    let daemon = daemon_stopping_at_create().await;

    let (code, output) = run_index(
        &daemon,
        root.path(),
        &["-i", "idx-a-12161", "--name", "idx-b-12161"],
        None,
        data_dir.path(),
        fake_home.path(),
    );

    assert_ne!(
        code, 0,
        "conflicting -i/--name must fail; output:\n{output}"
    );
    assert!(
        output.contains("idx-a-12161") && output.contains("idx-b-12161"),
        "the error must name both values; output:\n{output}"
    );
    assert!(
        output.contains("name different indexes"),
        "the refusal must be the -i/--name conflict, not another error; output:\n{output}"
    );
    assert!(
        daemon.mutations().is_empty(),
        "nothing may reach the daemon"
    );
    guard.assert_unchanged("a_typed_index_flag_that_differs_from_name_is_refused_12161");
}

/// `index <dir> -i X` alone registers under X, not the basename.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_explicit_index_flag_names_the_registered_index_12161() {
    let (data_dir, fake_home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let root = approved_root(data_dir.path());
    let guard = RealAllowlistGuard::capture();
    let daemon = daemon_stopping_at_create().await;

    let (_code, output) = run_index(
        &daemon,
        root.path(),
        &["-i", "flagged-12161"],
        None,
        data_dir.path(),
        fake_home.path(),
    );

    assert_eq!(
        created_id(&daemon).as_deref(),
        Some("flagged-12161"),
        "-i must name the registration, not the basename; output:\n{output}"
    );
    guard.assert_unchanged("an_explicit_index_flag_names_the_registered_index_12161");
}

/// `TRUSTY_INDEX` alone neither conflicts with `--name` nor renames it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_env_only_trusty_index_does_not_conflict_with_name_12161() {
    let (data_dir, fake_home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let root = approved_root(data_dir.path());
    let guard = RealAllowlistGuard::capture();
    let daemon = daemon_stopping_at_create().await;

    let (_code, output) = run_index(
        &daemon,
        root.path(),
        &["--name", "named-12161"],
        Some("from-env-12161"),
        data_dir.path(),
        fake_home.path(),
    );

    assert!(
        !output.contains("name different indexes"),
        "an env-only TRUSTY_INDEX must not read as a typed -i; output:\n{output}"
    );
    assert_eq!(
        created_id(&daemon).as_deref(),
        Some("named-12161"),
        "--name wins over an env-only TRUSTY_INDEX; output:\n{output}"
    );
    guard.assert_unchanged("an_env_only_trusty_index_does_not_conflict_with_name_12161");
}
