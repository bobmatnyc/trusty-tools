//! `tm memory recall|remember|note` — the no-MCP palace verbs (#8352).
//!
//! Why: a thin translation layer, the same shape `commands::memory` uses for
//! `import` — clap args in, one library call out, one report rendered. The verbs
//! themselves live in [`trusty_mpm::core::memory_verbs`] so they are testable
//! without a CLI, and so nothing about reaching the daemon is duplicated here.
//! What: [`run`] builds [`MemoryVerbOptions`], runs the verb, and prints either
//! the stable JSON envelope (`--json`) or a human summary. Returns `Err` — a
//! non-zero exit — when the palace could not be resolved, the socket could not
//! be derived, or the daemon did not answer.
//! Test: `cli_parses_memory_recall`, `cli_parses_memory_remember_with_tags`,
//! `cli_parses_memory_note` in `tests.rs`; the socket behaviour is covered by
//! `tests/memory_verbs_socket.rs`.

use std::path::PathBuf;

use anyhow::Context as _;
use serde_json::Value;
use trusty_mpm::core::memory_verbs::{MemoryVerb, MemoryVerbOptions, MemoryVerbOutcome, run_verb};

/// Longest slice of a recalled drawer printed per line.
///
/// Why: a recall hit can be a whole paragraph, and the human summary is an
/// index — `--json` is where the full text lives.
const SNIPPET_CHARS: usize = 160;

/// Run one no-MCP memory verb.
///
/// Why: keeps `commands::memory`'s dispatcher one arm per action.
/// What: see the module doc.
/// Test: `tests/memory_verbs_socket.rs`.
pub(crate) async fn run(
    verb: MemoryVerb,
    palace: Option<String>,
    memory_socket: Option<PathBuf>,
    json: bool,
) -> anyhow::Result<()> {
    let opts = MemoryVerbOptions {
        palace,
        socket: memory_socket,
        cwd: None,
    };
    let outcome = run_verb(&verb, &opts).await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&outcome).context("serialise the memory envelope")?
        );
    } else {
        print_summary(&outcome);
    }
    Ok(())
}

/// Render the human summary.
///
/// Why: an operator running this by hand wants the hits, or the one line saying
/// what the write did — not the envelope. Nothing parses this; `--json` is the
/// machine surface.
/// What: a recall prints one line per hit plus a count; a write prints the
/// daemon's own `status`, its reason when it skipped, and the drawer id when it
/// stored. A palace index (the #6318 answer to a recall with no palace) prints
/// the hint the daemon supplied.
/// Test: cosmetic; exercised through `tests/memory_verbs_socket.rs`.
fn print_summary(outcome: &MemoryVerbOutcome) {
    let palace = outcome.palace.as_deref().unwrap_or("(none resolved)");
    match outcome.count {
        Some(count) => {
            let results = outcome
                .result
                .get("results")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            for hit in results {
                let score = hit.get("score").and_then(Value::as_f64).unwrap_or(0.0);
                let layer = hit.get("layer").and_then(Value::as_str).unwrap_or("-");
                let content = hit.get("content").and_then(Value::as_str).unwrap_or("");
                println!("[{score:.3}] {layer:<10} {}", snippet(content));
            }
            if let Some(hint) = outcome.result.get("hint").and_then(Value::as_str) {
                println!("{hint}");
            }
            println!("\n{count} hit(s) from palace {palace}");
        }
        None => {
            let status = outcome
                .result
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("stored");
            let drawer = outcome
                .result
                .get("drawer_id")
                .and_then(Value::as_str)
                .unwrap_or("-");
            match outcome.result.get("reason").and_then(Value::as_str) {
                Some(reason) => println!("{status} → palace {palace}: {reason}"),
                None => println!("{status} {drawer} → palace {palace}"),
            }
        }
    }
}

/// One line of `content`, capped at [`SNIPPET_CHARS`] characters.
fn snippet(content: &str) -> String {
    let first = content.lines().next().unwrap_or("").trim();
    if first.chars().count() <= SNIPPET_CHARS {
        return first.to_string();
    }
    let cut: String = first.chars().take(SNIPPET_CHARS).collect();
    format!("{cut}…")
}
