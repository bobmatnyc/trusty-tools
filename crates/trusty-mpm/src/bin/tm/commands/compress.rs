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
//! consume" note is now discharged), appends one `compress` savings row for a
//! run that actually shrank its input (see
//! [`trusty_mpm::core::savings_compress`], which is what puts bash and gate
//! output into the `💸` statusline segment), and writes the compressed text to
//! stdout. Two things ride on top of that: the stats line reports the WRAPPED
//! command's exit status, which the rewritten pipeline appends as a sentinel
//! line and [`split_exit_sentinel`] strips back off (#7384), and a compression
//! that emptied a non-empty input hands the raw text back with a warning
//! instead of returning nothing ([`compress_with_raw_fallback`], #7377). The
//! stats line is emitted only when it carries something — a run that returned
//! its input byte-for-byte with nothing to report stays silent rather than
//! narrating `pct_reduction=0.0` into the caller's own tool result
//! ([`stats_line_is_informative`], #7607).
//! Test: `run_compress_shrinks_repetitive_cargo_test_output`,
//! `a_passthrough_run_emits_no_stats_line`,
//! `a_failing_wrapped_command_still_emits_the_stats_line`,
//! `an_expanding_run_still_emits_the_stats_line`,
//! `log_compression_stats_pct_reduction_is_zero_for_empty_input`,
//! `log_compression_stats_pct_reduction_can_be_negative_when_output_expands`,
//! `native_fallback_elision_reports_a_matching_non_zero_reduction`,
//! `run_compress_passes_through_short_output_unchanged`,
//! `split_exit_sentinel_reads_and_strips_a_trailing_status`,
//! `rtk_returning_nothing_falls_back_to_the_raw_output_and_warns_once`,
//! `append_compression_record_creates_file`,
//! `append_compression_record_appends` below; the full stdin→stdout process
//! contract (including this function's now-async write) is exercised end to
//! end through the real binary by the `tm_compress_pipe` integration test.

use serde::Serialize;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use trusty_agents_common::compress::{
    CompressionPath, RtkResolver, compress_tool_output_async_with_path_using, default_rtk_resolver,
};

/// Head of the exit-status line the rewritten pipeline appends to stdin (#7384).
///
/// Why: a pipeline hands its filter stdout, never the upstream command's exit
/// status, so `bytes_before=0` read the same whether the command found nothing,
/// failed, or had its stdout redirected away. The shell knows the status; a
/// trailing sentinel line is the one way to carry it into a process the shell
/// started concurrently.
/// What: the producer is [`wrap_command_reporting_exit`], the consumer
/// [`split_exit_sentinel`]. A wrapped command that prints this exact line
/// itself would have it stripped and its status misreported — accepted, since
/// the spelling is not one real output produces.
/// Test: `split_exit_sentinel_reads_and_strips_a_trailing_status`.
const EXIT_SENTINEL_PREFIX: &str = "__tm_compress_exit=";

/// Tail of the exit-status sentinel line. See [`EXIT_SENTINEL_PREFIX`].
const EXIT_SENTINEL_SUFFIX: &str = "__";

/// Wrap a Bash command so its exit status reaches this filter (#7384).
///
/// Why: `commands::hook_rewrite` pipes the wrapped command into `tm compress`.
/// `$?` cannot be read from inside that pipeline — the shell expands every
/// argument before any element runs — so the status is appended to the stream
/// the filter already reads, by a `printf` that runs after the command in the
/// same brace group.
/// What: `{ <command>; printf '\n<sentinel>\n' "$?"; }`. The whole group is one
/// physical line, so the caller owes this function a command that cannot break
/// it: `hook_rewrite::has_unsafe_pipe_composition` rejects `|`, `&`, `;`, `>`
/// and `<`, and `hook_rewrite::cannot_be_brace_wrapped` rejects a `#` (which
/// would comment out the `printf` and the closing brace) and a trailing
/// unescaped `\` (which would escape the `;` and turn the `printf` into
/// arguments). What reaches here is a simple command, and the group is then
/// valid in sh, bash and zsh alike. The leading newline keeps the sentinel on
/// its own line whether or not the command's output ended in one, and
/// [`split_exit_sentinel`] removes exactly the bytes this adds.
/// Test: `wrap_command_reporting_exit_round_trips_through_split`,
/// `rewrite_appends_compress_pipe_for_plain_command`, and the process-level
/// `tm_compress_reports_the_wrapped_commands_exit_status`.
pub(crate) fn wrap_command_reporting_exit(command: &str) -> String {
    format!(
        "{{ {command}; printf '\\n{EXIT_SENTINEL_PREFIX}%s{EXIT_SENTINEL_SUFFIX}\\n' \"$?\"; }}"
    )
}

