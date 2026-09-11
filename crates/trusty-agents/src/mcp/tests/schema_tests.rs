//! One MCP-server schema, mechanically (#7454, ADR-0060 decision 1).
//!
//! Why: the point of #7454 is not that the three retired schemas are unused —
//! it is that they are GONE. A type left in the tree is a type the next change
//! reaches for, which is exactly how this crate ended up with three of them.
//! A review can miss a re-added struct; a grep cannot.
//! What: scans every tracked `.rs` file under `crates/trusty-agents/src` for
//! the three retired declarations and asserts zero hits.
//! Test: this module.

use std::path::{Path, PathBuf};

/// The declarations that must not come back.
///
/// Matching is on the exact `struct <Name>` token followed by a
/// non-identifier character, so `struct McpServiceTool` (a different type
/// entirely) does not register as a hit and a re-declared `struct McpService`
/// does.
const RETIRED: &[&str] = &["McpService", "EndpointConfig", "McpJsonServer"];

/// Every `.rs` file under `dir`, recursively.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// `true` when `line` declares `struct <name>` as a whole token.
fn declares(line: &str, name: &str) -> bool {
    let needle = format!("struct {name}");
    let mut from = 0usize;
    while let Some(at) = line[from..].find(&needle) {
        let start = from + at;
        let end = start + needle.len();
        let next = line[end..].chars().next();
        // A declaration ends the name here: `struct McpService {`,
        // `struct McpService;`, `struct McpService<T>`. `struct McpServiceTool`
        // continues with an identifier character and is a different type.
        if !next.is_some_and(|c| c.is_alphanumeric() || c == '_') {
            return true;
        }
        from = end;
    }
    false
}

#[test]
fn the_three_retired_mcp_schemas_are_gone() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(!files.is_empty(), "found no sources to scan under {src:?}");

    let mut hits: Vec<String> = Vec::new();
    for file in &files {
        // This file names the retired types on purpose; scanning it would
        // make the test fail on its own documentation.
        if file.ends_with("schema_tests.rs") {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(file) else {
            continue;
        };
        for (number, line) in body.lines().enumerate() {
            for name in RETIRED {
                if declares(line, name) {
                    hits.push(format!(
                        "{}:{}: {}",
                        file.display(),
                        number + 1,
                        line.trim()
                    ));
                }
            }
        }
    }

    assert!(
        hits.is_empty(),
        "#7454 collapsed every MCP-server shape onto `trusty_mcp::config::McpServerConfig`; \
         these declarations bring a second one back:\n{}",
        hits.join("\n")
    );
}

/// The matcher itself, so a future rename cannot quietly turn the scan into a
/// no-op or into a false positive on an unrelated type.
#[test]
fn the_declaration_matcher_is_token_exact() {
    assert!(declares("pub struct McpService {", "McpService"));
    assert!(declares("struct McpService;", "McpService"));
    assert!(!declares("struct McpServiceTool {", "McpService"));
    assert!(!declares("// no McpService here", "McpService"));
}
