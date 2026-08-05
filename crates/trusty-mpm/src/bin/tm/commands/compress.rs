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
//! shell pipeline), durably appends a `trusty-agents::compression::CompressionRecord`-
//! shaped record (issue #3867's schema, plus issue #3870's additive
//! `compression_path`) to `~/.trusty-mpm/compression.jsonl` (issue #3870,
//! epic #3866 Slice D — this doc comment's own former "will eventually
//! consume" note is now discharged), and writes the compressed text to
//! stdout.
//! Test: `run_compress_shrinks_repetitive_cargo_test_output`,
//! `log_compression_stats_pct_reduction_is_zero_for_empty_input`,
//! `log_compression_stats_pct_reduction_can_be_negative_when_output_expands`,
//! `run_compress_passes_through_short_output_unchanged`,
//! `append_compression_record_creates_file`,
//! `append_compression_record_appends` below; the full stdin→stdout process
//! contract (including this function's now-async write) is exercised end to
//! end through the real binary by the `tm_compress_pipe` integration test.

use serde::Serialize;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use trusty_agents_common::compress::compress_tool_output_async_with_path;

/// Run `tm compress --tool <tool>`: read stdin, compress, log stats, print.
///
/// Why: a well-mannered Unix filter — read all of stdin, write the (possibly
/// unchanged) result to stdout, exit 0 — so trimming a caller's output never
/// itself introduces a failure.
///
/// 🔴 Exiting 0 DISCARDS the upstream command's exit status; it does not
/// preserve it. A shell takes a pipeline's status from its LAST command, so
/// `cargo test … | tm compress` reports success on a failing suite. Callers
/// that need the verdict must capture it before trimming, never pipe:
///
/// ```bash
/// <gate> > /tmp/gate.txt 2>&1; echo "EXIT=$?"
/// tm compress --tool "cargo test" < /tmp/gate.txt   # only when non-zero
/// ```
///
/// The exit-0 contract itself is deliberate and unchanged — callers rely on
/// this filter never failing them. #4837: the comment previously justified it
/// as never breaking a pipeline's exit-code semantics, which is backwards. See
/// `assets/agents/BASE-AGENT.md`, "Never end a gate chain in a pipe".
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

    let started = std::time::Instant::now();
    let (compressed, path) = compress_tool_output_async_with_path(tool, &input).await;
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    log_compression_stats(tool, input.len(), compressed.len(), path.as_str());
    // #3870: durable sink, awaited (not spawned) because `tm compress` is a
    // short-lived pipe filter — see `append_compression_record`'s doc
    // comment for why a detached `tokio::spawn` would race process exit.
    append_compression_record(
        &compression_log_path(),
        tool,
        input.len(),
        compressed.len(),
        path.as_str(),
        duration_ms,
    )
    .await;

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

/// One row of `tm compress`'s durable compression-effectiveness log.
///
/// Why: Mirrors `trusty-agents`'s `compression::CompressionRecord` shape
/// field-for-field (issue #3867's schema, plus issue #3870's additive
/// `compression_path`) so the epic #3866 Slice C soak report can join all
/// three sinks (`trusty-code`'s, `trusty-agents`'s, this one) on identical
/// columns — `trusty-mpm` does not depend on the `trusty-agents` lib crate
/// (only `trusty-agents-common`), so this is an intentional small
/// duplication rather than a shared type. A code-critic pass on the initial
/// version of this file (PR #3885) caught a genuine schema divergence here
/// (a `pct_reduction` field with inverted-sign semantics vs. every other
/// sink's `ratio`, and five missing fields) — fixed by matching
/// `trusty-agents::compression::CompressionRecord` exactly.
/// What: ts (RFC3339), session_id (always `None` — `tm compress` has no
/// session concept), surface (always `"tm-compress"` here), surface_detail
/// (the tool name), byte-proxy token counts, `ratio` (`tokens_after /
/// tokens_before`, **not** a percentage and **not** `1 - ratio` — see
/// [`append_compression_record`]'s doc for why this differs from
/// [`log_compression_stats`]'s stderr-only `pct_reduction`),
/// `working_context_pct_after`/`overhead_pct_after` (always `None` —
/// `tcode`-only concepts), `compaction_event` (always `false`),
/// `duration_ms` (the real wall-clock of the compression call), `rounds`
/// (always `1`), and the RTK-vs-native path.
/// Test: `compression_record_serializes_to_valid_jsonl`.
#[derive(Debug, Clone, Serialize)]
struct CompressionRecord {
    ts: String,
    session_id: Option<String>,
    surface: &'static str,
    surface_detail: String,
    tokens_before: u32,
    tokens_after: u32,
    ratio: f64,
    working_context_pct_after: Option<u8>,
    overhead_pct_after: Option<u8>,
    compaction_event: bool,
    duration_ms: u64,
    rounds: u32,
    compression_path: String,
}

