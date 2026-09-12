//! Retired local memory import/export commands (#7360).
#[cfg(test)]
mod tests;
use crate::memory::store::Segment;
use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
/// Parsed CLI command for `memories ...`.
///
/// Why: Exposed as a structured enum (not just a clap parser) so tests can
/// assert parse outcomes via equality without re-running clap.
/// What: Each variant maps 1:1 to a clap subcommand below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Export {
        session: Option<String>,
        output: Option<PathBuf>,
        segment: Option<String>,
    },
    Import {
        input: Option<PathBuf>,
        from_committed: bool,
    },
    List {
        scope: String,
    },
}

/// Clap front-end for `trusty-agents memories ...`.
///
/// Why: Replaces the hand-rolled `parse_args` flag scanner with derive-based
/// clap parsing so help text, error messages, and value validation come for
/// free. The downstream dispatcher continues to consume the structured
/// `Command` enum.
/// What: A single `Subcommand` enum mirroring `Command`; converted in
/// `parse_args` via a small `From` step.
/// Test: All existing `parse_args` unit tests still pass through this path.
#[derive(Debug, Parser)]
#[command(no_binary_name = true)]
struct MemoriesCli {
    #[command(subcommand)]
    cmd: MemoriesSubcommand,
}

#[derive(Debug, Subcommand)]
enum MemoriesSubcommand {
    /// Retired local export command; use trusty-memory. Existing files remain preserved.
    Export {
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        output: Option<PathBuf>,
        /// Optional segment filter — only export records from this segment.
        /// Valid values: context, brief, history, agent-memory, code-index.
        #[arg(long)]
        segment: Option<String>,
    },
    /// Retired local import command; use trusty-memory for explicit durable imports.
    Import {
        #[arg(long)]
        input: Option<PathBuf>,
        #[arg(long = "from-committed")]
        from_committed: bool,
    },
    /// Retired local session listing; use trusty-memory chat-session APIs.
    List {
        #[arg(long, default_value = "session")]
        scope: String,
    },
}

/// Parse argv tail into a `Command` via clap.
///
/// Why: Keep this function's signature stable so existing tests and callers
/// (`run_memories_command`, `main.rs` dispatch) continue to work.
/// What: Delegates to clap's derive parser, converts errors to `anyhow`,
/// and post-validates the `--scope` allowlist (clap doesn't enforce it
/// without a `value_parser`, and we want the original error wording).
/// Test: See `tests` module — every previous `parse_args` case still passes.
pub fn parse_args(args: &[&str]) -> Result<Command> {
    if args.is_empty() {
        bail!("usage: memories <export|import|list> [args...]");
    }
    let parsed = MemoriesCli::try_parse_from(args).map_err(|e| anyhow::anyhow!("{e}"))?;
    let cmd = match parsed.cmd {
        MemoriesSubcommand::Export {
            session,
            output,
            segment,
        } => {
            // Validate segment name early so the user gets a clean error
            // before we open any stores.
            if let Some(s) = &segment
                && Segment::from_name(s).is_none()
            {
                bail!(
                    "invalid --segment: {s} (expected context|brief|history|agent-memory|code-index)"
                );
            }
            Command::Export {
                session,
                output,
                segment,
            }
        }
        MemoriesSubcommand::Import {
            input,
            from_committed,
        } => Command::Import {
            input,
            from_committed,
        },
        MemoriesSubcommand::List { scope } => {
            if !matches!(scope.as_str(), "session" | "all" | "imported") {
                bail!("invalid --scope: {scope} (expected session|all|imported)");
            }
            Command::List { scope }
        }
    };
    Ok(cmd)
}

/// Entry point for `trusty-agents memories ...`.
pub async fn run_memories_command(args: &[String]) -> Result<()> {
    let refs: Vec<_> = args.iter().map(String::as_str).collect();
    parse_args(&refs)?;
    bail!(
        "Local memories import/export is retired. Use trusty-memory for durable facts. Existing shared-memories.jsonl and local stores are preserved; migrate their contents explicitly if needed."
    )
}
