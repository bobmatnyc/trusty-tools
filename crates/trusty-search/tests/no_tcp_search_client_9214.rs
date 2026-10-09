//! No workspace client reaches the trusty-search daemon over TCP (#9214).
//!
//! Why: ADR-0032 makes trusty-search UDS-only, delivered in phases. This phase
//! moves every in-workspace client onto the daemon's Unix socket; the last
//! phase removes the daemon's `:7878` bind. A client that still builds
//! `http://127.0.0.1:7878`, or still resolves the daemon's HTTP address, works
//! until that bind goes and then fails only on a host running the new build.
//! A sweep over the source sees such a call site before it ships.
//!
//! What: walks every workspace crate's production source — `crates/*/src/**`
//! `.rs` files, minus test files and inline `#[cfg(test)] mod` blocks — and
//! the console's UI sources (`crates/*/ui*/src/**` `.js`/`.ts`/`.svelte`, and
//! each `vite.config.js`), skipping built `*-dist` bundles, `node_modules` and
//! `*.test.js`. A non-comment line that carries one of [`FORBIDDEN`] fails,
//! named by file and line, unless [`EXEMPT`] names that file and needle.
//! Out of scope: `crates/*/tests/**` (test rigs, not clients), `scripts/`,
//! `docker/` and docs.
//!
//! Every [`EXEMPT`] row must still match, so a row whose call site moved fails
//! the sweep until it is deleted. The last #9214 phase empties the list.
//!
//! Test: this file IS the test. `sweep_sees_the_workspace` guards against a
//! scan that silently reads nothing; `the_needles_catch_each_spelling` and
//! `test_code_and_comments_are_skipped` pin the line filter.

use std::path::{Path, PathBuf};

/// Spellings of a trusty-search client that dials TCP.
///
/// The first three are the daemon's retired default address. The rest are the
/// ways a client looked up the HTTP address the daemon publishes.
const FORBIDDEN: &[&str] = &[
    "127.0.0.1:7878",
    "localhost:7878",
    "[::1]:7878",
    "resolve_daemon_base_url(\"trusty-search\")",
    "read_daemon_addr(\"trusty-search\")",
    "DaemonAddrLayout::TRUSTY_SEARCH",
];

/// `(workspace-relative file, needle, reason)` rows allowed to keep a needle
/// until the last #9214 phase. Each row must still match.
const EXEMPT: &[(&str, &str, &str)] = &[
    (
        "crates/trusty-analyze/src/commands/run.rs",
        "127.0.0.1:7878",
        "moves in #9214 slice trusty-analyze",
    ),
    (
        "crates/trusty-analyze/src/main.rs",
        "127.0.0.1:7878",
        "moves in #9214 slice trusty-analyze",
    ),
    (
        "crates/trusty-common/src/monitor/search_client.rs",
        "127.0.0.1:7878",
        "moves in #9214 slice trusty-common",
    ),
    (
        "crates/trusty-common/src/monitor/search_client.rs",
        "read_daemon_addr(\"trusty-search\")",
        "moves in #9214 slice trusty-common",
    ),
    (
        "crates/trusty-common/src/search_index.rs",
        "resolve_daemon_base_url(\"trusty-search\")",
        "moves in #9214 slice trusty-common",
    ),
    (
        "crates/trusty-common/src/search_readiness.rs",
        "resolve_daemon_base_url(\"trusty-search\")",
        "moves in #9214 slice trusty-common",
    ),
    (
        "crates/trusty-console/ui-search/vite.config.js",
        "127.0.0.1:7878",
        "moves in #9214 slice trusty-console",
    ),
    (
        "crates/trusty-mpm/src/tui/health/types.rs",
        "127.0.0.1:7878",
        "moves in #9214 slice trusty-mpm",
    ),
    (
        "crates/trusty-review/src/integrations/search_transport.rs",
        "localhost:7878",
        "moves in #9214 slice trusty-review",
    ),
    (
        "crates/trusty-review/src/integrations/search_transport.rs",
        "DaemonAddrLayout::TRUSTY_SEARCH",
        "moves in #9214 slice trusty-review",
    ),
    (
        "crates/trusty-search/src/commands/daemon_utils.rs",
        "DaemonAddrLayout::TRUSTY_SEARCH",
        "daemon's own HTTP UI; removed with the :7878 bind (#9214 final PR)",
    ),
    (
        "crates/trusty-search/src/service/constants.rs",
        "DaemonAddrLayout::TRUSTY_SEARCH",
        "daemon's own HTTP UI; removed with the :7878 bind (#9214 final PR)",
    ),
    (
        "crates/trusty-search/src/service/daemon.rs",
        "DaemonAddrLayout::TRUSTY_SEARCH",
        "daemon's own HTTP UI; removed with the :7878 bind (#9214 final PR)",
    ),
];

/// The workspace root, two levels above this crate.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonicalize the workspace root")
}

/// A Rust test file: one the line-cap script counts as test code, or a
/// `tests_*.rs` module (declared under `#[cfg(test)]`, e.g.
/// `service/server/tests_index.rs`).
fn is_rust_test_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    name == "tests.rs"
        || name.starts_with("tests_")
        || name.ends_with("_tests.rs")
        || name.ends_with("_test.rs")
        || path.components().any(|c| c.as_os_str() == "tests")
}

/// Every file under `dir` whose extension is in `exts`, recursively, skipping
/// built bundles and dependency trees.
fn walk(dir: &Path, exts: &[&str], out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if name == "node_modules" || name.ends_with("-dist") || name == "dist" {
                continue;
            }
            walk(&path, exts, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| exts.contains(&e))
        {
            out.push(path);
        }
    }
}

