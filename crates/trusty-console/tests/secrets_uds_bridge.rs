//! The secrets bridge's dial path and dependency shape (#9066, plan T22, A8).
//!
//! Why: the bridge must reach trusty-secrets over its Unix socket only, and
//! the console must link trusty-secrets' wire vocabulary and ancestry walk
//! only — never `server` or `store`, which would put a keychain and a second
//! trusty-common edge into the console (owner ruling Df0).
//! What: a source scan of `src/secrets_uds/**` for any HTTP client or URL,
//! and a manifest read of the console's `trusty-secrets` entry.
//! Test: this file IS the test.

use std::path::Path;

fn manifest_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Every `.rs` file under `dir`, recursively.
fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read_dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Why: a bridge that built a URL could be pointed at the network; this one
/// only ever dials a socket path.
/// Test: this is the test.
#[test]
fn the_secrets_bridge_builds_no_http_url() {
    let mut files = Vec::new();
    rust_files(&manifest_dir().join("src/secrets_uds"), &mut files);
    assert!(files.len() >= 6, "found {files:?}");
    for file in files {
        // #9066: test files (`tests.rs`, `*_tests.rs`) build requests on purpose.
        let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == "tests.rs" || name.ends_with("_tests.rs") {
            continue;
        }
        let source = std::fs::read_to_string(&file).expect("read");
        for banned in ["reqwest", "http://", "https://", "hyper::client"] {
            assert!(
                !source.contains(banned),
                "{} names {banned}",
                file.display()
            );
        }
    }
}

/// Why: Df0 — `api` + `ancestry` only, so no keyring, fd-lock or
/// trusty-common reaches the console through trusty-secrets.
/// Test: this is the test; `cargo tree -p trusty-console -e normal -i keyring`
/// is the build-graph check recorded on the PR.
#[test]
fn the_console_links_trusty_secrets_with_api_and_ancestry_only() {
    let manifest = std::fs::read_to_string(manifest_dir().join("Cargo.toml")).expect("manifest");
    let entries: Vec<&str> = manifest
        .lines()
        .filter(|line| line.trim_start().starts_with("trusty-secrets"))
        .collect();
    assert_eq!(entries.len(), 1, "one trusty-secrets entry: {entries:?}");
    let entry = entries[0];
    assert!(entry.contains("workspace = true"), "{entry}");
    let features = entry
        .split("features = [")
        .nth(1)
        .and_then(|rest| rest.split(']').next())
        .expect("a features list");
    let mut features: Vec<&str> = features
        .split(',')
        .map(|f| f.trim().trim_matches('"'))
        .filter(|f| !f.is_empty())
        .collect();
    features.sort_unstable();
    assert_eq!(features, ["ancestry", "api"]);
    assert!(!entry.contains("default-features = true"), "{entry}");
    let root = std::fs::read_to_string(manifest_dir().join("../../Cargo.toml")).expect("root");
    let workspace_entry = root
        .lines()
        .find(|line| line.starts_with("trusty-secrets ="))
        .expect("workspace entry");
    assert!(
        workspace_entry.contains("default-features = false"),
        "{workspace_entry}"
    );
}
