//! `tm memory` verbs for the MCP-only trusty-memory tools (#11625).
//!
//! Why: split from `memory.rs` so that file stays under the SLOC cap; these
//! variants are flattened into [`super::memory::MemoryAction`], so the user
//! sees one flat `tm memory` group.
//! What: [`MemoryToolAction`] — `recall-deep`, `recall-all`, `list`,
//! `prompt-context`, `task add|list|complete`, `kg assert|query|gaps`. Flags
//! map one-to-one onto the tool schema keys in trusty-memory's
//! `tools/definitions.rs`.
//! Test: `memory_tool_cli_tests.rs`.

use std::path::PathBuf;

use clap::{Args, Subcommand};

/// The output and transport flags every tool verb shares.
#[derive(Debug, Clone, Args)]
pub(crate) struct ToolOutput {
    /// Print the machine-readable envelope instead of the human summary.
    #[arg(long)]
    pub(crate) json: bool,
    /// trusty-memory socket path. Defaults to `TRUSTY_MEMORY_SOCKET`, else the
    /// derived one.
    #[arg(long)]
    pub(crate) memory_socket: Option<PathBuf>,
}

/// `tm memory` verbs for the MCP-only tools; see the module doc.
#[derive(Debug, Subcommand)]
pub(crate) enum MemoryToolAction {
    /// Deep recall (L3 full HNSW search) — the native `memory_recall_deep`.
    RecallDeep {
        /// What to recall.
        query: String,
        /// Palace to search. Defaults to the session's own.
        #[arg(long)]
        palace: Option<String>,
        /// Hits to return. Defaults to trusty-memory's own default.
        #[arg(long)]
        top_k: Option<u64>,
        /// Restrict the search to one room.
        #[arg(long)]
        room: Option<String>,
        /// Drop hits below this relevance floor.
        #[arg(long)]
        min_score: Option<f64>,
        #[command(flatten)]
        out: ToolOutput,
    },
    /// Search across palaces — the native `memory_recall_all`.
    ///
    /// Takes no `--palace`: the tool spans every palace.
    RecallAll {
        /// What to recall.
        query: String,
        /// Hits to return. Defaults to trusty-memory's own default.
        #[arg(long)]
        top_k: Option<u64>,
        /// Use the deep (L3) lane in each palace.
        #[arg(long)]
        deep: bool,
        /// `resident` searches loaded palaces only; `all` opens every one.
        #[arg(long, value_parser = ["resident", "all"])]
        scope: Option<String>,
        #[command(flatten)]
        out: ToolOutput,
    },
    /// List drawers in a palace — the native `memory_list`.
    List {
        /// Palace to list. Defaults to the session's own.
        #[arg(long)]
        palace: Option<String>,
        /// Filter by room.
        #[arg(long)]
        room: Option<String>,
        /// Filter by wing (every room it owns).
        #[arg(long, conflicts_with = "room")]
        wing: Option<String>,
        /// Filter by tag.
        #[arg(long)]
        tag: Option<String>,
        /// Max drawers. Defaults to trusty-memory's own default.
        #[arg(long)]
        limit: Option<u64>,
        #[command(flatten)]
        out: ToolOutput,
    },
    /// Print the hot prompt facts as Markdown — the native `get_prompt_context`.
    ///
    /// Takes no `--palace`: the daemon serves one cache across palaces.
    PromptContext {
        /// Only facts whose subject or object contains this text.
        query: Option<String>,
        #[command(flatten)]
        out: ToolOutput,
    },
    /// Task drawers — `task_add`, `task_list`, `task_complete`.
    Task {
        #[command(subcommand)]
        action: MemoryTaskAction,
    },
    /// The knowledge graph — `kg_assert`, `kg_query`, `kg_gaps`.
    Kg {
        #[command(subcommand)]
        action: MemoryKgAction,
    },
}

/// `tm memory task` actions.
#[derive(Debug, Subcommand)]
pub(crate) enum MemoryTaskAction {
    /// Create a Task drawer — the native `task_add`.
    Add {
        /// The task description.
        content: String,
        /// Palace to write to. Defaults to the session's own.
        #[arg(long)]
        palace: Option<String>,
        /// Room to file it in.
        #[arg(long)]
        room: Option<String>,
        /// Tag to store alongside it; repeat for several.
        #[arg(long = "tag")]
        tags: Vec<String>,
        #[command(flatten)]
        out: ToolOutput,
    },
    /// List open Task drawers — the native `task_list`.
    List {
        /// Palace to list. Defaults to the session's own.
        #[arg(long)]
        palace: Option<String>,
        /// Include completed tasks.
        #[arg(long)]
        include_completed: bool,
        #[command(flatten)]
        out: ToolOutput,
    },
    /// Mark a Task drawer completed — the native `task_complete`.
    Complete {
        /// UUID of the Task drawer.
        drawer_id: String,
        /// Palace the task lives in. Defaults to the session's own.
        #[arg(long)]
        palace: Option<String>,
        #[command(flatten)]
        out: ToolOutput,
    },
}

/// `tm memory kg` actions.
#[derive(Debug, Subcommand)]
pub(crate) enum MemoryKgAction {
    /// Assert a triple — the native `kg_assert`.
    Assert {
        /// Triple subject.
        subject: String,
        /// Triple predicate.
        predicate: String,
        /// Triple object.
        object: String,
        /// Palace to write to. Defaults to the session's own.
        #[arg(long)]
        palace: Option<String>,
        /// Confidence, 0.0–1.0. Defaults to trusty-memory's own (1.0).
        #[arg(long)]
        confidence: Option<f64>,
        /// Where the fact came from.
        #[arg(long)]
        provenance: Option<String>,
        #[command(flatten)]
        out: ToolOutput,
    },
    /// Active triples for one subject — the native `kg_query`.
    Query {
        /// The subject to query.
        subject: String,
        /// Palace to query. Defaults to the session's own.
        #[arg(long)]
        palace: Option<String>,
        #[command(flatten)]
        out: ToolOutput,
    },
    /// Knowledge gaps the dream cycle found — the native `kg_gaps`.
    Gaps {
        /// Palace to read. Defaults to the session's own.
        #[arg(long)]
        palace: Option<String>,
        #[command(flatten)]
        out: ToolOutput,
    },
}
