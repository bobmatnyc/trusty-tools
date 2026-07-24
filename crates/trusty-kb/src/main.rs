//! `trusty-kb` binary — a native MCP stdio server for the KB store.
//!
//! Why: external orchestrators (tagent's base assistant, a dream-extraction
//! pass) speak line-delimited JSON-RPC/MCP over stdio; this binary is the thin
//! CLI that resolves the service-level root configuration and hands off to the
//! shared stdio loop. All logic lives in the library — this file only parses
//! args and wires defaults.
//!
//! What: `trusty-kb serve --stdio [--root PATH] [--knowledge-dir PATH]`. The
//! knowledge directory (holding one subtree per assistant) defaults to
//! `~/.trusty-agents/knowledge` (override: `--knowledge-dir` / `KB_KNOWLEDGE_DIR`);
//! the service default root defaults to `<knowledge_dir>/bob-kb` (override:
//! `--root` / `KB_ROOT`). One instance serves every assistant's tree — the root
//! is resolved per tool call, not pinned here.
//!
//! Test: the binary is exercised by the live JSON-RPC smoke in the PR; the
//! per-message logic is unit-tested via `trusty_kb::mcp::dispatch_with`.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use trusty_kb::mcp::{ServerConfig, serve};
use trusty_kb::roots::Roots;

/// trusty-kb — deterministic personal-knowledge-base MCP server.
#[derive(Parser)]
#[command(name = "trusty-kb", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the MCP server over stdio.
    Serve {
        /// Speak MCP over stdio (the only supported transport today).
        #[arg(long)]
        stdio: bool,
        /// Service default KB root (override: KB_ROOT). Defaults to
        /// `<knowledge_dir>/bob-kb`.
        #[arg(long, env = "KB_ROOT")]
        root: Option<PathBuf>,
        /// Directory holding one subtree per assistant (override:
        /// KB_KNOWLEDGE_DIR). Defaults to `~/.trusty-agents/knowledge`.
        #[arg(long = "knowledge-dir", env = "KB_KNOWLEDGE_DIR")]
        knowledge_dir: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Serve {
            stdio,
            root,
            knowledge_dir,
        } => {
            if !stdio {
                anyhow::bail!("only --stdio transport is supported; pass `serve --stdio`");
            }
            let knowledge_dir = knowledge_dir.unwrap_or_else(default_knowledge_dir);
            let default_root = root.unwrap_or_else(|| knowledge_dir.join("bob-kb"));
            let config = ServerConfig::new(Roots::new(knowledge_dir, default_root));
            serve(config).await
        }
    }
}

/// The default knowledge directory: `<home>/.trusty-agents/knowledge`.
fn default_knowledge_dir() -> PathBuf {
    home_dir().join(".trusty-agents").join("knowledge")
}

/// Best-effort home directory from the environment (HOME, then USERPROFILE).
fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}
