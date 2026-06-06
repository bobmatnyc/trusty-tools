//! Store-backed handlers for the `memory`/`code` search subcommands.
//!
//! Why: Opening the redb/usearch stack and running the search/run/sessions
//! queries is the I/O-heavy half of the command; isolating it keeps the CLI
//! parsing module pure and both files under the 500-line cap.
//! What: `open_graph`/`open_code_indexer` store openers plus the
//! `run_memory_*` and `run_code_search` handlers.
//! Test: Covered indirectly; the formatters they call are unit-tested.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};

use super::format::{
    format_code_results, format_memory_results, format_session_list, format_sessions,
};
use crate::memory::{
    CodeStore, Embedder, FastEmbedder, MemoryGraph, MemoryStore, SessionRegistry, SessionStore,
};
use crate::search::CodeIndexer;

/// Embedding dimension used throughout the project (all-MiniLM-L6-v2).
const EMBED_DIM: usize = 384;

/// Resolve the current run_id for memory reads. Defaults to `default` if
/// the env var isn't set (matches the migration-path `sessions/default/`).
fn current_run_id() -> String {
    crate::env_compat::env_var("TAGENT_RUN_ID", "OPEN_MPM_RUN_ID")
        .unwrap_or_else(|_| "default".to_string())
}

async fn open_graph(sessions_dir: &Path) -> Result<MemoryGraph> {
    std::fs::create_dir_all(sessions_dir)
        .with_context(|| format!("failed to create sessions dir: {}", sessions_dir.display()))?;
    let run_id = current_run_id();
    let store = SessionStore::open(sessions_dir, &run_id, EMBED_DIM)
        .context("failed to open session store")?;
    let embedder = FastEmbedder::new().context("failed to construct FastEmbedder")?;
    let store_arc: Arc<dyn MemoryStore> = Arc::new(store);
    let embedder_arc: Arc<dyn Embedder> = Arc::new(embedder);
    Ok(MemoryGraph::new(store_arc, embedder_arc))
}

async fn open_code_indexer(code_dir: &Path) -> Result<CodeIndexer> {
    std::fs::create_dir_all(code_dir)
        .with_context(|| format!("failed to create code dir: {}", code_dir.display()))?;
    let store = CodeStore::open(code_dir, EMBED_DIM).context("failed to open code store")?;
    let embedder = FastEmbedder::new().context("failed to construct FastEmbedder")?;
    let store_arc: Arc<dyn MemoryStore> = Arc::new(store);
    let embedder_arc: Arc<dyn Embedder> = Arc::new(embedder);
    Ok(CodeIndexer::new(store_arc, embedder_arc))
}

/// Semantic search over agent memory (current session only).
pub(super) async fn run_memory_search(
    query: &str,
    top_k: usize,
    json: bool,
    sessions_dir: &Path,
) -> Result<()> {
    let graph = open_graph(sessions_dir).await?;
    let hits = graph.search(query, top_k).await?;
    if hits.is_empty() {
        println!("No results found.");
        return Ok(());
    }
    println!("{}", format_memory_results(&hits, json)?);
    Ok(())
}

/// Retrieve all sessions in a workflow run, ordered by timestamp.
pub(super) async fn run_memory_run(run_id: &str, json: bool, sessions_dir: &Path) -> Result<()> {
    // Open the specific run_id rather than current_run_id so users can inspect
    // any prior session without needing to set TAGENT_RUN_ID.
    let store = SessionStore::open(sessions_dir, run_id, EMBED_DIM)
        .context("failed to open session store")?;
    let embedder = FastEmbedder::new().context("failed to construct FastEmbedder")?;
    let store_arc: Arc<dyn MemoryStore> = Arc::new(store);
    let embedder_arc: Arc<dyn Embedder> = Arc::new(embedder);
    let graph = MemoryGraph::new(store_arc, embedder_arc);

    let sessions = graph.get_run(run_id).await?;
    if sessions.is_empty() {
        println!("No results found.");
        return Ok(());
    }
    println!("{}", format_sessions(&sessions, json)?);
    Ok(())
}