/// Byte-based token-count proxy — same `bytes / 4` heuristic as
/// `trusty-agents::compression::estimate_tokens_from_bytes` (issue #3870);
/// duplicated rather than imported since `trusty-mpm` doesn't depend on the
/// `trusty-agents` lib crate. Saturates rather than overflows/panics on a
/// pathological input.
fn estimate_tokens_from_bytes(bytes: usize) -> u32 {
    u32::try_from(bytes / 4).unwrap_or(u32::MAX)
}

/// Resolve `tm compress`'s durable compression-log path: `~/.trusty-mpm/compression.jsonl`.
///
/// Why: Unlike `trusty-agents`'s `tool_loop` (a long-running process inside
/// one project), `tm compress` is invoked once per rewritten Bash call with
/// no single project directory to root a per-project log under (it runs as
/// a pipe filter, potentially from any cwd) — so it follows the existing
/// `~/.trusty-mpm/` global-state-dir convention
/// (`core::config::MpmConfig::load_default`) instead of `usage.jsonl`'s
/// per-project `.trusty-agents/state/` convention.
/// What: `dirs::home_dir().join(".trusty-mpm").join("compression.jsonl")`;
/// falls back to a relative `.trusty-mpm/compression.jsonl` when home is
/// unavailable (stripped CI), matching `MpmConfig::load_default`'s
/// degrade-gracefully behavior.
/// Test: Indirectly via `append_compression_record_*` tests, which pass an
/// explicit path rather than calling this resolver.
fn compression_log_path() -> PathBuf {
    match dirs::home_dir() {
        Some(home) => home.join(".trusty-mpm").join("compression.jsonl"),
        None => PathBuf::from(".trusty-mpm").join("compression.jsonl"),
    }
}

