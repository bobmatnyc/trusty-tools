//! Implementation of `memory search`, `memory run`, and `code search` CLI subcommands.
//!
//! Why: Local store inspection needs a no-API-key, fast entry point. Humans
//! debug workflows with `memory run <run_id>`; agents recall prior sessions
//! with `memory search <query>`; code navigation uses `code search <query>`.
//! What: Parses argv into a `Command` enum (pure, testable), then executes
//! against a `RedbUsearchStore`+`FastEmbedder`+`MemoryGraph`/`CodeIndexer` stack.
//! Test: Unit tests cover `parse_args` for each form plus the human/JSON
//! formatters for memory and code results.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use crate::memory::{
    AgentSession, CodeStore, Embedder, FastEmbedder, MemoryGraph, MemoryResult, MemoryStore,
    SessionMeta, SessionRegistry, SessionStore,
};
use crate::search::{CodeChunk, CodeIndexer};

/// Embedding dimension used throughout the project (all-MiniLM-L6-v2).
const EMBED_DIM: usize = 384;

/// Default number of hits returned when `--top-k` is not supplied.
const DEFAULT_TOP_K: usize = 5;

/// Parsed CLI command. Constructed from `parse_args` and consumed by the
/// dispatcher; exposing this as an enum keeps parsing pure and testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    MemorySearch {
        query: String,
        top_k: usize,
        json: bool,
    },
    MemoryRun {
        run_id: String,
        json: bool,
    },
    MemorySessions {
        json: bool,
    },
    MemorySearchAll {
        query: String,
        top_k: usize,
        json: bool,
    },
    CodeSearch {
        query: String,
        top_k: usize,
        lang: Option<String>,
        json: bool,
    },
}

/// Clap front-end for `memory ...` and `code ...` subcommands.
///
/// Why: Replaces hand-rolled flag scanning with derive-based clap so help
/// text and error messages are generated automatically.
/// What: Two top-level subcommand groups (`memory`, `code`) each with their
/// own action subcommand. Converted to the public `Command` enum below.
/// Test: All existing `parse_args` unit tests still pass via this path.
#[derive(Debug, Parser)]
#[command(no_binary_name = true)]
struct SearchCli {
    #[command(subcommand)]
    cmd: SearchGroup,
}

#[derive(Debug, Subcommand)]
enum SearchGroup {
    /// Agent-memory queries (search, run, sessions, search-all).
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
    },
    /// Code-index queries.
    Code {
        #[command(subcommand)]
        action: CodeAction,
    },
}

