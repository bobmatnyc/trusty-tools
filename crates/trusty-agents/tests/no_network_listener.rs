//! Keep trusty-agents free of network listeners (#11632).
//!
//! Why: the HTTP API (`tagent --api`, `--serve --port`) was cut; the only
//! serving surface left is the stdio MCP server, and the UDS API that follows
//! must not reopen a TCP port. ADR-0032 makes the Console the only HTTP
//! surface. One `TcpListener::bind` copied back from history would quietly
//! put a port on the host again.
//! What: two guards. A source scan over `src/` fails, naming file and line, on
//! any non-comment spelling of a network listener outside test code (a
//! `tests.rs`/`*_tests.rs` file, a `tests/` directory, or the brace block of
//! a `#[cfg(test)]` item). And the built binary refuses the
//! removed `--api`/`--serve`/`--port` flags with a non-zero exit.
//! Test: this file IS the test. `scan_catches_a_planted_bind` shows the scan
//! can fail.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Spellings of a network listener.
const FORBIDDEN: &[&str] = &[
    "TcpListener",
    "TcpSocket",
    "UdpSocket",
    "axum::serve",
    "hyper::server",
    "0.0.0.0",
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

/// Whether `path` is a test-only source file by this crate's naming rules.
fn is_test_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name == "tests.rs"
        || name.ends_with("_tests.rs")
        || path.components().any(|c| c.as_os_str() == "tests")
}

/// The production part of `text`: every line except the brace block of a
/// `#[cfg(test)]` item.
///
/// Why: a test module may sit anywhere in a file, and a `#[cfg(test)] mod x;`
/// declaration has no body, so the code after it is production code and must
/// still be scanned (#11632 fix round).
/// What: on a `#[cfg(test)]` line, skips any further attribute lines, then the
/// item they decorate. An item ending in `;` before any `{` skips only its own
/// lines; otherwise lines are skipped until the braces opened by the item
/// balance. Skipped lines become empty, so line numbers stay true.
/// Test: `scan_catches_a_planted_bind`.
fn production_part(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() != "#[cfg(test)]" {
            out.push(lines[i]);
            i += 1;
            continue;
        }
        let mut depth: i64 = 0;
        let mut opened = false;
        while i < lines.len() {
            let line = lines[i];
            out.push("");
            i += 1;
            let trimmed = line.trim();
            if trimmed.starts_with("#[") && !opened {
                continue;
            }
            for c in line.chars() {
                match c {
                    '{' => {
                        depth += 1;
                        opened = true;
                    }
                    '}' => depth -= 1,
                    _ => {}
                }
            }
            if (!opened && trimmed.ends_with(';')) || (opened && depth <= 0) {
                break;
            }
        }
    }
    out.join("\n")
}

/// Every `file:line` in the production part of `text` whose non-comment
/// content names a network listener.
fn violations(name: &str, text: &str) -> Vec<String> {
    production_part(text)
        .lines()
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

fn sources() -> Vec<PathBuf> {
    let mut files = Vec::new();
    rust_sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    files
}

#[test]
fn no_network_listener_outside_test_code() {
    let mut found = Vec::new();
    for file in sources() {
        if is_test_file(&file) {
            continue;
        }
        let text = std::fs::read_to_string(&file).expect("read source");
        found.extend(violations(&file.display().to_string(), &text));
    }
    assert!(
        found.is_empty(),
        "trusty-agents serves stdio MCP only; no TCP/UDP listener outside test \
         code (#11632, ADR-0032):\n{}",
        found.join("\n")
    );
}

#[test]
fn scan_catches_a_planted_bind() {
    assert!(
        sources().len() > 100,
        "the scan found almost no files; the manifest path is wrong"
    );
    let planted = "let l = tokio::net::TcpListener::bind(\"127.0.0.1:8080\").await?;";
    assert_eq!(violations("planted.rs", planted).len(), 1);
    assert!(violations("planted.rs", "// TcpListener::bind was removed").is_empty());
    let in_tests = format!("fn a() {{}}\n#[cfg(test)]\nmod tests {{\n{planted}\n}}\n");
    assert!(violations("planted.rs", &in_tests).is_empty());
    assert!(is_test_file(Path::new("src/channels/migrate_tests.rs")));
    // A `mod tests;` declaration skips nothing: the code after it is production.
    let after_decl = "#[cfg(test)]\nmod tests;\nfn f() { UdpSocket::bind(..) }";
    assert_eq!(violations("planted.rs", after_decl).len(), 1);
}

#[test]
fn removed_api_flags_are_refused() {
    for argv in [
        &["--api", "--port", "7654"][..],
        &["--serve", "--port", "8080"][..],
    ] {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tagent"));
        trusty_common::parent_death::exit_with_parent(&mut cmd);
        let out = cmd
            .args(argv)
            .current_dir(tmp.path())
            .env("HOME", tmp.path())
            .output()
            .expect("spawn tagent");
        assert!(
            !out.status.success(),
            "`tagent {argv:?}` must fail now that the HTTP API is gone"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("#11632"),
            "`tagent {argv:?}` must say why it refused; stderr:\n{stderr}"
        );
    }
}
