//! Ratchet: no NEW CLI or MCP-bridge file may dial the daemon over HTTP (#6285).
//!
//! Why: the consumer move lands in steps, and a later edit that reaches for
//! `daemon_base_url()` or a `reqwest` client re-opens a path the retire slice
//! is about to delete. A sweep over the source is the only check that sees a
//! call site before it runs.
//! What: every `.rs` file under `src/commands/`, `src/mcp/`, plus `src/main.rs`
//! is read; a file that names one of [`HTTP_MARKERS`] must be listed in
//! [`NOT_YET_MOVED`], and a listed file that no longer names one must be
//! removed from the list. The list only shrinks; the step that moves the last
//! file deletes it and this test then forbids every marker outright.
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
];

/// Files that still dial HTTP, relative to the crate root. Remove a row when
/// its file moves onto `service::daemon_client`; never add one.
///
/// `src/main.rs` and `src/commands/dashboard.rs` open the browser UI at
/// `<base>/ui`, which is not a daemon call and moves with #6155's UI carve-out.
/// `src/commands/serve.rs`, `serve_scope.rs` and `src/mcp/**` are the MCP
/// bridge, the next step of #6285.
const NOT_YET_MOVED: &[&str] = &[
    "src/commands/add.rs",
    "src/commands/cleanup.rs",
    "src/commands/config.rs",
    "src/commands/convert.rs",
    "src/commands/daemon_utils.rs",
    "src/commands/dashboard.rs",
    "src/commands/discover/http.rs",
    "src/commands/discover/mod.rs",
    "src/commands/doctor.rs",
    "src/commands/doctor_checks/mod.rs",
    "src/commands/doctor_pipeline.rs",
    "src/commands/explicit_target.rs",
    "src/commands/index.rs",
    "src/commands/index_cwd_resolve.rs",
    "src/commands/index_relocate.rs",
    "src/commands/index_remove.rs",
    "src/commands/index_remove_stale.rs",
    "src/commands/index_status.rs",
    "src/commands/list.rs",
    "src/commands/migrate.rs",
    "src/commands/query.rs",
    "src/commands/reindex.rs",
    "src/commands/reindex_engine/driver.rs",
    "src/commands/reindex_engine/file_ops.rs",
    "src/commands/reindex_engine/registration.rs",
    "src/commands/reindex_engine/tests.rs",
    "src/commands/reindex_engine/verify.rs",
    "src/commands/remove.rs",
    "src/commands/serve.rs",
    "src/commands/serve_scope.rs",
    "src/commands/start/tests.rs",
    "src/commands/status.rs",
    "src/commands/watch.rs",
    "src/main.rs",
    "src/mcp/tools/health.rs",
    "src/mcp/tools/http.rs",
    "src/mcp/tools/mod.rs",
    "src/mcp/tools/tests.rs",
    "src/mcp/tools/tests_unavailable.rs",
    "src/mcp/tools/unavailable.rs",
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

/// A file outside [`NOT_YET_MOVED`] that dials HTTP fails; so does a listed
/// file that no longer does.
#[test]
fn no_cli_or_bridge_file_reintroduces_an_http_daemon_call() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let hits = files_dialling_http(root);

    let new: Vec<&String> = hits
        .iter()
        .filter(|f| !NOT_YET_MOVED.contains(&f.as_str()))
        .collect();
    assert!(
        new.is_empty(),
        "these files reach the daemon over HTTP; use service::daemon_client instead: {new:?}"
    );

    let moved: Vec<&&str> = NOT_YET_MOVED
        .iter()
        .filter(|f| !hits.iter().any(|h| h == *f))
        .collect();
    assert!(
        moved.is_empty(),
        "these files no longer dial HTTP; delete their NOT_YET_MOVED rows: {moved:?}"
    );
}

/// The quantize command — this step's one moved subcommand — stays moved.
#[test]
fn the_quantize_command_dials_no_http() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(!files_dialling_http(root).contains(&"src/commands/quantize.rs".to_string()));
}
