//! `trusty-agents memories <export|import|list>` — cross-machine memory sharing.
//!
//! Why: Teams want to share project knowledge captured by one engineer's
//! agent runs with the rest of the team via git. The export format is plain
//! JSONL committed to `.trusty-agents/shared-memories.jsonl`; teammates pull,
//! `import` runs (manually or auto on startup), and the foreign sessions
//! become recallable via `memory_recall` with `scope: "imported"`.
//! What: Three subcommands — `export` writes every memory in the current
//! session (or `--session <id>`) as one JSONL record per line including the
//! embedding vector + machine_id; `import` reads such a file and inserts
//! every record into the local store with `imported: true` stamped on the
//! payload; `list` enumerates the sessions reachable under a chosen `--scope`.
//!
//! Module layout (see #366 split):
//! - `mod.rs` — CLI parsing (`Command`, clap front-end), dispatch, record types
//! - `ops.rs` — the export/import/list/auto-import implementations + helpers
//! - `tests.rs` — unit tests
//!
//! Test: See `tests` module — round-trip export → import preserves payloads,
//! import flag-stamps imported=true, scope routing parses correctly.

mod ops;

#[cfg(test)]
mod tests;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

use crate::memory::store::Segment;

pub use ops::{auto_import_if_changed, export_session, import_file};

/// File name for the committed cross-machine JSONL.
///
/// Why: Living at the repo root inside `.trusty-agents/` means `git pull` brings
/// teammates' memories with the same ergonomics as code; auto-import keys
/// off this fixed path so the workflow is "drop the file in, it shows up".
pub const SHARED_MEMORIES_FILENAME: &str = "shared-memories.jsonl";

/// Tracker file recording which shared-memories files have already been
/// imported (by sha256 of contents). Lives under `.trusty-agents/state/` so it's
/// gitignored alongside other runtime state.
pub(super) const IMPORT_TRACKER_FILENAME: &str = "memories_imported.json";

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
    /// Export this session's memories as JSONL.
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
    /// Import memories from a JSONL file (defaults to the committed file).
    Import {
        #[arg(long)]
        input: Option<PathBuf>,
        #[arg(long = "from-committed")]
        from_committed: bool,
    },
    /// List sessions (scope: session|all|imported).
    List {
        #[arg(long, default_value = "session")]
        scope: String,
    },
}

/// One JSONL record. `embedding` is included so the receiving machine can
/// insert without recomputing — saves a model load and keeps results
/// deterministic across machines that may use different embedders later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportRecord {
    pub id: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub machine_id: String,
    pub exported_at: String,
    pub embedding: Vec<f32>,
    /// Echoes the entire original payload so importers can preserve any
    /// extra fields (tags, paths, created_at, etc.) the producer stamped.
    #[serde(default)]
    pub payload: serde_json::Value,
    /// Optional graph-native segment tier this record belongs to.
    ///
    /// Why: With Context/Brief/History segments now first-class, the export
    /// format needs to carry the tier so an importer can re-route to the
    /// matching segment on the receiving machine. Absent on records from
    /// older exports — those default to `Segment::AgentMemory` on import.
    /// What: Snake-case name (e.g., `"context"`, `"brief"`, `"history"`,
    /// `"agent_memory"`).
    /// Test: `memory_export_includes_segment_field` and
    /// `memory_import_routes_by_segment` exercise the round-trip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segment: Option<String>,
}

/// Tracker JSON shape.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(super) struct ImportTracker {
    /// Map of file path → {hash, imported_at, count}.
    #[serde(default)]
    pub(super) files: std::collections::BTreeMap<String, ImportEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ImportEntry {
    pub(super) hash: String,
    pub(super) imported_at: String,
    pub(super) count: usize,
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
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let cmd = parse_args(&arg_refs)?;
    let cwd = std::env::current_dir().context("failed to get cwd")?;

    match cmd {
        Command::Export {
            session,
            output,
            segment,
        } => {
            let session_id = session.unwrap_or_else(current_session_id);
            let out_path =
                output.unwrap_or_else(|| cwd.join(".trusty-agents").join(SHARED_MEMORIES_FILENAME));
            let segment_filter = segment.as_deref().and_then(Segment::from_name);
            let n = export_session(&cwd, &session_id, &out_path, segment_filter).await?;
            let seg_label = segment
                .as_deref()
                .map(|s| format!(" (segment={s})"))
                .unwrap_or_default();
            println!(
                "[trusty-agents] Exported {n} memories from session '{session_id}'{seg_label} → {}",
                out_path.display()
            );
            Ok(())
        }
        Command::Import {
            input,
            from_committed,
        } => {
            // Why: Both the `--from-committed` and default branches currently
            // resolve to the same shared-memories file under `.trusty-agents/`.
            // The flag is kept on the CLI for forward compatibility (e.g. if
            // we later split the committed vs. local file paths) but it has
            // no effect today — flatten the branches to satisfy
            // `clippy::if_same_then_else` without changing behavior.
            let _ = from_committed;
            let in_path = if let Some(p) = input {
                p
            } else {
                cwd.join(".trusty-agents").join(SHARED_MEMORIES_FILENAME)
            };
            let n = import_file(&cwd, &in_path).await?;
            println!(
                "[trusty-agents] Imported {n} memories from {}",
                in_path.display()
            );
            Ok(())
        }
        Command::List { scope } => {
            ops::list_sessions(&cwd, &scope).await?;
            Ok(())
        }
    }
}

/// Resolve the active session_id: prefer the run id env var, fall back to the
/// most recent registered session, finally "default".
pub fn current_session_id() -> String {
    if let Ok(rid) = crate::env_compat::env_var("TAGENT_RUN_ID", "OPEN_MPM_RUN_ID")
        && !rid.is_empty()
    {
        return rid;
    }
    "default".to_string()
}
