//! Code search CLI and explicit retirement of local memory commands (#7360).
//! Test: parser cases and `legacy_memory_commands_fail_before_store_access`.
mod format;
mod handlers;

#[cfg(test)]
mod tests;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

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
    /// Retired local memory commands; use trusty-memory or assistant memory tools.
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
    /// Retired local memory search; use assistant memory_recall.
    Search {
        query: String,
        #[arg(long = "top-k", default_value_t = DEFAULT_TOP_K)]
        top_k: usize,
        #[arg(long)]
        json: bool,
    },
    /// Retired local run inspection; use the current task API.
    Run {
        run_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Retired local session listing; use trusty-memory chat-session APIs.
    Sessions {
        #[arg(long)]
        json: bool,
    },
    /// Retired cross-session local search; use trusty-memory recall.
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
/// What: Resolves the store path to `$CWD/.trusty-agents/state/`, opens the
/// store+embedder lazily, and runs the selected handler.
/// Test: Integration test would require disk I/O + fastembed model download;
/// unit tests cover the parsing and formatting in isolation.
pub async fn run_search_command(args: &[String]) -> Result<()> {
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let cmd = parse_args(&arg_refs)?;
    // #7360: legacy commands must not open or migrate another memory backend.
    if !matches!(cmd, Command::CodeSearch { .. }) {
        bail!(
            "Local memory commands are retired. Use trusty-memory or an assistant's memory_remember/memory_recall tools. Existing local files are preserved; migrate their contents explicitly if needed."
        );
    }
    let code_dir = default_agent_dir()?.join("code");
    match cmd {
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
            handlers::run_code_search(&query, top_k, lang.as_deref(), json, &code_dir).await
        }
        _ => unreachable!("legacy memory commands returned before filesystem access"),
    }
}

/// Default on-disk trusty-agents runtime-state dir (`$CWD/.trusty-agents/state/`).
///
/// NOTE: The repo-root `.trusty-agents/` now holds committed bundled config
/// (agents/, skills/, workflows/, …). Runtime state (build.json, history,
/// code index, sessions, …) lives under `.trusty-agents/state/`.
fn default_agent_dir() -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("failed to get cwd")?;
    Ok(cwd.join(".trusty-agents").join("state"))
}
