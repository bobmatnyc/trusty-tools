//! Ratchet: no NEW CLI or MCP-bridge file may dial the daemon over HTTP (#6285).
//!
//! Why: the consumer move lands in steps, and a later edit that reaches for
//! `daemon_base_url()` or a `reqwest` client re-opens a path the retire slice
//! is about to delete. A sweep over the source is the only check that sees a
//! call site before it runs.
//! What: every `.rs` file under `src/commands/`, `src/mcp/`, plus `src/main.rs`
//! is read, and none may name one of [`HTTP_MARKERS`]. #9214 PR-A moved the
//! last listed file (`src/commands/start/tests.rs`) onto the socket and deleted
//! the `NOT_YET_MOVED` list, so every marker is now forbidden outright.
//! Test: this file.

use std::path::{Path, PathBuf};

/// Source text that means "this file reaches the daemon over HTTP".
const HTTP_MARKERS: &[&str] = &[
    "daemon_base_url(",
    "daemon_http_client(",
    "reqwest::",
    "base_url_fn",
    ".http.get(",
    ".http.post(",
    ".http.delete(",
    // #9214: the HTTP-base guard the not-yet-moved subcommands call.
    "ensure_daemon_http_base",
];

/// Every `.rs` file under `dir`, recursively.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|x| x == "rs") {
            out.push(path);
        }
    }
}

/// The crate-relative paths of every swept file that names an HTTP marker.
fn files_dialling_http(root: &Path) -> Vec<String> {
    let mut files = vec![root.join("src/main.rs")];
    rust_files(&root.join("src/commands"), &mut files);
    rust_files(&root.join("src/mcp"), &mut files);
    let mut hits: Vec<String> = files
        .into_iter()
        .filter(|path| {
            let text = std::fs::read_to_string(path).expect("read a swept file");
            HTTP_MARKERS.iter().any(|m| text.contains(m))
        })
        .map(|path| {
            path.strip_prefix(root)
                .expect("under the crate root")
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    hits.sort();
    hits
}

/// Any file that dials HTTP fails (#9214 PR-A: nothing is exempt).
#[test]
fn no_cli_or_bridge_file_reintroduces_an_http_daemon_call() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let hits = files_dialling_http(root);
    assert!(
        hits.is_empty(),
        "these files reach the daemon over HTTP; use service::daemon_client instead: {hits:?}"
    );
}

/// The quantize command — this step's one moved subcommand — stays moved.
#[test]
fn the_quantize_command_dials_no_http() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(!files_dialling_http(root).contains(&"src/commands/quantize.rs".to_string()));
}

/// #9214 B2(a): the CLI paths moved onto `DaemonClient` stay moved, and
/// none of them builds a literal daemon URL.
///
/// Why: the ratchet above lets a listed file keep dialling; these left the
/// list, so a regression must name the file rather than add a row back.
/// What: each file is free of [`HTTP_MARKERS`], and no non-comment line builds
/// a `7878` address or a `daemon_base_url` call.
/// Test: this test.
#[test]
fn the_b2a_cli_paths_dial_no_http() {
    assert_moved_off_http(&[
        "add.rs",
        "daemon_guard.rs",
        "daemon_utils.rs",
        "list.rs",
        "remove.rs",
        "status.rs",
        "watch.rs",
    ]);
}

/// #9214 B2(b1): `cleanup`, `config`, `convert`, `migrate` and the shared
/// `daemon_rpc` mapper stay on the socket, under the same rule as
/// [`the_b2a_cli_paths_dial_no_http`].
/// Test: this test.
#[test]
fn the_b2b1_cli_paths_dial_no_http() {
    assert_moved_off_http(&[
        "cleanup.rs",
        "config.rs",
        "convert.rs",
        "daemon_rpc.rs",
        "migrate.rs",
    ]);
}

