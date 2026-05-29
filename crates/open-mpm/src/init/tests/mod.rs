//! Unit tests for the `init` module (scan, render, lifecycle, and seeders).
//!
//! Why: These tests hold `HOME_LOCK` (a `std::sync::Mutex`) across async
//! I/O to serialize global $HOME mutation between tests. The lock is
//! intentionally held for the full test body; see `crate::test_env` for
//! the rationale. The tokio multi-threaded test runtime keeps this from
//! deadlocking.
//!
//! Layout: `mod.rs` covers scan/render/lifecycle plus shared test fixtures;
//! `seed_tests.rs` covers the three memory seeders + `seed_all`.
#![allow(clippy::await_holding_lock)]

mod mcp_tests;
mod seed_tests;

use std::path::PathBuf;

use anyhow::Result;
use chrono::Utc;
use tempfile::tempdir;

use crate::init::scan::render_index_markdown;
use crate::init::{IndexEntry, InitContext, ProjectIndex, ProjectInitializer};

/// Build a minimal project tree (src/lib.rs + Cargo.toml + README.md).
///
/// Why: Several lifecycle tests need the same on-disk fixture.
/// What: Returns the tempdir guard, project root, and `.open-mpm` state dir.
/// Test: Used by the scan/lifecycle tests in this module.
async fn fixture_project() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let omd = root.join(".open-mpm");
    tokio::fs::create_dir_all(&root.join("src")).await.unwrap();
    tokio::fs::write(
        root.join("src/lib.rs"),
        "//! Example library crate root.\nfn main() {}\n",
    )
    .await
    .unwrap();
    tokio::fs::write(root.join("Cargo.toml"), "[package]\nname = \"x\"\n")
        .await
        .unwrap();
    tokio::fs::write(root.join("README.md"), "# Hello\nProject docs.\n")
        .await
        .unwrap();
    (tmp, root, omd)
}

/// Stub embedder shared by the skill / MCP / seed_all tests so we don't
/// need to load the real ONNX model in CI.
pub(super) struct StubEmbedderShared {
    pub(super) dim: usize,
}
impl crate::memory::Embedder for StubEmbedderShared {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = vec![0.0f32; self.dim];
                for (i, b) in t.bytes().take(self.dim).enumerate() {
                    v[i] = (b as f32) / 255.0;
                }
                v
            })
            .collect())
    }
    fn embed_single(&self, text: &str) -> Result<Vec<f32>> {
        Ok(self.embed(&[text])?.remove(0))
    }
    fn dimension(&self) -> usize {
        self.dim
    }
}

#[tokio::test]
async fn scan_project_finds_source_files() {
    let (_g, root, omd) = fixture_project().await;
    let init = ProjectInitializer::new(&root, &omd);
    let idx = init.scan_project().await.unwrap();
    assert!(idx.entries.iter().any(|e| e.rel_path.ends_with("lib.rs")));
    assert!(idx.entries.iter().any(|e| e.rel_path == "Cargo.toml"));
    assert!(idx.entries.iter().any(|e| e.rel_path == "README.md"));
}

#[tokio::test]
async fn render_markdown_groups_by_kind() {
    let idx = ProjectIndex {
        project_name: "demo".into(),
        entries: vec![
            IndexEntry {
                rel_path: "src/lib.rs".into(),
                summary: "Entry".into(),
            },
            IndexEntry {
                rel_path: "Cargo.toml".into(),
                summary: "Manifest".into(),
            },
            IndexEntry {
                rel_path: "README.md".into(),
                summary: "Docs".into(),
            },
        ],
    };
    let md = render_index_markdown(&idx, Utc::now());
    assert!(md.contains("# Project: demo"));
    assert!(md.contains("## Source Structure"));
    assert!(md.contains("## Config"));
    assert!(md.contains("## Docs"));
    assert!(md.contains("src/lib.rs"));
}

#[tokio::test]
async fn initialize_if_needed_creates_index_and_marker() {
    let (_g, root, omd) = fixture_project().await;
    let init = ProjectInitializer::new(&root, &omd);
    let ctx = init.initialize_if_needed().await.unwrap();
    assert!(ctx.project_summary.contains("# Project:"));
    assert!(omd.join("project-index.md").exists());
    assert!(omd.join("initialized").exists());
}

#[tokio::test]
async fn initialize_if_needed_skips_when_fresh() {
    let (_g, root, omd) = fixture_project().await;
    let init = ProjectInitializer::new(&root, &omd);
    let first = init.initialize_if_needed().await.unwrap();
    // Mutate the written index and confirm the second call doesn't
    // regenerate it (proves cache hit).
    tokio::fs::write(omd.join("project-index.md"), b"CACHED_CONTENT")
        .await
        .unwrap();
    let second = init.initialize_if_needed().await.unwrap();
    assert_eq!(second.project_summary, "CACHED_CONTENT");
    assert_eq!(second.initialized_at, first.initialized_at);
}

#[tokio::test]
async fn force_reinitialize_rewrites_index() {
    let (_g, root, omd) = fixture_project().await;
    let init = ProjectInitializer::new(&root, &omd);
    let _ = init.initialize_if_needed().await.unwrap();
    tokio::fs::write(omd.join("project-index.md"), b"STALE")
        .await
        .unwrap();
    let fresh = init.force_reinitialize().await.unwrap();
    assert!(fresh.project_summary.contains("# Project:"));
    assert!(!fresh.project_summary.contains("STALE"));
}

#[tokio::test]
async fn prompt_prefix_includes_both_sections() {
    let ctx = InitContext {
        project_summary: "# Project: x".into(),
        relevant_memories: vec!["m1".into(), "m2".into()],
        initialized_at: Utc::now(),
    };
    let p = ctx.to_prompt_prefix();
    assert!(p.contains("## Project Context"));
    assert!(p.contains("## Relevant Prior Knowledge"));
    assert!(p.contains("- m1"));
    assert!(p.contains("- m2"));
    assert!(p.ends_with("---\n\n"));
}

#[tokio::test]
async fn prompt_prefix_empty_when_both_blank() {
    let ctx = InitContext {
        project_summary: String::new(),
        relevant_memories: vec![],
        initialized_at: Utc::now(),
    };
    assert!(ctx.to_prompt_prefix().is_empty());
}
