//! `tm memory recall|remember|note|forget` — the no-MCP palace verbs (#8352).
//!
//! Why: a thin translation layer, the same shape `commands::memory` uses for
//! `import` — clap args in, one library call out, one report rendered. The verbs
//! themselves live in [`trusty_mpm::core::memory_verbs`] so they are testable
//! without a CLI, and so nothing about reaching the daemon is duplicated here.
//! What: [`run`] builds [`MemoryVerbOptions`], runs the verb, and prints either
//! the stable JSON envelope (`--json`) or a human summary. Returns `Err` — a
//! non-zero exit — when the palace could not be resolved, the socket could not
//! be derived, or the daemon did not answer. Exits
//! [`EXIT_SLOT_REFUSED`] (3) when a write was stored but its `--fact-key` slot
//! was refused (#9142). A forget the daemon did not report `deleted` is an
//! `Err` too (#9340).
//! Test: `cli_parses_memory_recall`, `cli_parses_memory_remember_with_tags`,
//! `cli_parses_memory_note`, `cli_parses_memory_forget` in `tests.rs`; the
//! socket behaviour is covered by
//! `tests/memory_verbs_socket.rs`.

use std::path::PathBuf;

use std::io::Write as _;

use anyhow::Context as _;
use serde_json::Value;
use trusty_mpm::core::memory_forget::forget_failure;
use trusty_mpm::core::memory_verbs::{
    EXIT_SLOT_REFUSED, MemoryVerb, MemoryVerbOptions, MemoryVerbOutcome, run_verb, slot_refusal,
};

/// Longest slice of a recalled drawer printed per line.
///
/// Why: a recall hit can be a whole paragraph, and the human summary is an
/// index — `--json` is where the full text lives.
const SNIPPET_CHARS: usize = 160;

/// Run one no-MCP memory verb.
///
/// Why: keeps `commands::memory`'s dispatcher one arm per action.
/// What: see the module doc. A refused slot prints a warning (stderr under
/// `--json`, whose stdout is unchanged) and exits [`EXIT_SLOT_REFUSED`]; the
/// stored drawer is left as it is and nothing is retried. A forget answered
/// with anything but `deleted` prints the envelope under `--json`, no human
/// summary, and returns `Err` (#9340).
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
    // #9340: `not_found` is a successful daemon answer; it must not exit 0.
    if let MemoryVerb::Forget { drawer_id } = &verb
        && let Some(reason) = forget_failure(drawer_id, outcome.palace.as_deref(), &outcome.result)
    {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&outcome).context("serialise the memory envelope")?
            );
        }
        anyhow::bail!(reason);
    }
    // #9142: a refused slot still stores the drawer; make that loud.
    let refusal = slot_refusal(&verb, &outcome.result);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&outcome).context("serialise the memory envelope")?
        );
        if let Some(reason) = &refusal {
            eprintln!("WARNING: slot refused: {reason}");
        }
    } else {
        print_summary(&outcome, refusal.as_deref());
    }
    if refusal.is_some() {
        std::io::stdout().flush().ok();
        std::process::exit(EXIT_SLOT_REFUSED);
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
/// the hint the daemon supplied. A `refusal` adds a WARNING line (#9142).
/// Test: cosmetic; exercised through `tests/memory_verbs_socket.rs`.
fn print_summary(outcome: &MemoryVerbOutcome, refusal: Option<&str>) {
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
            if let Some(reason) = refusal {
                println!("WARNING: slot refused: {reason} — drawer {drawer} was stored unslotted");
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
