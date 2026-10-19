//! A live check of the LSP client against a real `rust-analyzer` (#9078, L2).
//!
//! Why: the fake server in `tests/lsp_child.rs` proves the protocol and the
//! process lifecycle, but not that a real server accepts this client's
//! `initialize` and answers a request. CI has no language server (DOC-72 §12),
//! so this test is ignored and run by hand:
//! `cargo test -p trusty-analyze --test lsp_rust_analyzer -- --include-ignored`.
//! What: a two-function crate in a tempdir; one `textDocument/references` call
//! against `rust-analyzer` on `PATH`, retried while the server loads the
//! workspace; then the child must be gone after `shutdown_all`.
//! Test: this file is the test.
#![cfg(unix)]

use std::time::{Duration, Instant};

use serde_json::json;
use trusty_analyze::lsp::{LspConfig, LspError, LspLanguage, LspManager};

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs rust-analyzer on PATH; CI has no language server (DOC-72 §12)"]
async fn real_rust_analyzer_answers_references() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("ws");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"ws\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    let lib = root.join("src/lib.rs");
    std::fs::write(&lib, "pub fn foo() {}\n\npub fn bar() {\n    foo();\n}\n").unwrap();

    let manager = LspManager::new(LspConfig {
        enabled: true,
        initialize_timeout: Duration::from_secs(60),
        request_timeout: Duration::from_secs(60),
        ..LspConfig::default()
    });
    let lease = manager
        .session(&root, LspLanguage::Rust)
        .await
        .expect("rust-analyzer starts");
    let pid = lease.pid();
    let doc = lease.client().sync_document(&lib, "rust").await.unwrap();
    let params = json!({"textDocument": {"uri": doc.uri}, "position": {"line": 0, "character": 8},
                        "context": {"includeDeclaration": false}});

    let deadline = Instant::now() + Duration::from_secs(120);
    let refs = loop {
        // While it loads the workspace, rust-analyzer answers `-32801 content
        // modified`; that, like an empty list, means "ask again".
        let refs = match lease
            .request("textDocument/references", params.clone())
            .await
        {
            Err(LspError::ServerError { code: -32801, .. }) => json!([]),
            other => other.expect("references"),
        };
        if refs.as_array().is_some_and(|a| !a.is_empty()) || Instant::now() > deadline {
            break refs;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    assert_eq!(refs[0]["range"]["start"]["line"], 3, "{refs}");

    drop(lease);
    manager.shutdown_all().await;
    // SAFETY: signal 0 only probes for existence.
    assert!(
        unsafe { libc::kill(pid as libc::pid_t, 0) } != 0,
        "rust-analyzer leaked"
    );
}