/// Every swept file in the workspace.
fn swept_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let crates = std::fs::read_dir(root.join("crates")).expect("read crates/");
    for krate in crates.flatten() {
        let dir = krate.path();
        let mut rust = Vec::new();
        walk(&dir.join("src"), &["rs"], &mut rust);
        files.extend(rust.into_iter().filter(|p| !is_rust_test_file(p)));
        let Ok(children) = std::fs::read_dir(&dir) else {
            continue;
        };
        for child in children.flatten() {
            let name = child.file_name().to_string_lossy().into_owned();
            if !name.starts_with("ui") || name.ends_with("-dist") || !child.path().is_dir() {
                continue;
            }
            let vite = child.path().join("vite.config.js");
            if vite.is_file() {
                files.push(vite);
            }
            let mut ui = Vec::new();
            walk(&child.path().join("src"), &["js", "ts", "svelte"], &mut ui);
            files.extend(ui.into_iter().filter(|p| {
                let n = p.to_string_lossy();
                !n.ends_with(".test.js") && !n.ends_with(".test.ts")
            }));
        }
    }
    files.sort();
    files
}

/// A whole-line comment in Rust, JS or Svelte: prose about history may name
/// the retired address.
fn is_comment(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("//") || t.starts_with("/*") || t.starts_with('*') || t.starts_with("<!--")
}

/// `(1-based line, needle, text)` for every non-comment, non-test line in
/// `text` that carries a [`FORBIDDEN`] needle. A Rust `#[cfg(test)]` followed
/// by a `mod … {` opens an inline test module, which runs to the end of the
/// file by this workspace's convention.
fn hits(text: &str) -> Vec<(usize, &'static str, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.trim() == "#[cfg(test)]" {
            let next = lines[i + 1..].iter().find(|l| !l.trim().is_empty());
            if next.is_some_and(|l| {
                let t = l.trim_start();
                (t.starts_with("mod ") || t.starts_with("pub mod ")) && t.trim_end().ends_with('{')
            }) {
                break;
            }
        }
        if is_comment(line) {
            continue;
        }
        if let Some(needle) = FORBIDDEN.iter().find(|n| line.contains(*n)) {
            out.push((i + 1, *needle, line.trim().to_string()));
        }
    }
    out
}

/// The sweep: no unexempted hit, and no exemption without a hit.
#[test]
fn no_workspace_client_dials_the_search_daemon_over_tcp() {
    let root = workspace_root();
    let mut violations = Vec::new();
    let mut used = vec![false; EXEMPT.len()];
    for file in swept_files(&root) {
        let rel = file
            .strip_prefix(&root)
            .expect("under the workspace root")
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        for (line, needle, code) in hits(&text) {
            match EXEMPT
                .iter()
                .position(|(f, n, _)| *f == rel && *n == needle)
            {
                Some(i) => used[i] = true,
                None => violations.push(format!("{rel}:{line}: `{needle}`: {code}")),
            }
        }
    }
    assert!(
        violations.is_empty(),
        "these lines reach the trusty-search daemon over TCP; use its Unix socket \
         (`trusty_common::search_rpc`) instead (#9214, ADR-0032):\n{}",
        violations.join("\n")
    );
    let stale: Vec<String> = EXEMPT
        .iter()
        .zip(&used)
        .filter(|(_, u)| !**u)
        .map(|((f, n, _), _)| format!("{f} `{n}`"))
        .collect();
    assert!(
        stale.is_empty(),
        "these EXEMPT rows no longer match; delete them: {stale:?}"
    );
}

/// The walk reaches the workspace: a sweep that reads nothing passes vacuously.
#[test]
fn sweep_sees_the_workspace() {
    let root = workspace_root();
    let files = swept_files(&root);
    let rel = |p: &PathBuf| {
        p.strip_prefix(&root)
            .map(|r| r.to_string_lossy().replace('\\', "/"))
    };
    let names: Vec<String> = files.iter().filter_map(|p| rel(p).ok()).collect();
    for expected in [
        "crates/trusty-common/src/search_rpc.rs",
        "crates/trusty-mpm/src/lib.rs",
        "crates/trusty-console/ui-search/vite.config.js",
        "crates/trusty-console/ui-search/src/lib/base.js",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "the sweep missed {expected}"
        );
    }
    assert!(
        !names
            .iter()
            .any(|n| n.contains("-dist/") || n.ends_with("_tests.rs") || n.contains("/tests_")),
        "the sweep read a built bundle or a test file"
    );
}

/// Each needle is caught in code, in Rust and JS alike.
#[test]
fn the_needles_catch_each_spelling() {
    for needle in FORBIDDEN {
        let line = format!("    let x = f(\"{needle}\");");
        assert_eq!(hits(&line).len(), 1, "missed {needle}");
    }
    assert_eq!(hits("      '/health': 'http://127.0.0.1:7878',").len(), 1);
}

/// Comments and an inline test module are not client code.
#[test]
fn test_code_and_comments_are_skipped() {
    let text = "/// was http://127.0.0.1:7878\n\
                // localhost:7878\n\
                 * http://127.0.0.1:7878/ui\n\
                fn live() {}\n\
                #[cfg(test)]\n\
                mod tests {\n\
                    const U: &str = \"http://127.0.0.1:7878\";\n\
                }\n";
    assert!(hits(text).is_empty(), "{:?}", hits(text));
    // A `#[cfg(test)]` on a single item does not end the scan.
    let item = "#[cfg(test)]\nfn helper() {}\nconst U: &str = \"http://localhost:7878\";\n";
    assert_eq!(hits(item).len(), 1);
}
