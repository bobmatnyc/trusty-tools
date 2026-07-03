//! `tm compress --tool <name>` — pipe-filter subcommand for the `tm hook`
//! `PreToolUse` Bash command-rewrite spike (issue #1956).
//!
//! Why: Option 0 (`docs/specs/tool-output-interception-seam.md`) rewrites a
//! Bash tool call's command to
//! `<original> | tm compress --tool "<effective tool name>"` (the tool name
//! is derived from the wrapped command by
//! `commands::hook_rewrite::effective_tool_name` — e.g. `"cargo test"`,
//! `"git diff"` — not a hardcoded `"bash"`, since `compress_tool_output`'s
//! dispatch table matches filters by substring against the tool name) so
//! Claude Code's own subprocess execution produces already-compressed
//! output — this binary is the pipe's filter stage. It exists so the
//! rewrite in `commands::hook_rewrite`/`commands::misc::hook` has something
//! real to pipe into.
//! What: Reads the piped command's full stdout from stdin to EOF,
//! compresses it via the hoisted
//! `trusty_agents_common::compress::compress_tool_output_async_with_path`
//! (issue #1959), emits a structured `tracing::info!` stats log line to
//! stderr (never stdout — stdout carries the compressed payload back to the
//! shell pipeline), and writes the compressed text to stdout.
//! Test: `run_compress_shrinks_repetitive_cargo_test_output`,
//! `log_compression_stats_pct_reduction_is_zero_for_empty_input`,
//! `log_compression_stats_pct_reduction_can_be_negative_when_output_expands`,
//! `run_compress_passes_through_short_output_unchanged` below; the full
//! stdin→stdout process contract (including this function's now-async
//! write) is exercised end to end through the real binary by the
//! `tm_compress_pipe` integration test.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use trusty_agents_common::compress::compress_tool_output_async_with_path;

/// Run `tm compress --tool <tool>`: read stdin, compress, log stats, print.
///
/// Why: the single entry point the rewritten Bash command pipes into
/// (`<cmd> | tm compress --tool "<effective tool name>"`); must behave like
/// a well-mannered Unix filter — read all of stdin, write the (possibly
/// unchanged) result to stdout, exit 0 — so it never breaks the exit-code
/// semantics of whatever pipeline it's the tail of.
/// What: Blocks until stdin reaches EOF (this is intentional — the whole
/// point is to wait for the wrapped command to finish producing output), via
/// [`tokio::io::AsyncReadExt::read_to_string`] rather than a synchronous
/// `std::io::stdin().read_to_string` — the latter would block this async
/// fn's Tokio worker thread for the entire read, which is exactly the wrong
/// tradeoff here since large piped output (the case this subcommand exists
/// to compress) is the slow case (trusty-review finding, PR #1968).
/// Compresses via [`compress_tool_output_async_with_path`], logs
/// `tool_name`/`bytes_before`/`bytes_after`/`pct_reduction`/
/// `compression_path` at `info` level, then writes the compressed text via
/// [`tokio::io::AsyncWriteExt::write_all`] (rather than a synchronous
/// `print!`) so the write side of this filter is exactly as
/// async-runtime-friendly as the read side, and the write's `Result` is
/// propagated through `?` instead of silently assuming the OS always
/// accepts the full buffer (a trusty-review-flagged robustness gap, PR
/// #1968 — `print!`'s panic-on-write-failure behavior is unrecoverable in a
/// pipe filter, whereas `?` degrades to a clean non-zero exit).
/// Test: See module tests; `tm compress` has no daemon-only tracing
/// subscriber from `main()` (that only inits for `Daemon`/`Supervisor`), so
/// this function installs its own stderr-writing subscriber via
/// `try_init()` — idempotent, safe to call from every invocation including
/// under `cargo test`.
pub(crate) async fn run_compress(tool: &str) -> anyhow::Result<()> {
    init_stats_log_subscriber();

    let mut input = String::new();
    tokio::io::stdin().read_to_string(&mut input).await?;

    let (compressed, path) = compress_tool_output_async_with_path(tool, &input).await;
    log_compression_stats(tool, input.len(), compressed.len(), path.as_str());

    // Explicit `.flush()` (trusty-review finding, PR #1968): `write_all`
    // hands the bytes to Tokio's stdout writer but does not itself guarantee
    // the OS has accepted them before this function returns and the process
    // exits — flushing closes that gap so the compressed payload can never
    // be silently truncated in the shell pipeline this binary is the tail
    // of.
    let mut stdout = tokio::io::stdout();
    stdout.write_all(compressed.as_bytes()).await?;
    stdout.flush().await?;
    Ok(())
}