/// Split stdin into the wrapped command's own output and its exit status.
///
/// Why: the sentinel is telemetry, not output — leaving it in would put a
/// `__tm_compress_exit=0__` line in front of every agent reading a wrapped
/// command's result (#7384).
/// What: looks at the last line, ignoring one trailing newline. When it is a
/// sentinel, returns everything before the newline that introduced it plus the
/// parsed status; otherwise returns the input untouched and `None`, which is
/// what an unwrapped invocation (`tm compress < file`) and an older rewrite
/// both produce.
/// Test: `split_exit_sentinel_reads_and_strips_a_trailing_status`,
/// `split_exit_sentinel_leaves_unwrapped_input_untouched`,
/// `wrap_command_reporting_exit_round_trips_through_split`.
fn split_exit_sentinel(stdin_text: &str) -> (&str, Option<i32>) {
    let body = stdin_text.strip_suffix('\n').unwrap_or(stdin_text);
    let (head, last) = match body.rfind('\n') {
        Some(newline) => (&body[..newline], &body[newline + 1..]),
        None => ("", body),
    };
    match parse_exit_sentinel(last) {
        Some(code) => (head, Some(code)),
        None => (stdin_text, None),
    }
}

/// Parse one sentinel line into a shell exit status.
///
/// A signal-killed command reaches the shell as `128 + signal`, so the same
/// numeric range covers it — 137 for `SIGKILL`, no separate spelling (#7384).
/// Values outside a shell's `0..=255` are rejected so a line that merely looks
/// like the sentinel cannot be mistaken for one.
/// Test: `parse_exit_sentinel_rejects_out_of_range_and_malformed_lines`.
fn parse_exit_sentinel(line: &str) -> Option<i32> {
    let digits = line
        .strip_prefix(EXIT_SENTINEL_PREFIX)?
        .strip_suffix(EXIT_SENTINEL_SUFFIX)?;
    let code: i32 = digits.parse().ok()?;
    (0..=255).contains(&code).then_some(code)
}

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
/// Compresses via [`compress_with_raw_fallback`], logs
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

    let mut stdin_text = String::new();
    tokio::io::stdin().read_to_string(&mut stdin_text).await?;
    // #7384: the wrapped command's status rides in on a trailing sentinel line;
    // strip it here so it never reaches the caller's output.
    let (input, exit) = split_exit_sentinel(&stdin_text);

    let started = std::time::Instant::now();
    let (compressed, path) = compress_with_raw_fallback(default_rtk_resolver(), tool, input).await;
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let _pct = log_compression_stats(tool, input.len(), compressed.len(), path.as_str(), exit);
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
    // The 💸 statusline segment folds this technique too, so the same run that
    // writes the telemetry record also writes one savings row.
    record_compress_savings(input.len(), compressed.len(), path.as_str());

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

