//! `tm memory recall-deep|recall-all|list|prompt-context|task|kg` (#11625).
//!
//! Why: a thin translation layer like `commands::memory_verbs` — clap args in,
//! one [`run_tool`] call out, one report rendered — so the MCP-only memory
//! tools stay reachable when the MCP connection is dead.
//! What: [`run`] maps a [`MemoryToolAction`] to a [`MemoryTool`], runs it, and
//! prints the shared JSON envelope (`--json`) or a short human summary.
//! Returns `Err` — a non-zero exit — when the palace, the socket or the call
//! fails.
//! Test: `memory_tool_cli_tests.rs` for parsing;
//! `tests/memory_tool_verbs_socket.rs` for the binary over a socket.

use anyhow::Context as _;
use serde_json::Value;
use trusty_mpm::core::memory_tool_verbs::{MemoryTool, run_tool};
use trusty_mpm::core::memory_verbs::{MemoryVerbOptions, MemoryVerbOutcome};

use crate::cli::{MemoryKgAction, MemoryTaskAction, MemoryToolAction, ToolOutput};

/// Longest slice of a row printed per line; `--json` carries the full text.
const SNIPPET_CHARS: usize = 120;

/// Split an action into the tool, its `--palace`, and its output flags.
fn into_parts(action: MemoryToolAction) -> (MemoryTool, Option<String>, ToolOutput) {
    use MemoryToolAction as A;
    match action {
        A::RecallDeep {
            query,
            palace,
            top_k,
            room,
            min_score,
            out,
        } => (
            MemoryTool::RecallDeep {
                query,
                top_k,
                room,
                min_score,
            },
            palace,
            out,
        ),
        A::RecallAll {
            query,
            top_k,
            deep,
            scope,
            out,
        } => (
            MemoryTool::RecallAll {
                query,
                top_k,
                deep,
                scope,
            },
            None,
            out,
        ),
        A::List {
            palace,
            room,
            wing,
            tag,
            limit,
            out,
        } => (
            MemoryTool::List {
                room,
                wing,
                tag,
                limit,
            },
            palace,
            out,
        ),
        A::PromptContext { query, out } => (MemoryTool::PromptContext { query }, None, out),
        A::Task { action } => match action {
            MemoryTaskAction::Add {
                content,
                palace,
                room,
                tags,
                out,
            } => (
                MemoryTool::TaskAdd {
                    content,
                    room,
                    tags,
                },
                palace,
                out,
            ),
            MemoryTaskAction::List {
                palace,
                include_completed,
                out,
            } => (MemoryTool::TaskList { include_completed }, palace, out),
            MemoryTaskAction::Complete {
                drawer_id,
                palace,
                out,
            } => (MemoryTool::TaskComplete { drawer_id }, palace, out),
        },
        A::Kg { action } => match action {
            MemoryKgAction::Assert {
                subject,
                predicate,
                object,
                palace,
                confidence,
                provenance,
                out,
            } => (
                MemoryTool::KgAssert {
                    subject,
                    predicate,
                    object,
                    confidence,
                    provenance,
                },
                palace,
                out,
            ),
            MemoryKgAction::Query {
                subject,
                palace,
                out,
            } => (MemoryTool::KgQuery { subject }, palace, out),
            MemoryKgAction::Gaps { palace, out } => (MemoryTool::KgGaps, palace, out),
        },
    }
}

/// Run one MCP-only memory tool over the daemon socket.
///
/// Why: keeps `commands::memory`'s dispatcher one arm for all ten verbs.
/// What: see the module doc.
/// Test: `tests/memory_tool_verbs_socket.rs`.
pub(crate) async fn run(action: MemoryToolAction) -> anyhow::Result<()> {
    let (tool, palace, out) = into_parts(action);
    let opts = MemoryVerbOptions {
        palace,
        socket: out.memory_socket,
        cwd: None,
    };
    let outcome = run_tool(&tool, &opts).await?;
    if out.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&outcome).context("serialise the memory envelope")?
        );
    } else {
        print_summary(&tool, &outcome);
    }
    Ok(())
}

/// Render the human summary: rows, prompt Markdown, or one write line.
///
/// Why: an operator wants the answer, not the envelope; nothing parses this.
/// Test: cosmetic; exercised through `tests/memory_tool_verbs_socket.rs`.
fn print_summary(tool: &MemoryTool, outcome: &MemoryVerbOutcome) {
    let palace = outcome.palace.as_deref().unwrap_or("(all palaces)");
    let result = &outcome.result;
    if let Some(text) = result.as_str() {
        println!("{text}");
        return;
    }
    if let (Some(key), Some(rows)) = (
        tool.items_key(),
        tool.items_key()
            .and_then(|k| result.get(k))
            .and_then(Value::as_array),
    ) {
        for row in rows {
            println!("{}", row_line(row));
        }
        if let Some(hint) = result.get("hint").and_then(Value::as_str) {
            println!("{hint}");
        }
        println!("\n{} {key} from palace {palace}", rows.len());
        return;
    }
    if let Some(hint) = result.get("hint").and_then(Value::as_str) {
        println!("{hint}");
        return;
    }
    let status = result.get("status").and_then(Value::as_str).unwrap_or("ok");
    let drawer = result
        .get("drawer_id")
        .and_then(Value::as_str)
        .unwrap_or("-");
    println!("{status} {drawer} → palace {palace}");
}

/// One line per row: a triple as `s p o`, a drawer as `id content`, else JSON.
fn row_line(row: &Value) -> String {
    let s = |k: &str| row.get(k).and_then(Value::as_str);
    if let (Some(subj), Some(pred), Some(obj)) = (s("subject"), s("predicate"), s("object")) {
        return format!("{subj} {pred} {obj}");
    }
    if let Some(content) = s("content") {
        let id = s("drawer_id").unwrap_or("-");
        let palace = s("palace_id")
            .map(|p| format!("[{p}] "))
            .unwrap_or_default();
        return format!("{id} {palace}{}", snippet(content));
    }
    snippet(&row.to_string())
}

/// The first line of `content`, capped at [`SNIPPET_CHARS`] characters.
fn snippet(content: &str) -> String {
    let first = content.lines().next().unwrap_or("").trim();
    if first.chars().count() <= SNIPPET_CHARS {
        return first.to_string();
    }
    let cut: String = first.chars().take(SNIPPET_CHARS).collect();
    format!("{cut}…")
}

// #11625: the parse tests live beside the other bin tests; attached here so
// `main.rs` stays under the SLOC cap.
#[cfg(test)]
#[path = "../memory_tool_cli_tests.rs"]
mod memory_tool_cli_tests;
