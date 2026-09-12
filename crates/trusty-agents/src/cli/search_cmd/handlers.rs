//! Code index query handlers. Memory commands use trusty-memory (#7360).
use super::format::format_code_results;
use crate::memory::{CodeStore, Embedder, FastEmbedder, MemoryStore};
use crate::search::CodeIndexer;
use anyhow::{Context, Result};
use std::{path::Path, sync::Arc};
const EMBED_DIM: usize = 384;
async fn open_code_indexer(code_dir: &Path) -> Result<CodeIndexer> {
    std::fs::create_dir_all(code_dir)
        .with_context(|| format!("failed to create code dir: {}", code_dir.display()))?;
    let store = CodeStore::open(code_dir, EMBED_DIM).context("failed to open code store")?;
    let embedder = FastEmbedder::new().context("failed to construct FastEmbedder")?;
    let store_arc: Arc<dyn MemoryStore> = Arc::new(store);
    let embedder_arc: Arc<dyn Embedder> = Arc::new(embedder);
    Ok(CodeIndexer::new(store_arc, embedder_arc))
}

/// Semantic search over the code index with optional language filter.
///
/// Why: When the search daemon is running it holds an exclusive redb lock on
/// the code store, so any direct `CodeStore::open()` from the CLI crashes with
/// "Database already open." Routing through the daemon's socket first
/// (mirroring `SearchCodeTool::new_auto`) avoids that contention and gives the
/// CLI the same hybrid-search quality as in-agent tool calls (#398, #402).
/// What: 1) Probes for a running daemon via `SearchDaemonClient::connect_if_running`.
/// 2) If connected, calls `search.query` over the socket and uses those hits
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
    // #6433: the daemon's socket path is derived from the project root
    // (`search_socket_path`), so the root is what the client needs. It is the
    // parent of the state dir, which is the parent of `code_dir`. Walk up:
    // code_dir -> state -> .trusty-agents -> project root.
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