/// Append one durable `CompressionRecord` line to `path`.
///
/// Why: `tm compress` is a short-lived pipe filter with no background
/// runtime to hand a detached write off to — unlike `trusty-agents`'s
/// `tool_loop` (which spawns the append and lets the process keep running),
/// a `tokio::spawn`ed write here would race the process's own exit and
/// could be silently dropped when the async runtime shuts down. So this is
/// **awaited inline** in `run_compress`, best-effort (any I/O/serialize
/// failure logs at debug level and is swallowed — a full disk must never
/// turn the pipe filter into a non-zero exit and break the caller's shell
/// pipeline). `ratio` uses the `tokens_after / tokens_before` convention
/// (0.0 when `tokens_before == 0`) — deliberately the OPPOSITE field and
/// sign convention from [`log_compression_stats`]'s stderr-only
/// `pct_reduction` (`(1 - after/before) * 100`); the two fields serve
/// different audiences (this one is the epic #3866 Slice C soak report's
/// join column and must match `trusty-agents`'s/`trusty-code`'s `ratio`
/// exactly, the stderr one is a human-readable percentage for live
/// debugging) and are intentionally NOT unified into one field.
/// What: `mkdir -p` the parent dir, then open `create + append`, write one
/// `serde_json::to_string(&record)` line + `\n`, flush.
/// Test: `append_compression_record_creates_file`,
/// `append_compression_record_appends`.
async fn append_compression_record(
    path: &Path,
    tool_name: &str,
    bytes_before: usize,
    bytes_after: usize,
    compression_path: &str,
    duration_ms: u64,
) {
    let tokens_before = estimate_tokens_from_bytes(bytes_before);
    let tokens_after = estimate_tokens_from_bytes(bytes_after);
    let ratio = if tokens_before > 0 {
        f64::from(tokens_after) / f64::from(tokens_before)
    } else {
        0.0
    };
    let record = CompressionRecord {
        ts: chrono::Utc::now().to_rfc3339(),
        session_id: None,
        surface: "tm-compress",
        surface_detail: tool_name.to_string(),
        tokens_before,
        tokens_after,
        ratio,
        working_context_pct_after: None,
        overhead_pct_after: None,
        compaction_event: false,
        duration_ms,
        rounds: 1,
        compression_path: compression_path.to_string(),
    };
    if let Some(parent) = path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let line = match serde_json::to_string(&record) {
        Ok(s) => format!("{s}\n"),
        Err(e) => {
            tracing::debug!(error = %e, "compression telemetry: serialize failed");
            return;
        }
    };
    match tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
    {
        Ok(mut f) => {
            if let Err(e) = f.write_all(line.as_bytes()).await {
                tracing::debug!(error = %e, path = %path.display(), "compression telemetry: write failed");
                return;
            }
            if let Err(e) = f.flush().await {
                tracing::debug!(error = %e, path = %path.display(), "compression telemetry: flush failed");
            }
        }
        Err(e) => {
            tracing::debug!(error = %e, path = %path.display(), "compression telemetry: open failed");
        }
    }
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

    // -- #3870 (epic #3866 Slice D): durable sink tests --------------------
    // Sibling assertions to the `log_compression_stats_*` tests above,
    // reusing the SAME bytes_before/bytes_after numbers rather than
    // recomputing them a second way (per the issue's test expectations).

    #[test]
    fn compression_record_serializes_to_valid_jsonl() {
        let record = CompressionRecord {
            ts: "2026-07-24T19:00:00Z".to_string(),
            session_id: None,
            surface: "tm-compress",
            surface_detail: "cargo test".to_string(),
            tokens_before: 100,
            tokens_after: 25,
            ratio: 0.25,
            working_context_pct_after: None,
            overhead_pct_after: None,
            compaction_event: false,
            duration_ms: 4,
            rounds: 1,
            compression_path: "native_fallback".to_string(),
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains('\n'), "JSONL invariant: single line");
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["surface"], "tm-compress");
        assert_eq!(parsed["surface_detail"], "cargo test");
        assert_eq!(parsed["ratio"], 0.25);
        assert_eq!(parsed["duration_ms"], 4);
        assert_eq!(parsed["rounds"], 1);
        assert_eq!(parsed["compaction_event"], false);
        assert_eq!(parsed["compression_path"], "native_fallback");
        assert!(parsed["session_id"].is_null());
        assert!(parsed["working_context_pct_after"].is_null());
        assert!(parsed["overhead_pct_after"].is_null());
    }

    #[tokio::test]
    async fn append_compression_record_creates_file() {
        // Same 400-byte-before/100-byte-after shape as
        // `log_compression_stats_pct_reduction_is_zero_for_empty_input`'s
        // sibling non-zero case — proves the durable append derives its
        // fields from the SAME bytes_before/bytes_after the stderr stats
        // line uses, not a second independent computation.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("compression.jsonl");
        append_compression_record(&path, "cargo test", 400, 100, "native_fallback", 7).await;
        assert!(path.exists(), "compression.jsonl should be created");
        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 1, "exactly one line after one append");
        let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["surface"], "tm-compress");
        assert_eq!(parsed["surface_detail"], "cargo test");
        assert_eq!(parsed["tokens_before"], 100);
        assert_eq!(parsed["tokens_after"], 25);
        assert_eq!(parsed["ratio"], 0.25);
        assert_eq!(parsed["duration_ms"], 7);
        assert_eq!(parsed["rounds"], 1);
        assert_eq!(parsed["compaction_event"], false);
        assert_eq!(parsed["compression_path"], "native_fallback");
        assert!(parsed["session_id"].is_null());
    }

    #[tokio::test]
    async fn append_compression_record_appends() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("compression.jsonl");
        append_compression_record(&path, "cargo test", 400, 100, "rtk_binary", 1).await;
        append_compression_record(&path, "git diff", 200, 200, "native_fallback", 2).await;
        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2, "second append should not overwrite");
        let p1: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        let p2: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(p1["surface_detail"], "cargo test");
        assert_eq!(p1["compression_path"], "rtk_binary");
        assert_eq!(p2["surface_detail"], "git diff");
        assert_eq!(
            p2["ratio"], 1.0,
            "unchanged-size compression must report ratio 1.0, not 0.0"
        );
    }

    /// #3885 code-critic MEDIUM: an unwritable durable-log directory must
    /// never fail the pipe filter's actual job (compressing + returning
    /// output) — mirrors PR #3880's
    /// `unwritable_data_dir_does_not_fail_the_loop` pattern. Simulates
    /// "unwritable" by pointing the sink at a path whose PARENT is a plain
    /// file (so `create_dir_all`/`OpenOptions::open` both fail), the same
    /// technique used by the `trusty-agents` siblings of this test.
    #[tokio::test]
    async fn unwritable_log_dir_does_not_panic_or_block_the_append() {
        let dir = tempfile::tempdir().unwrap();
        let blocked = dir.path().join("not-a-dir");
        tokio::fs::write(&blocked, b"i am a file, not a directory")
            .await
            .unwrap();
        let path = blocked.join("compression.jsonl");

        // Must return normally (best-effort swallow), not panic or hang.
        append_compression_record(&path, "cargo test", 400, 100, "native_fallback", 1).await;
        assert!(
            !path.exists(),
            "the record must not have been written under an unwritable parent"
        );
    }

    #[test]
    fn estimate_tokens_from_bytes_divides_by_four() {
        assert_eq!(estimate_tokens_from_bytes(400), 100);
        assert_eq!(estimate_tokens_from_bytes(0), 0);
    }

    #[test]
    fn compression_log_path_ends_with_trusty_mpm_compression_jsonl() {
        let path = compression_log_path();
        assert!(path.ends_with(".trusty-mpm/compression.jsonl"));
    }
}