#[derive(Debug, Subcommand)]
enum MemoryAction {
    /// Semantic search over memories in the current session.
    Search {
        query: String,
        #[arg(long = "top-k", default_value_t = DEFAULT_TOP_K)]
        top_k: usize,
        #[arg(long)]
        json: bool,
    },
    /// Inspect a workflow run by id.
    Run {
        run_id: String,
        #[arg(long)]
        json: bool,
    },
    /// List all known sessions.
    Sessions {
        #[arg(long)]
        json: bool,
    },
    /// Cross-session semantic search.
    #[command(name = "search-all")]
    SearchAll {
        query: String,
        #[arg(long = "top-k", default_value_t = DEFAULT_TOP_K)]
        top_k: usize,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum CodeAction {
    /// Semantic search over the code index.
    Search {
        query: String,
        #[arg(long = "top-k", default_value_t = DEFAULT_TOP_K)]
        top_k: usize,
        #[arg(long)]
        lang: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

/// Parse argv into a `Command` via clap.
///
/// Why: Keep the public function signature so existing callers and tests
/// don't have to change.
/// What: Runs clap's derive parser; converts errors to anyhow with the
/// original error message preserved.
/// Test: `parse_memory_search_args`, `parse_code_search_with_lang_filter`,
/// `parse_memory_run`, etc.
pub fn parse_args(args: &[&str]) -> Result<Command> {
    if args.len() < 2 {
        bail!("usage: <memory|code> <search|run> [args...]");
    }
    let parsed = SearchCli::try_parse_from(args).map_err(|e| anyhow::anyhow!("{e}"))?;
    let cmd = match parsed.cmd {
        SearchGroup::Memory { action } => match action {
            MemoryAction::Search { query, top_k, json } => {
                Command::MemorySearch { query, top_k, json }
            }
            MemoryAction::Run { run_id, json } => Command::MemoryRun { run_id, json },
            MemoryAction::Sessions { json } => Command::MemorySessions { json },
            MemoryAction::SearchAll { query, top_k, json } => {
                Command::MemorySearchAll { query, top_k, json }
            }
        },
        SearchGroup::Code { action } => match action {
            CodeAction::Search {
                query,
                top_k,
                lang,
                json,
            } => Command::CodeSearch {
                query,
                top_k,
                lang,
                json,
            },
        },
    };
    Ok(cmd)
}

/// Entry point: parse argv, open the local store, dispatch to the handler.
///
/// Why: Called from `main.rs` after it detects a `memory`/`code` subcommand.
/// Keeping the dispatch here means `main.rs` stays thin.
/// What: Resolves the store path to `$CWD/.open-mpm/state/`, opens the
/// store+embedder lazily, and runs the selected handler.
/// Test: Integration test would require disk I/O + fastembed model download;
/// unit tests cover the parsing and formatting in isolation.
pub async fn run_search_command(args: &[String]) -> Result<()> {
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let cmd = parse_args(&arg_refs)?;
    let open_mpm_dir = default_open_mpm_dir()?;

    // Migrate legacy `.open-mpm/store/` if present.
    if open_mpm_dir.exists() {
        crate::memory::migrate_if_needed(&open_mpm_dir)?;
    }

    let code_dir = open_mpm_dir.join("code");
    let sessions_dir = open_mpm_dir.join("sessions");

    match cmd {
        Command::MemorySearch { query, top_k, json } => {
            if !sessions_dir.exists() {
                println!("No sessions found at {}.", sessions_dir.display());
                return Ok(());
            }
            run_memory_search(&query, top_k, json, &sessions_dir).await
        }
        Command::MemoryRun { run_id, json } => {
            if !sessions_dir.exists() {
                println!("No sessions found at {}.", sessions_dir.display());
                return Ok(());
            }
            run_memory_run(&run_id, json, &sessions_dir).await
        }
        Command::MemorySessions { json } => run_memory_sessions(json, &sessions_dir).await,
        Command::MemorySearchAll { query, top_k, json } => {
            if !sessions_dir.exists() {
                println!("No sessions found at {}.", sessions_dir.display());
                return Ok(());
            }
            run_memory_search_all(&query, top_k, json, &sessions_dir).await
        }
        Command::CodeSearch {
            query,
            top_k,
            lang,
            json,
        } => {
            if !code_dir.exists() {
                println!(
                    "No code index found at {}. Run with --reindex first.",
                    code_dir.display()
                );
                return Ok(());
            }
            run_code_search(&query, top_k, lang.as_deref(), json, &code_dir).await
        }
    }
}

/// Default on-disk open-mpm runtime-state dir (`$CWD/.open-mpm/state/`).
///
/// NOTE: The repo-root `.open-mpm/` now holds committed bundled config
/// (agents/, skills/, workflows/, …). Runtime state (build.json, history,
/// code index, sessions, …) lives under `.open-mpm/state/`.
fn default_open_mpm_dir() -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("failed to get cwd")?;
    Ok(cwd.join(".open-mpm").join("state"))
}

/// Resolve the current run_id for memory reads. Defaults to `default` if
/// the env var isn't set (matches the migration-path `sessions/default/`).
fn current_run_id() -> String {
    std::env::var("OPEN_MPM_RUN_ID").unwrap_or_else(|_| "default".to_string())
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
async fn run_memory_search(
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
async fn run_memory_run(run_id: &str, json: bool, sessions_dir: &Path) -> Result<()> {
    // Open the specific run_id rather than current_run_id so users can inspect
    // any prior session without needing to set OPEN_MPM_RUN_ID.
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
async fn run_memory_sessions(json: bool, sessions_dir: &Path) -> Result<()> {
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
async fn run_memory_search_all(
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
async fn run_code_search(
    query: &str,
    top_k: usize,
    lang: Option<&str>,
    json: bool,
    code_dir: &Path,
) -> Result<()> {
    // The daemon's pid file lives at `<project_root>/.open-mpm/state/search.pid`,
    // so the project root is the parent of the state dir (which is the parent
    // of `code_dir`). Walk up two levels: code_dir -> state -> .open-mpm/parent.
    let project_root = code_dir
        .parent() // .open-mpm/state
        .and_then(|p| p.parent()) // .open-mpm
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

/// Format `MemoryResult`s as either an aligned table or a JSON array.
///
/// Why: The human table is the default for interactive use; `--json` is for
/// piping into other tools. Both paths share this function so the dispatch
/// stays boring.
/// What: JSON = `serde_json::to_string_pretty`. Human = header row +
/// `Agent | Phase | Timestamp | Score | Preview` for each hit. Preview is
/// the first 80 chars of the `response` payload field.
/// Test: `format_memory_results_human`, `format_memory_results_json`.
pub fn format_memory_results(hits: &[MemoryResult], json: bool) -> Result<String> {
    if json {
        return serde_json::to_string_pretty(hits).context("failed to serialize memory hits");
    }
    let mut out = String::new();
    out.push_str(&format!(
        "{:<20} {:<12} {:<17} {:<7} {}\n",
        "Agent", "Phase", "Timestamp", "Score", "Preview"
    ));
    out.push_str(&"-".repeat(100));
    out.push('\n');
    for h in hits {
        let agent = h
            .payload
            .get("agent_name")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)");
        let phase = h
            .payload
            .get("phase")
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        let ts = h
            .payload
            .get("timestamp")
            .and_then(|v| v.as_str())
            .map(format_timestamp)
            .unwrap_or_else(|| "-".to_string());
        let response = h
            .payload
            .get("response")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let preview = preview_text(response, 80);
        out.push_str(&format!(
            "{:<20} {:<12} {:<17} {:<7.3} {}\n",
            truncate_display(agent, 20),
            truncate_display(phase, 12),
            ts,
            h.score,
            preview
        ));
    }
    Ok(out)
}

/// Format an ordered list of `AgentSession`s (from `memory run`) as text or JSON.
pub fn format_sessions(sessions: &[AgentSession], json: bool) -> Result<String> {
    if json {
        return serde_json::to_string_pretty(sessions).context("failed to serialize sessions");
    }
    let mut out = String::new();
    for s in sessions {
        let ts = s.timestamp.format("%Y-%m-%d %H:%M").to_string();
        let preview = preview_text(&s.prompt, 80);
        out.push_str(&format!(
            "[{}] {} ({}): {}\n",
            ts, s.agent_name, s.phase, preview
        ));
    }
    Ok(out)
}

/// Format a list of `SessionMeta` entries as a table or JSON.
///
/// Why: `memory sessions` needs a compact listing for human readers and a
/// stable JSON shape for tooling.
/// What: JSON = `serde_json::to_string_pretty`. Human = header + rows of
/// `<run_id_prefix>  <started_at>  <task_preview>`.
/// Test: `format_session_list_human`.
pub fn format_session_list(sessions: &[SessionMeta], json: bool) -> Result<String> {
    if json {
        return serde_json::to_string_pretty(sessions).context("failed to serialize sessions");
    }
    let mut out = String::new();
    out.push_str(&format!(
        "{:<10} {:<17} {}\n",
        "Run", "Started", "Task preview"
    ));
    out.push_str(&"-".repeat(80));
    out.push('\n');
    for s in sessions {
        let run_short: String = s.run_id.chars().take(8).collect();
        let ts = s.started_at.format("%Y-%m-%d %H:%M").to_string();
        let preview = preview_text(&s.task_preview, 50);
        out.push_str(&format!("{run_short:<10} {ts:<17} {preview}\n"));
    }
    Ok(out)
}

/// Format `CodeChunk`s as either an aligned table or a JSON array.
pub fn format_code_results(chunks: &[CodeChunk], json: bool) -> Result<String> {
    if json {
        return serde_json::to_string_pretty(chunks).context("failed to serialize code chunks");
    }
    let mut out = String::new();
    out.push_str(&format!(
        "{:<40} {:<24} {:<10} {:<7} {}\n",
        "File:Line", "Function", "Lang", "Score", "Snippet"
    ));
    out.push_str(&"-".repeat(120));
    out.push('\n');
    for c in chunks {
        let fname = c
            .file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_else(|| c.file.to_str().unwrap_or("?"));
        let file_line = format!("{}:{}", fname, c.start_line);
        let func = c.function_name.as_deref().unwrap_or("-");
        let snippet = preview_text(&c.text, 80);
        out.push_str(&format!(
            "{:<40} {:<24} {:<10} {:<7.3} {}\n",
            truncate_display(&file_line, 40),
            truncate_display(func, 24),
            truncate_display(&c.language, 10),
            c.score,
            snippet
        ));
    }
    Ok(out)
}

/// First `max` chars of `s` with newlines collapsed to spaces.
fn preview_text(s: &str, max: usize) -> String {
    let flat: String = s.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
    if flat.chars().count() <= max {
        flat
    } else {
        flat.chars().take(max).collect()
    }
}

/// Truncate a string for fixed-width display, appending `…` when cut.
fn truncate_display(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else if max == 0 {
        String::new()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}

/// Format an RFC3339 timestamp string as `YYYY-MM-DD HH:MM`.
fn format_timestamp(rfc3339: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(rfc3339) {
        Ok(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
        Err(_) => rfc3339.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    #[test]
    fn parse_memory_search_args() {
        let cmd = parse_args(&["memory", "search", "hello", "--top-k", "3"]).unwrap();
        assert_eq!(
            cmd,
            Command::MemorySearch {
                query: "hello".to_string(),
                top_k: 3,
                json: false,
            }
        );
    }

    #[test]
    fn parse_code_search_with_lang_filter() {
        let cmd = parse_args(&["code", "search", "fn main", "--lang", "rust", "--json"]).unwrap();
        assert_eq!(
            cmd,
            Command::CodeSearch {
                query: "fn main".to_string(),
                top_k: 5,
                lang: Some("rust".to_string()),
                json: true,
            }
        );
    }

    #[test]
    fn parse_memory_run() {
        let cmd = parse_args(&["memory", "run", "run-abc-123"]).unwrap();
        assert_eq!(
            cmd,
            Command::MemoryRun {
                run_id: "run-abc-123".to_string(),
                json: false,
            }
        );
    }

    #[test]
    fn format_memory_results_human() {
        let results = vec![MemoryResult {
            id: "sess-1".to_string(),
            score: 0.87,
            segment: "mem".to_string(),
            payload: json!({
                "agent_name": "python-engineer",
                "phase": "code",
                "timestamp": "2026-04-22T10:30:00Z",
                "prompt": "write a hello world",
                "response": "print('hello')"
            }),
        }];
        let out = format_memory_results(&results, false).unwrap();
        assert!(out.contains("Agent"));
        assert!(out.contains("Phase"));
        assert!(out.contains("Score"));
        assert!(out.contains("python-engineer"));
    }

    #[test]
    fn format_code_results_json() {
        let chunks = vec![CodeChunk {
            file: PathBuf::from("/tmp/foo.rs"),
            function_name: Some("main".to_string()),
            start_line: 1,
            end_line: 3,
            language: "rust".to_string(),
            score: 0.9,
            text: "fn main() {}".to_string(),
            match_reason: "hybrid".to_string(),
        }];
        let out = format_code_results(&chunks, true).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(parsed.is_array());
        assert_eq!(parsed[0]["function_name"], "main");
    }

    #[test]
    fn format_sessions_human_includes_timestamp_and_preview() {
        let sessions = vec![AgentSession {
            id: "s1".to_string(),
            agent_name: "pm".to_string(),
            workflow_run_id: "run-1".to_string(),
            phase: "plan".to_string(),
            prompt: "plan the work".to_string(),
            response: "ok".to_string(),
            timestamp: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
            parent_id: None,
            segment: None,
        }];
        let out = format_sessions(&sessions, false).unwrap();
        assert!(out.contains("pm"));
        assert!(out.contains("(plan)"));
        assert!(out.contains("plan the work"));
    }

    #[test]
    fn preview_text_handles_newlines_and_truncation() {
        assert_eq!(preview_text("hi\nthere", 80), "hi there");
        let long = "x".repeat(200);
        assert_eq!(preview_text(&long, 10).chars().count(), 10);
    }

    #[test]
    fn parse_rejects_unknown_command() {
        assert!(parse_args(&["foo", "bar", "baz"]).is_err());
    }

    #[test]
    fn parse_rejects_missing_positional() {
        assert!(parse_args(&["memory", "search"]).is_err());
    }

    #[test]
    fn parse_memory_sessions() {
        let cmd = parse_args(&["memory", "sessions"]).unwrap();
        assert_eq!(cmd, Command::MemorySessions { json: false });
        let cmd = parse_args(&["memory", "sessions", "--json"]).unwrap();
        assert_eq!(cmd, Command::MemorySessions { json: true });
    }

    #[test]
    fn parse_memory_search_all() {
        let cmd = parse_args(&["memory", "search-all", "hello", "--top-k", "7"]).unwrap();
        assert_eq!(
            cmd,
            Command::MemorySearchAll {
                query: "hello".to_string(),
                top_k: 7,
                json: false,
            }
        );
    }

    #[test]
    fn format_session_list_human_lists_runs() {
        let s = vec![SessionMeta {
            run_id: "abcdef1234567890".to_string(),
            started_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
            task_preview: "hello world task".to_string(),
        }];
        let out = format_session_list(&s, false).unwrap();
        assert!(out.contains("Run"));
        assert!(out.contains("abcdef12"));
        assert!(out.contains("hello world task"));
    }

    #[test]
    fn parse_handles_json_flag_before_query() {
        let cmd = parse_args(&["memory", "search", "--json", "hello"]).unwrap();
        assert_eq!(
            cmd,
            Command::MemorySearch {
                query: "hello".to_string(),
                top_k: 5,
                json: true,
            }
        );
    }
}
