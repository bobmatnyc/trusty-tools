//! `tm divert bulk-read` — the cheap worker the diversion hook steers to (#6887).
//!
//! Why: when `tm hook --divert-check` blocks an oversized read, this is the
//! command it names. It reads the files itself, asks headless Claude Code Haiku
//! the agent's question about them, and prints only the answer — so the
//! session's context carries a paragraph instead of two thousand lines. The raw
//! file content goes to the child over stdin and never reaches the parent
//! session's transcript.
//!
//! What: [`run`] dispatches the `bulk-read` action through
//! [`crate::commands::divert_worker`]. There is no provider, no credential and
//! no registry in this path — owner ruling 2026-09-07 settled the worker as
//! Claude Code's own Haiku under the developer's existing login, which the
//! child inherits via `CLAUDE_CONFIG_DIR`. On any worker failure this prints
//! [`FALLTHROUGH_MARKER`] and exits [`FALLTHROUGH_EXIT`], which the hook's own
//! block reason tells the agent to read as "retry with `offset`/`limit`". That
//! distinguishable signal is the point: a bare non-zero exit would look like a
//! transient failure worth retrying, and the agent would loop.
//!
//! Each SUCCESSFUL round trip appends one line to the session's diversion
//! ledger ([`record_diversion`]) carrying the running count and the child's own
//! token and cost numbers. A bare hook block appends nothing — only real worker
//! traffic is counted, because the number is meant to answer "what did
//! diversion save", not "how often did we say no".
//! Test: the `#[cfg(test)]` suite in `divert_tests.rs`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::cli::DivertAction;
use crate::commands::divert_worker::{DEFAULT_WORKER_MODEL, WorkerReply, spawn_worker, wrap_files};

/// Stdout marker meaning "no worker ran; do it yourself".
///
/// Why: the hook's block reason quotes this string verbatim, so the agent has a
/// literal to match rather than a heuristic on exit codes it cannot see. Change
/// it here and `divert_check::block_reason` must change with it — the test
/// `fallthrough_marker_matches_the_hook_reason` pins the pair.
/// What: the exact bytes printed on any worker failure.
/// Test: `fallthrough_marker_matches_the_hook_reason`.
pub(crate) const FALLTHROUGH_MARKER: &str = "divert: fall-through";

/// Process exit code accompanying [`FALLTHROUGH_MARKER`].
///
/// Why: a caller scripting around this command needs to branch without parsing
/// prose. `3` is unused by `tm`'s other exit codes (75 is daemon-unavailable,
/// 0/1/2 are the ordinary success/failure/usage set).
/// What: `3`.
/// Test: exercised by the CLI smoke in the PR body.
pub(crate) const FALLTHROUGH_EXIT: i32 = 3;

/// Prefix of the diversion ledger line.
///
/// Why (acceptance criterion 6): the line is the record, so it needs one stable
/// literal a test — and an operator — can grep for.
/// What: `"divert.diversion"`.
/// Test: `diversion_line_carries_the_count_and_the_child_usage`.
pub(crate) const DIVERSION_LOG_MARKER: &str = "divert.diversion";

/// Maximum bytes of file content sent to the worker in one call.
///
/// Why: the worker's context is finite. Truncating explicitly beats an API-side
/// error that reads as a transport failure and triggers the fall-through path
/// for a reason the operator cannot diagnose.
/// What: 400 KiB, roughly 100k tokens of source — comfortably more than the
/// files a `min_lines` threshold diverts, and bounded.
/// Test: `read_sources_truncates_past_the_budget`.
const MAX_CONTENT_BYTES: usize = 400 * 1024;

/// Dispatch a `tm divert` action.
///
/// Why: one entry point per command group, mirroring `commands::memory::run`.
/// What: currently only `bulk-read`. Prints the worker's answer on stdout and
/// exits [`FALLTHROUGH_EXIT`] when no worker could answer.
/// Test: `cli_parses_divert_bulk_read`.
pub(crate) async fn run(action: DivertAction) -> anyhow::Result<()> {
    match action {
        DivertAction::BulkRead {
            files,
            prompt,
            timeout_secs,
        } => bulk_read(files, prompt, timeout_secs).await,
    }
}

/// Answer a question about `files` on the cheap worker model.
///
/// Why: this is the whole point of the feature — the expensive session never
/// sees the bytes.
/// What: reads the files, asks the headless child, prints the answer, and
/// records one ledger line. Any worker failure prints [`FALLTHROUGH_MARKER`] to
/// stdout (so the agent, which only sees stdout, can act on it) with the detail
/// on stderr, then exits [`FALLTHROUGH_EXIT`].
/// Test: `read_sources_labels_each_file`,
/// `diversion_line_carries_the_count_and_the_child_usage`.
async fn bulk_read(
    files: Vec<PathBuf>,
    prompt: Option<String>,
    timeout_secs: u64,
) -> anyhow::Result<()> {
    let question = prompt.unwrap_or_else(|| {
        "Summarize these files: what they are for, their public surface, and \
         anything a reader must know before editing them."
            .to_string()
    });
    let sources = read_sources(&files)?;
    let payload = wrap_files(&sources, &question);
    let model = std::env::var(trusty_mpm::core::mcp_session_env::DIVERT_WORKER_MODEL_ENV)
        .ok()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_WORKER_MODEL.to_string());

    match spawn_worker(&model, &payload, Duration::from_secs(timeout_secs)).await {
        Ok(reply) => {
            println!("{}", reply.text);
            log_diversion(sources.len(), &reply);
            Ok(())
        }
        Err(reason) => {
            // stdout, because the agent reading this command's output is the
            // party that must act on it; the detail goes to stderr.
            println!("{FALLTHROUGH_MARKER}");
            eprintln!("divert: no worker answered ({reason})");
            std::process::exit(FALLTHROUGH_EXIT);
        }
    }
}

