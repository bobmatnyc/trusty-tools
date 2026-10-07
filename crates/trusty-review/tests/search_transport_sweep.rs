//! Keep trusty-search's HTTP address inside the HTTP leg (#9214).
//!
//! Why: phase B5 moves trusty-review's trusty-search clients onto the Unix
//! socket and keeps HTTP only as a marked leg that phase C deletes. A new
//! `:7878` literal outside that leg would survive phase C and fail only on a
//! machine whose daemon no longer listens on TCP.
//! What: scans this crate's production `src/` (test files excluded by the
//! line-cap script's own naming rules) for `:7878` in non-comment lines, and
//! fails on any that does not carry the `#9214 phase C: delete` marker.
//! Test: this file IS the test. `sweep_sees_the_source_tree` guards against
//! the scan silently matching nothing.

use std::path::{Path, PathBuf};

/// The HTTP listener's port, as a literal.
const NEEDLE: &str = ":7878";

/// The marker every HTTP-leg line carries.
const PHASE_C: &str = "#9214 phase C: delete";

fn is_test_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    name == "tests.rs" || name.ends_with("_tests.rs") || name.ends_with("_test.rs")
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") && !is_test_file(&path) {
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

/// Every `file:line` whose code carries the port without the phase-C marker.
fn violations(name: &str, text: &str) -> Vec<String> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
        .filter(|(_, line)| line.contains(NEEDLE) && !line.contains(PHASE_C))
        .map(|(n, line)| format!("{name}:{}: {}", n + 1, line.trim()))
        .collect()
}

#[test]
fn no_trusty_search_port_literal_outside_the_http_leg() {
    let mut found = Vec::new();
    for file in sources() {
        let text = std::fs::read_to_string(&file).expect("read a source file");
        found.extend(violations(&file.display().to_string(), &text));
    }
    assert!(
        found.is_empty(),
        "trusty-review reaches trusty-search through `SearchTransport` (#9214); a \
         `:7878` literal belongs only on a line marked `{PHASE_C}`:\n{}",
        found.join("\n")
    );
}

#[test]
fn sweep_sees_the_source_tree() {
    assert!(
        sources().len() > 100,
        "the scan found almost no files; the manifest path is wrong"
    );
    let planted = "let url = \"http://127.0.0.1:7878\".to_string();";
    assert_eq!(violations("planted.rs", planted).len(), 1);
    assert!(violations("planted.rs", "// the old :7878 listener").is_empty());
    let marked = "const U: &str = \"http://localhost:7878\"; // #9214 phase C: delete";
    assert!(violations("planted.rs", marked).is_empty());
}