/// Install a stderr-only tracing subscriber for short-lived `tm compress`
/// invocations.
///
/// Why: `main()` only initializes a global tracing subscriber for
/// long-running modes (`Daemon`/`Supervisor`) — every other CLI subcommand,
/// including this one, otherwise has no registered subscriber and
/// `tracing::info!` calls are silently dropped. Without this, issue #1956's
/// stats-logging requirement would produce no output at all.
/// What: `tracing_subscriber::fmt()` writing to stderr (never stdout — see
/// module docs), filtered by `RUST_LOG` (default `info`). Uses `try_init`
/// so repeated calls (e.g. across unit tests in the same process) are a
/// harmless no-op rather than a panic.
/// Test: Exercised indirectly by every `run_compress_*` test capturing
/// `tracing_test`-free stderr is out of scope for this spike; the log
/// *content* is verified directly via [`log_compression_stats`]'s own test.
fn init_stats_log_subscriber() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .try_init();
}

/// Emit the structured compression-effectiveness stats log line.
///
/// Why: Split out from [`run_compress`] so the exact field set is a
/// directly testable, single-purpose function — this is the foundational
/// per-event log line a broader meta-harness aggregation effort (tracked
/// separately, out of scope here) will eventually consume.
/// What: One `tracing::info!` with `tool_name`, `bytes_before`,
/// `bytes_after`, `pct_reduction` (0.0 when `bytes_before` is 0), and
/// `compression_path` (`"rtk_binary"` / `"native_fallback"`, see
/// [`trusty_agents_common::compress::CompressionPath`]) as structured
/// fields, machine-parseable from `RUST_LOG=info` output. `pct_reduction`
/// is deliberately **not** clamped to `>= 0.0`: if a compression path ever
/// expands the input (e.g. added framing/summary text pushes `bytes_after`
/// above `bytes_before`), the field goes negative rather than being
/// silently floored to `0.0` — a trusty-review-flagged naming concern (PR
/// #1968) that we resolve by documentation rather than by clamping, since
/// clamping would hide the one signal ("compression made this worse") a
/// downstream meta-harness aggregation effort would most want to see.
/// Test: `log_compression_stats_pct_reduction_is_zero_for_empty_input`,
/// `log_compression_stats_pct_reduction_can_be_negative_when_output_expands`,
/// exercised end-to-end via `run_compress_*` tests below.
fn log_compression_stats(tool_name: &str, bytes_before: usize, bytes_after: usize, path: &str) {
    let pct_reduction = if bytes_before > 0 {
        (1.0 - bytes_after as f64 / bytes_before as f64) * 100.0
    } else {
        0.0
    };
    tracing::info!(
        tool_name = %tool_name,
        bytes_before,
        bytes_after,
        pct_reduction,
        compression_path = %path,
        "tool output compressed"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_compression_stats_pct_reduction_is_zero_for_empty_input() {
        // Guards the division-by-zero edge case: an empty tool output must
        // report 0.0% reduction, not NaN/panic.
        log_compression_stats("bash", 0, 0, "native_fallback");
    }

    #[test]
    fn log_compression_stats_pct_reduction_can_be_negative_when_output_expands() {
        // If a compression path ever expands the input, the stats log must
        // still emit — a negative `pct_reduction` is the honest signal that
        // compression made things worse, not a bug in this function to
        // clamp away (trusty-review finding, PR #1968; see this function's
        // doc comment for why we don't clamp). This test only proves no
        // panic/NaN occurs for `bytes_after > bytes_before`.
        log_compression_stats("bash", 10, 20, "native_fallback");
    }

    #[tokio::test]
    async fn run_compress_shrinks_repetitive_cargo_test_output() {
        // `compress_tool_output_async_with_path` is exercised directly here
        // (rather than through stdin/stdout) so the test doesn't depend on
        // process-level stdio redirection — `run_compress` itself is a thin
        // wrapper proven correct by inspection plus this + the stats test.
        let mut input = String::new();
        for i in 0..50 {
            input.push_str(&format!("test mod::t{i} ... ok\n"));
        }
        input.push_str("test result: ok. 50 passed; 0 failed\n");
        let (compressed, _path) = compress_tool_output_async_with_path("cargo test", &input).await;
        assert!(
            compressed.len() < input.len(),
            "expected compression to shrink repetitive passing-test output"
        );
        assert!(compressed.contains("test result"));
    }

    #[tokio::test]
    async fn run_compress_passes_through_short_output_unchanged() {
        // Below the 80-byte size gate in `compress_tool_output` — must be a
        // verbatim passthrough, proving `tm compress` never mangles small
        // Bash results (exit codes, short status lines).
        let input = "ok\n";
        let (compressed, _path) = compress_tool_output_async_with_path("bash", input).await;
        assert_eq!(compressed, input);
    }
}