/// #9214 B2(b2): the index, reindex, remove, relocate and status paths stay on
/// the socket, under the same rule as [`the_b2a_cli_paths_dial_no_http`].
/// Test: this test.
#[test]
fn the_b2b2_cli_paths_dial_no_http() {
    assert_moved_off_http(&[
        "explicit_target.rs",
        "index.rs",
        "index_cwd_resolve.rs",
        "index_relocate.rs",
        "index_remove.rs",
        "index_remove_stale.rs",
        "index_status.rs",
        "reindex.rs",
        "reindex_engine/driver.rs",
        "reindex_engine/file_ops.rs",
        "reindex_engine/registration.rs",
        "reindex_engine/verify.rs",
    ]);
}

/// #9214 slice 1: `query`, `doctor`, auto-discover and the `monitor` status
/// reads stay on the socket, under the same rule as
/// [`the_b2a_cli_paths_dial_no_http`].
/// Test: this test.
#[test]
fn the_slice1_cli_paths_dial_no_http() {
    assert_moved_off_http(&[
        "discover/mod.rs",
        "discover/rpc.rs",
        "doctor.rs",
        "doctor_checks/mod.rs",
        "doctor_checks/tests.rs",
        "doctor_pipeline.rs",
        "monitor.rs",
        "query.rs",
    ]);
}

/// Each of `files` under `src/commands/` is free of [`HTTP_MARKERS`], and no
/// non-comment line builds a `7878` address or a `daemon_base_url` call.
fn assert_moved_off_http(files: &[&str]) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dialling = files_dialling_http(root);
    let forbidden = [["daemon_base", "_url"].concat(), ["78", "78"].concat()];
    for f in files {
        let rel = format!("src/commands/{f}");
        assert!(!dialling.contains(&rel), "{rel} dials HTTP");
        let text = std::fs::read_to_string(root.join(&rel)).expect("read a moved file");
        for (n, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            assert!(
                !forbidden.iter().any(|m| line.contains(m.as_str())),
                "{rel}:{} reaches for an HTTP daemon address",
                n + 1
            );
        }
    }
}

/// The MCP bridge's own files, crate-relative: everything under `src/mcp/`,
/// plus the `serve` command and its scope module with their tests.
fn bridge_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    rust_files(&root.join("src/mcp"), &mut files);
    for f in [
        "serve.rs",
        "serve_scope.rs",
        "serve_scope_tests.rs",
        "serve_index_env_tests.rs",
    ] {
        files.push(root.join("src/commands").join(f));
    }
    files
}

/// Source text that means "this line builds or dials an HTTP daemon address".
///
/// Assembled from parts so this file does not name the markers it forbids.
fn url_markers() -> Vec<String> {
    vec![
        ["http", "://"].concat(),
        ["daemon_base", "_url"].concat(),
        ["req", "west"].concat(),
        ["DaemonBridge", "Config"].concat(),
        ["78", "78"].concat(),
    ]
}

/// #9168: no line of the MCP bridge builds an `http://` URL or reaches for an
/// HTTP client. Comment lines are skipped — a doc may name the retired route.
///
/// Why: acceptance item 1 — `trusty-search serve` reaches the daemon only
/// through `DaemonClient` over the socket. The ratchet above only forbids the
/// daemon-call markers; this also forbids a literal URL, so a test mock or a
/// log line that builds one cannot creep back in.
/// What: every non-comment line of [`bridge_files`] is checked against
/// [`url_markers`]; each hit is reported as `path:line`.
/// Test: this test.
#[test]
fn the_mcp_bridge_builds_no_http_url() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let markers = url_markers();
    let mut hits = Vec::new();
    for path in bridge_files(root) {
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        for (n, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            if markers.iter().any(|m| line.contains(m.as_str())) {
                let rel = path.strip_prefix(root).unwrap_or(&path);
                hits.push(format!("{}:{}", rel.display(), n + 1));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "the MCP bridge must reach the daemon only over its socket: {hits:?}"
    );
}