/// Compress `input`, handing back the raw text when compression emptied it.
///
/// Why: the `rtk_binary` path returns whatever the subprocess printed, and rtk
/// can exit zero having printed nothing — an 824-byte `git diff --stat` came
/// back as 0 bytes, reported as a successful 100% reduction, and the caller
/// lost the diff (#7377). #6986 put the same backstop inside
/// [`trusty_agents_common::compress::compress_tool_output`], but that guard
/// only covers the native filter chain; the rtk subprocess never reaches it.
/// What: runs [`compress_tool_output_async_with_path_using`] against `resolve`,
/// then returns `input` verbatim when the result is blank and the input was
/// not. The warning carries the tool, the input size and the path that emptied
/// it, so the fallback is visible in the same stderr stream as the stats line
/// rather than being silently indistinguishable from a real reduction. The
/// reported [`CompressionPath`] stays the path that actually ran.
/// Test: `rtk_returning_nothing_falls_back_to_the_raw_output_and_warns_once`,
/// `a_real_compression_is_not_treated_as_an_empty_result`.
async fn compress_with_raw_fallback(
    resolve: RtkResolver,
    tool: &str,
    input: &str,
) -> (String, CompressionPath) {
    let (compressed, path) = compress_tool_output_async_with_path_using(resolve, tool, input).await;
    // #7377: an empty result for a non-empty input destroyed the caller's
    // output rather than shortening it.
    if compressed.trim().is_empty() && !input.trim().is_empty() {
        tracing::warn!(
            tool_name = %tool,
            bytes_before = input.len(),
            compression_path = %path.as_str(),
            "compression returned no output for a non-empty input; falling back to the raw output"
        );
        return (input.to_string(), path);
    }
    (compressed, path)
}

/// Append this run's savings row to the ledger the `💸` statusline segment folds.
///
/// Why: `tm compress` avoids sending tokens exactly the way instruction folding
/// and bulk-read diversion do, but until this call it wrote only its own
/// `compression.jsonl` telemetry — so the segment reported everything the
/// harness saves EXCEPT bash and gate output. The two writes live side by side
/// because they describe the same event and must never disagree about its byte
/// counts.
/// What: resolves the session id Claude Code exports and the framework root the
/// statusline reads from — the same `--root` / `TRUSTY_MPM_ROOT` / config chain
/// [`crate::commands::divert`] uses — then hands both byte counts and the
/// compression-path label to
/// [`trusty_mpm::core::savings_compress::record_compress`], which declines a
/// passthrough run rather than writing a zero-saving row. Declines quietly with
/// no session id or no resolvable root; never fails the compression.
///
/// #7514: the two ambient reads are ISOLATED here and nowhere else, so the only
/// way a caller reaches the operator's ledger is by going through this function.
/// `run_compress` is the one that does; every test drives
/// [`record_compress_savings_with`] or the producer directly, and the
/// `tests/tm_compress_pipe.rs` helpers — which spawn the real binary and so
/// cannot be given a Rust parameter — clear `CLAUDE_CODE_SESSION_ID` from the
/// CHILD's environment instead. A test that appends a row keyed by the
/// developer's own live session id is the defect #7514 names.
/// Test: `record_compress_savings_with_declines_without_a_session_id`.
fn record_compress_savings(bytes_before: usize, bytes_after: usize, compression_path: &str) {
    let root = match crate::commands::managed_root::resolve_managed_paths(None) {
        Ok(paths) => Some(paths.root),
        Err(source) => {
            tracing::warn!(
                %source,
                "cannot resolve the framework root: writing no compress savings row"
            );
            None
        }
    };
    record_compress_savings_with(
        root.as_deref(),
        trusty_mpm::core::savings::claude_code_session_id().as_deref(),
        bytes_before,
        bytes_after,
        compression_path,
    );
}

