use super::*;
use crate::core::indexer::CodeIndexer;
use std::fs;
use std::sync::atomic::Ordering;

/// Build an IndexHandle with only the `lexical_only` and `skip_kg` flags set.
///
/// Why: the staged-pipeline tests need to flip these flags independently
/// without duplicating the full IndexHandle construction boilerplate.
/// What: constructs an `Arc<IndexHandle>` with the given flags; pre-sets
/// `stages` based on which pipeline phases are enabled.
/// Test: used by stage, skip_kg, and last_indexed tests.
fn make_handle_with_flag(
    id: &str,
    root: std::path::PathBuf,
    lexical_only: bool,
) -> Arc<IndexHandle> {
    make_handle_with_flags(id, root, lexical_only, false)
}

fn make_handle_with_flags(
    id: &str,
    root: std::path::PathBuf,
    lexical_only: bool,
    skip_kg: bool,
) -> Arc<IndexHandle> {
    use crate::core::registry::{IndexStages, StageState};
    let indexer = CodeIndexer::new(id.to_string(), root.clone());
    let stages = if lexical_only {
        IndexStages {
            lexical: StageState::pending(),
            semantic: StageState::skipped(),
            graph: StageState::skipped(),
        }
    } else if skip_kg {
        IndexStages {
            lexical: StageState::pending(),
            semantic: StageState::pending(),
            graph: StageState::skipped(),
        }
    } else {
        IndexStages::default()
    };
    Arc::new(IndexHandle {
        id: IndexId::new(id),
        indexer: Arc::new(tokio::sync::RwLock::new(indexer)),
        root_path: root,
        include_paths: vec![],
        exclude_globs: vec![],
        extensions: vec![],
        domain_terms: vec![],
        include_docs: false,
        respect_gitignore: true,
        path_filter: vec![],
        context_embedding: Arc::new(tokio::sync::RwLock::new(None)),
        context_summary: Arc::new(tokio::sync::RwLock::new(None)),
        indexed_head_sha: Arc::new(tokio::sync::RwLock::new(None)),
        last_indexed_at: Arc::new(tokio::sync::RwLock::new(None)),
        lexical_only,
        skip_kg,
        defer_embed: false,
        stages: Arc::new(tokio::sync::RwLock::new(stages)),
        search_pressure: Arc::new(tokio::sync::Notify::new()),
        walk_diagnostics: Arc::new(tokio::sync::RwLock::new(
            crate::core::registry::WalkDiagnostics::default(),
        )),
    })
}

mod context;
mod corpus;
mod semaphore;
mod stages;
mod walk_filter;