/// List all known agent-memory sessions from the registry.
pub(super) async fn run_memory_sessions(json: bool, sessions_dir: &Path) -> Result<()> {
    if !sessions_dir.exists() {
        println!("No sessions found at {}.", sessions_dir.display());
        return Ok(());
    }
    let reg = SessionRegistry::open(sessions_dir)?;
    let sessions = reg.list()?;
    if sessions.is_empty() {
        println!("No sessions found.");
        return Ok(());
    }
    println!("{}", format_session_list(&sessions, json)?);
    Ok(())
}

/// Cross-session semantic search: merges results from every session in the
/// `sessions/` dir.
pub(super) async fn run_memory_search_all(
    query: &str,
    top_k: usize,
    json: bool,
    sessions_dir: &Path,
) -> Result<()> {
    let embedder = FastEmbedder::new().context("failed to construct FastEmbedder")?;
    let hits = MemoryGraph::search_all_sessions(sessions_dir, &embedder, query, top_k).await?;
    if hits.is_empty() {
        println!("No results found.");
        return Ok(());
    }
    println!("{}", format_memory_results(&hits, json)?);
    Ok(())
}

/// Semantic search over the code index with optional language filter.
///
/// Why: When the search daemon is running it holds an exclusive redb lock on
/// the code store, so any direct `CodeStore::open()` from the CLI crashes with
/// "Database already open." Routing through the daemon's HTTP API first
/// (mirroring `SearchCodeTool::new_auto`) avoids that contention and gives the
/// CLI the same hybrid-search quality as in-agent tool calls (#398, #402).
/// What: 1) Probes for a running daemon via `SearchDaemonClient::connect_if_running`.
/// 2) If connected, POSTs the query to `/search/query` and uses those hits
///    (note: daemon responses are already hybrid+KG; `--lang` filtering is
///    applied client-side here).
/// 3) Otherwise falls back to opening the local store directly and runs
///    `search_hybrid` (parity with the daemon path) or `search_filtered` when
///    `--lang` is supplied.
/// Test: Manual: with daemon running, `code search foo` returns hits; without
/// daemon, the same command falls back through `open_code_indexer`.
pub(super) async fn run_code_search(
    query: &str,
    top_k: usize,
    lang: Option<&str>,
    json: bool,
    code_dir: &Path,
) -> Result<()> {
    // The daemon's pid file lives at `<project_root>/.trusty-agents/state/search.pid`,
    // so the project root is the parent of the state dir (which is the parent
    // of `code_dir`). Walk up two levels: code_dir -> state -> .trusty-agents/parent.
    let project_root = code_dir
        .parent() // .trusty-agents/state
        .and_then(|p| p.parent()) // .trusty-agents
        .and_then(|p| p.parent()) // project root
        .map(Path::to_path_buf);

    if let Some(root) = project_root
        && let Some(client) =
            crate::search::service_client::SearchDaemonClient::connect_if_running(&root).await
    {
        // Pull a slightly larger pool when filtering so the post-filter
        // result count still has a chance of reaching `top_k`.
        let fetch_k = if lang.is_some() { top_k * 4 } else { top_k };
        let mut hits = client.search(query, fetch_k).await?;
        if let Some(lang_filter) = lang {
            hits.retain(|c| c.language == lang_filter);
            hits.truncate(top_k);
        }
        if hits.is_empty() {
            println!("No results found.");
            return Ok(());
        }
        println!("{}", format_code_results(&hits, json)?);
        return Ok(());
    }

    // Daemon not running — open the store directly and run hybrid search
    // locally. Hybrid search matches the daemon's behavior so the two paths
    // produce comparable results (#402).
    let indexer = open_code_indexer(code_dir).await?;
    let hits = if lang.is_some() {
        indexer.search_filtered(query, top_k, lang).await?
    } else {
        indexer.search_hybrid(query, top_k, true).await?
    };
    if hits.is_empty() {
        println!("No results found.");
        return Ok(());
    }
    println!("{}", format_code_results(&hits, json)?);
    Ok(())
}