/// [`record_compress_savings`] with the framework root and session id supplied.
///
/// Why (#7514): `resolve_managed_paths` reads the operator's `--root` /
/// `TRUSTY_MPM_ROOT` / config chain and `claude_code_session_id` reads the
/// harness variable, so a test that reached this code wrote a real row into the
/// operator's real ledger under the developer's own live session id. Injecting
/// both — rather than setting them, which the `src/bin/tm/**` env ratchet
/// (#5544) forbids — keeps the decision testable and leaves exactly one
/// ambient call site.
/// What: declines with no session id, a blank one, or no resolvable root;
/// otherwise defers to
/// [`trusty_mpm::core::savings_compress::record_compress`], which declines a
/// passthrough run itself.
/// Test: `record_compress_savings_with_declines_without_a_session_id`,
/// `record_compress_savings_with_writes_under_the_named_root`.
fn record_compress_savings_with(
    root: Option<&std::path::Path>,
    session_id: Option<&str>,
    bytes_before: usize,
    bytes_after: usize,
    compression_path: &str,
) {
    let Some(session_id) = session_id.map(str::trim).filter(|id| !id.is_empty()) else {
        tracing::debug!("no claude session id: writing no compress savings row");
        return;
    };
    let Some(root) = root else {
        return;
    };
    trusty_mpm::core::savings_compress::record_compress(
        root,
        session_id,
        bytes_before,
        bytes_after,
        compression_path,
    );
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
///
/// #7180: the percentage is also RETURNED, not only logged, so a test can
/// assert the number the stats line carries. The two edge-case tests below
/// previously proved only that logging did not panic, which left a claim of
/// `pct_reduction=0.0` on an eliding path unfalsifiable.
///
/// #7384: the line also carries `exit`, the WRAPPED command's status — not
/// this filter's, which is always 0. Without it `bytes_before=0` read
/// identically whether the command found nothing, failed, or had its stdout
/// dropped; a zsh `no matches found:` error was invisible on one such call.
/// `exit=unknown` when stdin carried no sentinel, which is what an unwrapped
/// `tm compress < file` produces.
///
/// #7607: the line is emitted only when it carries something, per
/// [`stats_line_is_informative`]. A passthrough run — output returned
/// byte-for-byte, `pct_reduction=0.0` — used to narrate itself into the middle
/// of the caller's own tool result.
/// Test: `log_compression_stats_pct_reduction_is_zero_for_empty_input`,
/// `log_compression_stats_pct_reduction_can_be_negative_when_output_expands`,
/// `native_fallback_elision_reports_a_matching_non_zero_reduction`,
/// `a_passthrough_run_emits_no_stats_line`,
/// exercised end-to-end via `run_compress_*` tests below and, for the exit
/// field, by `tm_compress_reports_the_wrapped_commands_exit_status`.
fn log_compression_stats(
    tool_name: &str,
    bytes_before: usize,
    bytes_after: usize,
    path: &str,
    exit: Option<i32>,
) -> f64 {
    // #7180: the emitted percentage is also RETURNED so a test can assert the
    // number itself, not merely that logging did not panic.
    let pct_reduction = if bytes_before > 0 {
        (1.0 - bytes_after as f64 / bytes_before as f64) * 100.0
    } else {
        0.0
    };
    // #7607: a no-op run says nothing, so it says nothing.
    if !stats_line_is_informative(bytes_before, bytes_after, exit) {
        return pct_reduction;
    }
    // #7384: `%` so the field renders as `exit=3`, not a quoted `exit="3"`.
    let exit_status = exit.map_or_else(|| "unknown".to_string(), |code| code.to_string());
    tracing::info!(
        tool_name = %tool_name,
        bytes_before,
        bytes_after,
        pct_reduction,
        compression_path = %path,
        exit = %exit_status,
        "tool output compressed"
    );
    pct_reduction
}

/// Whether this run's stats line carries anything the reader did not already have.
///
/// Why (#7607): `tm compress` is the tail of a pipeline whose stdout IS the
/// agent's tool result, and its stats line goes to stderr — which Claude Code
/// interleaves into that same result. On a passthrough (input below the size
/// gate, or a tool name with no dispatch branch) the filter returns the bytes
/// unchanged and the line reports `pct_reduction=0.0`: pure narration wedged
/// into the middle of a `grep` result, the symptom #7607 reports. Suppressing it
/// under a MINIMUM PAYLOAD SIZE — the issue's other suggestion — would be the
/// wrong test: an 80 KB output that compresses to exactly 80 KB is equally
/// uninformative, and a 3-byte output from a command that exited 3 is not.
/// What: `true` when the byte counts moved AT ALL — a reduction, and equally an
/// expansion, which [`log_compression_stats`] documents as the one signal a
/// reader most wants — or when the wrapped command reported a failing status
/// (#7384's `exit` field, the other reason the line exists). `false` only for
/// the exact no-op: same bytes out as in, and no failure to report. An
/// `exit=unknown` run is an unwrapped invocation, which reports no failure.
///
/// FAIL-OPEN BY CONSTRUCTION: every input this cannot read as a proven no-op
/// keeps the line. Nothing is suppressed on a measurement that did not happen.
/// Test: `a_passthrough_run_emits_no_stats_line`,
/// `a_failing_wrapped_command_still_emits_the_stats_line`,
/// `an_expanding_run_still_emits_the_stats_line`.
fn stats_line_is_informative(bytes_before: usize, bytes_after: usize, exit: Option<i32>) -> bool {
    bytes_after != bytes_before || exit.is_some_and(|code| code != 0)
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
    use trusty_agents_common::compress::no_rtk;

    /// Compress through the native chain, whatever the host has installed.
    ///
    /// Why: `trusty_common::bin_resolve::resolve_binary` finds a Homebrew
    /// `rtk` whatever `PATH` says, so every assertion below about the native
    /// chain's own output — the 80-byte size gate, the line-list elision —
    /// passed or failed by host until the resolver seam existed (#7325).
    /// What: [`compress_tool_output_async_with_path_using`] with the
    /// never-resolving [`no_rtk`] resolver. No env mutation, so the
    /// `src/bin/tm/**` env-isolation ratchet stays intact.
    /// Test: every caller in this module.
    async fn native_compress(tool: &str, input: &str) -> (String, CompressionPath) {
        compress_tool_output_async_with_path_using(no_rtk, tool, input).await
    }

    #[test]
    fn log_compression_stats_pct_reduction_is_zero_for_empty_input() {
        // Guards the division-by-zero edge case: an empty tool output must
        // report 0.0% reduction, not NaN/panic.
        assert_eq!(
            log_compression_stats("bash", 0, 0, "native_fallback", None),
            0.0
        );
    }

    #[tokio::test]
    async fn native_fallback_elision_reports_a_matching_non_zero_reduction() {
        // #7180: the `native_fallback` path was reported as eliding lines
        // while announcing `pct_reduction=0.0`. Before this test nothing
        // asserted the emitted number at all — the two sibling
        // `log_compression_stats_*` tests only proved it did not panic — so a
        // stale or zeroed metric on an eliding path was unfalsifiable. This
        // pins the whole chain: an eliding compression, the path it took, and
        // the percentage the stats line carries.
        let mut input = String::new();
        for i in 0..90 {
            input.push_str(&format!(
                "crates/x/src/lib.rs:{i}: fn candidate_{i}() -> bool\n"
            ));
        }
        // #7325: force the native chain through the resolver seam. The default
        // resolver finds a Homebrew `rtk` whatever PATH says, so without this
        // the assertion below passed only on hosts without rtk installed.
        let (compressed, path) = native_compress("grep -n", &input).await;
        assert_eq!(path.as_str(), "native_fallback");
        assert!(
            compressed.contains("lines omitted"),
            "expected the line-list cap to elide: {compressed}"
        );
        assert!(compressed.len() < input.len(), "expected fewer bytes out");

        let pct = log_compression_stats(
            "grep -n",
            input.len(),
            compressed.len(),
            path.as_str(),
            Some(0),
        );
        let expected = (1.0 - compressed.len() as f64 / input.len() as f64) * 100.0;
        assert!(
            (pct - expected).abs() < f64::EPSILON,
            "reported {pct} does not match the byte delta {expected}"
        );
        assert!(pct > 0.0, "an eliding compression must not report 0.0");
    }

    #[test]
    fn log_compression_stats_pct_reduction_can_be_negative_when_output_expands() {
        // If a compression path ever expands the input, the stats log must
        // still emit — a negative `pct_reduction` is the honest signal that
        // compression made things worse, not a bug in this function to
        // clamp away (trusty-review finding, PR #1968; see this function's
        // doc comment for why we don't clamp). This test only proves no
        // panic/NaN occurs for `bytes_after > bytes_before`.
        assert_eq!(
            log_compression_stats("bash", 10, 20, "native_fallback", None),
            -100.0
        );
    }

    /// Why (#7607): a 668-byte `grep` result came back unchanged and the stats
    /// line reporting `pct_reduction=0.0` landed in the middle of it, in the
    /// agent's own tool result. The predicate is what decides that, so it is
    /// what this pins.
    /// Test: itself.
    #[test]
    fn a_passthrough_run_emits_no_stats_line() {
        assert!(
            !stats_line_is_informative(668, 668, Some(0)),
            "an unchanged payload from a command that succeeded says nothing"
        );
        assert!(
            !stats_line_is_informative(668, 668, None),
            "an unwrapped invocation reports no failure, so it is no different"
        );
        assert!(
            !stats_line_is_informative(0, 0, Some(0)),
            "an empty payload is the same no-op"
        );
    }

    /// Why (#7607, guarding #7384): the exit status is the OTHER reason this
    /// line exists. Suppressing a no-op must not suppress a failure report —
    /// a zsh `no matches found:` error arrives as exactly this shape, zero
    /// bytes moved and a non-zero status.
    /// Test: itself.
    #[test]
    fn a_failing_wrapped_command_still_emits_the_stats_line() {
        assert!(stats_line_is_informative(0, 0, Some(1)));
        assert!(stats_line_is_informative(668, 668, Some(3)));
        assert!(stats_line_is_informative(0, 0, Some(137)));
    }

    /// Why (#7607, guarding PR #1968's ruling): an expansion is the one signal
    /// the line's own doc comment says must never be hidden, so the
    /// no-op suppression must not reach it.
    /// Test: itself.
    #[test]
    fn an_expanding_run_still_emits_the_stats_line() {
        assert!(stats_line_is_informative(10, 20, None));
        assert!(stats_line_is_informative(1076, 36, Some(0)));
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
        // #7325: the shrinkage asserted below is the NATIVE chain's, so force
        // it — with the default resolver this measured whichever chain the
        // host happened to have.
        let (compressed, _path) = native_compress("cargo test", &input).await;
        assert!(
            compressed.len() < input.len(),
            "expected compression to shrink repetitive passing-test output"
        );
        assert!(compressed.contains("test result"));
    }

    /// Why (#7514): `tm compress`'s savings write read the operator's framework
    /// root and the live `CLAUDE_CODE_SESSION_ID`, so a test that reached it
    /// appended a real `compress` row to the operator's ledger keyed by the
    /// developer's own session. The decline must be a property of the injected
    /// inputs, not of the machine.
    /// Test: itself.
    #[test]
    fn record_compress_savings_with_declines_without_a_session_id() {
        let root = tempfile::tempdir().expect("temp root");
        for id in [None, Some(""), Some("   ")] {
            record_compress_savings_with(Some(root.path()), id, 10_000, 1_000, "native");
        }
        record_compress_savings_with(None, Some("c1"), 10_000, 1_000, "native");
        assert!(
            !root.path().join("usage").exists(),
            "no session id, or no root, must write nothing at all"
        );
    }

    /// Why (#7514): the other half — with both inputs supplied the row must land
    /// under the root the CALLER named, or the injection would be decoration.
    /// Test: itself.
    #[test]
    fn record_compress_savings_with_writes_under_the_named_root() {
        let root = tempfile::tempdir().expect("temp root");

        record_compress_savings_with(Some(root.path()), Some("c-7514"), 10_000, 1_000, "native");

        let ledger = trusty_mpm::core::savings::savings_log_in(root.path());
        let written = std::fs::read_to_string(&ledger).expect("the named root must hold the row");
        assert!(
            written.contains("c-7514") && written.contains("\"technique\":\"compress\""),
            "the row must be keyed by the supplied session id: {written}"
        );
    }

    #[tokio::test]
    async fn run_compress_passes_through_short_output_unchanged() {
        // Below the 80-byte size gate in `compress_tool_output` — must be a
        // verbatim passthrough, proving `tm compress` never mangles small
        // Bash results (exit codes, short status lines). The gate is the
        // native chain's, so #7325's seam forces that chain: rtk applies no
        // size gate and rewrites even three bytes.
        let input = "ok\n";
        let (compressed, _path) = native_compress("bash", input).await;
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

    // -- #7384: the wrapped command's exit status ---------------------------

    #[test]
    fn split_exit_sentinel_reads_and_strips_a_trailing_status() {
        // The exact stream `wrap_command_reporting_exit` produces for a command
        // whose own output already ended in a newline.
        let (payload, exit) = split_exit_sentinel("boom\n\n__tm_compress_exit=3__\n");
        assert_eq!(payload, "boom\n", "the sentinel must not reach the caller");
        assert_eq!(exit, Some(3));

        // A command whose output did NOT end in a newline: the newline the
        // sentinel brought with it is the one that gets removed.
        let (payload, exit) = split_exit_sentinel("boom\n__tm_compress_exit=0__\n");
        assert_eq!(payload, "boom");
        assert_eq!(exit, Some(0));

        // A command that printed nothing at all.
        let (payload, exit) = split_exit_sentinel("\n__tm_compress_exit=127__\n");
        assert_eq!(payload, "");
        assert_eq!(exit, Some(127));

        // Signal-killed reaches the shell as 128 + signal; 137 is SIGKILL.
        let (_, exit) = split_exit_sentinel("out\n\n__tm_compress_exit=137__\n");
        assert_eq!(exit, Some(137));
    }

    #[test]
    fn split_exit_sentinel_leaves_unwrapped_input_untouched() {
        // `tm compress < file`, and any rewrite older than #7384, carry no
        // sentinel — the payload must come back byte-for-byte.
        for raw in ["plain output\n", "no trailing newline", "", "\n"] {
            let (payload, exit) = split_exit_sentinel(raw);
            assert_eq!(payload, raw, "unwrapped input must pass through: {raw:?}");
            assert_eq!(exit, None);
        }
    }

    #[test]
    fn parse_exit_sentinel_rejects_out_of_range_and_malformed_lines() {
        assert_eq!(parse_exit_sentinel("__tm_compress_exit=3__"), Some(3));
        assert_eq!(parse_exit_sentinel("__tm_compress_exit=255__"), Some(255));
        // A shell status is 0..=255; anything else is a line that merely looks
        // like the sentinel.
        assert_eq!(parse_exit_sentinel("__tm_compress_exit=256__"), None);
        assert_eq!(parse_exit_sentinel("__tm_compress_exit=-1__"), None);
        assert_eq!(parse_exit_sentinel("__tm_compress_exit=ok__"), None);
        assert_eq!(parse_exit_sentinel("__tm_compress_exit=3"), None);
        assert_eq!(parse_exit_sentinel("test result: ok. 3 passed"), None);
    }

    #[test]
    fn wrap_command_reporting_exit_round_trips_through_split() {
        // The producer and the consumer must agree on the exact spelling; this
        // pins them to each other rather than to two hand-written literals.
        let wrapped = wrap_command_reporting_exit("cargo test");
        assert!(
            wrapped.starts_with("{ cargo test; printf "),
            "the command must run first inside the brace group: {wrapped}"
        );
        assert!(
            wrapped.contains(EXIT_SENTINEL_PREFIX) && wrapped.contains("\"$?\""),
            "the sentinel must carry the command's own status: {wrapped}"
        );
        // What the shell would emit for a command printing "out\n" and exiting 3.
        let stream = format!("out\n\n{EXIT_SENTINEL_PREFIX}3{EXIT_SENTINEL_SUFFIX}\n");
        assert_eq!(split_exit_sentinel(&stream), ("out\n", Some(3)));
    }

    // -- #7377: an empty compression falls back to the raw output -----------

    /// Raise the process-global tracing level so the thread-local capture below
    /// can see `warn!` (#4931).
    ///
    /// Why: `tracing`'s macros short-circuit on a process-global `MAX_LEVEL`
    /// that only a GLOBAL default subscriber raises, so a `with_default`
    /// capture records nothing unless something else in the binary happened to
    /// install one first. `trusty_mpm::test_support::enable_event_capture` is
    /// the library's copy of this and is not reachable from this bin target.
    /// What: installs a bare registry once per process, then asserts the
    /// resulting level admits `WARN` so a filtered global installed elsewhere
    /// fails here by name instead of as an empty capture.
    /// Test: the capture test below is vacuous without it.
    fn enable_event_capture() {
        static RAISE_MAX_LEVEL: std::sync::Once = std::sync::Once::new();
        RAISE_MAX_LEVEL.call_once(|| {
            let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry());
        });
        assert!(
            tracing::level_filters::LevelFilter::current() >= tracing::Level::WARN,
            "the process-global tracing level is {:?}, which discards WARN before \
             any subscriber sees it (#4931)",
            tracing::level_filters::LevelFilter::current()
        );
    }

    /// Collects a subscriber's output so a test can read back what was logged.
    #[derive(Clone, Default)]
    struct CaptureWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("capture lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
        type Writer = CaptureWriter;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// Path of this process's stub rtk. See [`empty_rtk`].
    #[cfg(unix)]
    fn empty_rtk_stub_path() -> PathBuf {
        std::env::temp_dir().join(format!("tm-compress-empty-rtk-{}.sh", std::process::id()))
    }

    /// An [`RtkResolver`] naming a stub rtk that prints nothing.
    ///
    /// Why: [`RtkResolver`] is a plain `fn` pointer, so it cannot close over a
    /// `TempDir` — the stub is written by the resolver itself, at a path keyed
    /// on this process's pid so two test binaries never share one.
    /// What: an `/bin/sh` script that drains stdin (otherwise the writer in
    /// `compress_via_rtk_binary` takes `EPIPE`, the rtk arm reports failure,
    /// and the native chain would answer instead) and exits zero having
    /// written nothing — exactly the #7377 shape.
    /// Test: `rtk_returning_nothing_falls_back_to_the_raw_output_and_warns_once`.
    #[cfg(unix)]
    fn empty_rtk(_name: &str) -> Option<PathBuf> {
        use std::os::unix::fs::PermissionsExt;
        let path = empty_rtk_stub_path();
        std::fs::write(&path, "#!/bin/sh\ncat >/dev/null\nexit 0\n").ok()?;
        let mut perms = std::fs::metadata(&path).ok()?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).ok()?;
        Some(path)
    }

    /// Run `body` with a thread-local capturing subscriber, returning its
    /// result and everything logged at `WARN` or above.
    fn with_captured_warnings<T>(body: impl FnOnce() -> T) -> (T, String) {
        enable_event_capture();
        let capture = CaptureWriter::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(capture.clone())
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        let out = tracing::subscriber::with_default(subscriber, body);
        let logged = String::from_utf8(capture.0.lock().expect("capture lock").clone())
            .expect("the captured log must be utf-8");
        (out, logged)
    }

    /// A `git diff --stat`-shaped payload — the input #7377 was reported on.
    fn diff_stat_payload() -> String {
        let mut input = String::new();
        for i in 0..40 {
            input.push_str(&format!(" crates/x/src/file_{i}.rs | 4 ++--\n"));
        }
        input.push_str(" 40 files changed, 80 insertions(+), 80 deletions(-)\n");
        input
    }

    #[cfg(unix)]
    #[test]
    fn rtk_returning_nothing_falls_back_to_the_raw_output_and_warns_once() {
        // #7377: rtk exited zero having printed nothing and an 824-byte diff
        // was reported as a successful 100% reduction. The caller must get its
        // bytes back, and the substitution must be visible.
        let input = diff_stat_payload();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime");

        let ((compressed, path), logged) = with_captured_warnings(|| {
            runtime.block_on(compress_with_raw_fallback(empty_rtk, "git diff", &input))
        });
        let _ = std::fs::remove_file(empty_rtk_stub_path());

        assert_eq!(
            path.as_str(),
            "rtk_binary",
            "the stub must have been taken as the rtk arm, else this test proves \
             nothing about #7377's path: {logged}"
        );
        assert_eq!(
            compressed, input,
            "the caller must receive the original text byte-for-byte"
        );
        assert_eq!(
            logged.matches("falling back to the raw output").count(),
            1,
            "the fallback must be announced exactly once: {logged}"
        );
        assert!(
            logged.contains("rtk_binary"),
            "the warning must name the path that emptied the output: {logged}"
        );
    }

    #[tokio::test]
    async fn a_real_compression_is_not_treated_as_an_empty_result() {
        // The other half of the guard: a compression that shortened its input
        // must pass through untouched, with no warning and no fallback.
        let mut input = String::new();
        for i in 0..50 {
            input.push_str(&format!("test mod::t{i} ... ok\n"));
        }
        input.push_str("test result: ok. 50 passed; 0 failed\n");
        let (compressed, path) = compress_with_raw_fallback(no_rtk, "cargo test", &input).await;
        assert_eq!(path.as_str(), "native_fallback");
        assert!(
            compressed.len() < input.len(),
            "the guard must not have replaced a real compression"
        );
    }
}