/// Read every requested file into `(path, content)` pairs.
///
/// Why: the worker needs to know which bytes came from which file, and the
/// caller needs a hard failure when a named file cannot be read — a silently
/// skipped file would produce an answer about the wrong thing.
/// What: one pair per file, stopping once [`MAX_CONTENT_BYTES`] of content is
/// gathered and noting the truncation inline.
/// Test: `read_sources_labels_each_file`, `read_sources_truncates_past_the_budget`.
fn read_sources(files: &[PathBuf]) -> anyhow::Result<Vec<(String, String)>> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut budget = MAX_CONTENT_BYTES;
    for file in files {
        let text = std::fs::read_to_string(file)
            .map_err(|e| anyhow::anyhow!("divert: cannot read {}: {e}", file.display()))?;
        let path = file.display().to_string();
        if text.len() <= budget {
            budget -= text.len();
            out.push((path, text));
            continue;
        }
        let mut cut = budget.min(text.len());
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        let mut truncated = text[..cut].to_string();
        truncated.push_str("\n… [truncated: content budget reached]");
        out.push((path, truncated));
        break;
    }
    Ok(out)
}

/// Write the diversion ledger line, and say so on stderr.
///
/// Why (acceptance criterion 6): #6873's usage ledger is not merged, so the
/// record is a line in the session's own log. Best-effort on the file — a
/// diversion that answered must not fail because a counter could not be
/// written.
/// What: increments the per-session count, emits the line at `info` (stderr, so
/// it never corrupts the answer on stdout), and appends it to the ledger file.
/// Test: `diversion_line_carries_the_count_and_the_child_usage`,
/// `record_diversion_counts_per_session`.
fn log_diversion(files: usize, reply: &WorkerReply) {
    let session = std::env::var("CLAUDE_CODE_SESSION_ID").unwrap_or_else(|_| "unknown".to_string());
    let count = trusty_common::resolve_data_dir("trusty-mpm")
        .ok()
        .and_then(|dir| record_diversion(&dir, &session, files, reply).ok())
        .unwrap_or(1);
    tracing::info!("{}", diversion_line(count, files, reply));
    eprintln!("{}", diversion_line(count, files, reply));
}

/// Append one diversion to a session's ledger and return its running count.
///
/// Why: `tm divert` is a fresh process per diversion, so "count per session"
/// can only come from something durable. The ledger file IS the count — one
/// line per diversion — so there is no second counter to drift.
/// What: appends [`diversion_line`] to `<data_dir>/divert/<session>.log` and
/// returns the number of lines it now holds. Takes the directory as an argument
/// so the whole thing is testable against a tempdir.
/// Test: `record_diversion_counts_per_session`.
pub(crate) fn record_diversion(
    data_dir: &Path,
    session_id: &str,
    files: usize,
    reply: &WorkerReply,
) -> std::io::Result<u64> {
    use std::io::Write;

    let dir = data_dir.join("divert");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.log", session_id.replace(['/', '\\'], "_")));
    let prior = std::fs::read_to_string(&path)
        .map(|s| s.lines().count() as u64)
        .unwrap_or(0);
    let count = prior + 1;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(file, "{}", diversion_line(count, files, reply))?;
    Ok(count)
}

/// Format one diversion ledger line.
///
/// Why: pure, so the exact literals a test (and an operator) greps for are
/// pinned in one place.
/// What: `divert.diversion count=… files=… model=… input_tokens=…
/// output_tokens=… cache_read_tokens=… cache_creation_tokens=… cost_usd=…`.
/// The cache counters are included because they dominate a Claude Code child's
/// prompt spend — a line reporting only `input_tokens` would read as far
/// cheaper than the call actually was.
/// Test: `diversion_line_carries_the_count_and_the_child_usage`.
pub(crate) fn diversion_line(count: u64, files: usize, reply: &WorkerReply) -> String {
    format!(
        "{DIVERSION_LOG_MARKER} count={count} files={files} model={} \
         input_tokens={} output_tokens={} cache_read_tokens={} \
         cache_creation_tokens={} cost_usd={:.6}",
        reply.model,
        reply.input_tokens,
        reply.output_tokens,
        reply.cache_read_tokens,
        reply.cache_creation_tokens,
        reply.cost_usd,
    )
}

#[cfg(test)]
#[path = "divert_tests.rs"]
mod tests;
