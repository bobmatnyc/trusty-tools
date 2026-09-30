//! Keep trusty-agents off trusty-search's retired HTTP listener (#6285).
//!
//! Why: the trusty-search daemon is moving to a Unix socket only (ADR-0032).
//! This crate's call sites were moved onto `trusty_common::search_rpc` in
//! #6285's consumer step, and the retire PR removes the TCP bind. One new
//! `reqwest` call to `:7878`, or one `resolve_daemon_base_url("trusty-search")`
//! copied from an old example, would work until that PR lands and then fail
//! only on a machine running the retired build.
//! What: scans this crate's own `src/` for the spellings an HTTP call to
//! trusty-search takes and fails, naming file and line, on any non-comment
//! match. Literal substrings, deliberately: no Rust parser, and the spellings
//! below are the ones the removed call sites used.
//! Test: this file IS the test. `sweep_sees_the_source_tree` guards against
//! the scan silently matching nothing; the red proof is a planted line.

use std::path::{Path, PathBuf};

/// Spellings of an HTTP call to trusty-search.
const FORBIDDEN: &[&str] = &[
    "resolve_daemon_base_url(\"trusty-search\")",
    "probe(\"trusty-search\"",
    ":7878",
    "search_base",
    "/indexes/",
    "monitor::search_client",
    "HttpIndexFeed",
];

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn sources() -> Vec<PathBuf> {
    let mut files = Vec::new();
    rust_sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    files
}

/// Every `file:line` in `text` whose non-comment content carries a forbidden
/// spelling. A whole-line comment (`//`, `///`, `//!`) is prose about history
/// and is allowed to name the retired route.
fn violations(name: &str, text: &str) -> Vec<String> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
        .filter_map(|(n, line)| {
            FORBIDDEN
                .iter()
                .find(|needle| line.contains(*needle))
                .map(|needle| format!("{name}:{}: `{needle}`: {}", n + 1, line.trim()))
        })
        .collect()
}

#[test]
fn no_http_call_to_trusty_search_in_src() {
    let mut found = Vec::new();
    for file in sources() {
        let text = std::fs::read_to_string(&file).unwrap();
        found.extend(violations(&file.display().to_string(), &text));
    }
    assert!(
        found.is_empty(),
        "trusty-agents must reach trusty-search over its Unix socket \
         (`trusty_common::search_rpc`), never HTTP (#6285):\n{}",
        found.join("\n")
    );
}

#[test]
fn sweep_sees_the_source_tree() {
    assert!(
        sources().len() > 100,
        "the scan found almost no files; the manifest path is wrong"
    );
    let planted = "let url = format!(\"{base}/indexes/{id}/status\");";
    assert_eq!(violations("planted.rs", planted).len(), 1);
    assert!(violations("planted.rs", "// GET /indexes/{id}/status").is_empty());
}
