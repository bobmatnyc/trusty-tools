//! Per-tool output compression filters.
//!
//! Why: Verbose tool outputs (cargo test, git diff, git log, file reads) waste
//! tokens when re-injected into LLM conversation history. Stripping noise
//! preserves signal while shrinking context.
//! What: `compress_tool_output(name, output)` dispatches to a filter based on
//! the tool name, returning a possibly-shorter string. Each filter is a pure
//! `fn` for unit testability.
//!
//! Module layout (see #366 split):
//! - `mod.rs` — dispatch + the native per-tool filters
//! - `structured.rs` — JSON/YAML/TOML/CSV passthrough detection
//! - `strategy.rs` — generic `FilterLevel`/`Language`/`FilterStrategy`
//! - `rtk.rs` — RTK subprocess delegation + async wrapper
//! - `tests.rs` — unit tests
//!
//! Test: See `tests` — covers each filter and the dispatch table.

mod rtk;
mod strategy;
mod structured;

#[cfg(test)]
mod tests;

// Re-export the full public surface so callers can keep using
// `compress::tool_output::{...}` (and `compress::{compress_tool_output, ...}`).
pub use rtk::{compress_tool_output_async, compress_via_rtk};
pub use strategy::{
    AggressiveFilter, FilterLevel, FilterStrategy, Language, MinimalFilter, NoFilter, get_filter,
};
pub use structured::is_structured_format;

/// Minimum byte length below which compression is skipped.
///
/// Why: Tiny outputs (status codes, short results) are already cheaper than
/// the cognitive cost of compression artifacts. RTK uses 80 bytes.
const SIZE_GATE_BYTES: usize = 80;

/// Compress a tool's textual output based on its name.
///
/// Why: Centralizes the per-tool filter dispatch so callers don't have to
/// know which filter applies to which tool.
/// What: Routes by substring match in `tool_name`. Applies a size gate and a
/// structured-format passthrough before any filter runs. Unknown tools pass
/// through unchanged. Always infallible.
/// Test: `compress_tool_output_dispatch_test` plus per-filter tests.
pub fn compress_tool_output(tool_name: &str, output: &str) -> String {
    // Size gate: very small outputs aren't worth touching.
    if output.len() < SIZE_GATE_BYTES {
        return output.to_string();
    }
    // Structured formats (JSON/YAML/TOML/CSV) must pass through unchanged so
    // we don't corrupt machine-parseable payloads.
    if is_structured_format(output) {
        return output.to_string();
    }
    let n = tool_name.to_ascii_lowercase();
    if n.contains("test") || n.contains("cargo") {
        // Note: "cargo check"/"cargo clippy" go to the cargo_check filter
        // which strips Compiling/Finished lines; "cargo test" goes here.
        if n.contains("check") || n.contains("clippy") {
            return filter_cargo_check(output);
        }
        return filter_test_runner(output);
    }
    if n.contains("diff") {
        return filter_git_diff(output);
    }
    if n.contains("log") {
        let line_count = output.lines().count();
        if line_count > 30 {
            return filter_git_log(output);
        }
        return output.to_string();
    }
    if n.contains("read") || n.contains("cat") {
        let line_count = output.lines().count();
        if line_count > 200 {
            return filter_file_read(output);
        }
        return output.to_string();
    }
    if n.contains("check") || n.contains("clippy") {
        return filter_cargo_check(output);
    }
    output.to_string()
}

/// Strip passing test lines from `cargo test` output.
///
/// Why: Hundreds of `test foo ... ok` lines drown out the few failures the
/// model needs to see.
/// What: Drops lines matching `test <name> ... ok`. Keeps `FAILED`, `error`,
/// `warning`, and the final `test result:` summary. Returns summary-only
/// when nothing else remains.
/// Test: `test_runner_strips_passing_tests`, `test_runner_keeps_summary_line`,
/// `test_runner_no_failures_returns_summary_only`.
pub fn filter_test_runner(output: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut summary: Option<&str> = None;
    for line in output.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("test result:") {
            summary = Some(line);
            continue;
        }
        if is_passing_test_line(trimmed) {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if trimmed.contains("FAILED")
            || lower.contains("error")
            || lower.contains("warning")
            || trimmed.starts_with("---- ")
            || trimmed.starts_with("failures:")
        {
            kept.push(line);
        }
    }

    if kept.is_empty() {
        return summary.map(|s| s.to_string()).unwrap_or_default();
    }
    let mut out = kept.join("\n");
    if let Some(s) = summary {
        out.push('\n');
        out.push_str(s);
    }
    out
}

fn is_passing_test_line(s: &str) -> bool {
    // Match `test <something> ... ok` (cargo's per-test line)
    if !s.starts_with("test ") {
        return false;
    }
    s.ends_with(" ... ok") || s.ends_with(" ... ignored")
}

/// Collapse runs of context lines in a unified diff.
///
/// Why: Diff context (lines starting with a space) is rarely needed by the
/// model; the +/- lines carry the real signal.
/// What: Replaces every run of context lines with a single
/// `@@ ... @@ [+N added, -N removed]` summary header reflecting counts of
/// the surrounding hunk. Preserves `---`, `+++`, `@@`, `+`, `-` lines.
/// Test: `filter_git_diff_strips_context_lines`,
/// `filter_git_diff_preserves_adds_and_removes`,
/// `filter_git_diff_passthrough_no_context`.
pub fn filter_git_diff(output: &str) -> String {
    // First pass: count adds/removes per hunk so we can annotate replacement headers.
    let lines: Vec<&str> = output.lines().collect();
    let mut hunk_stats: Vec<(usize, usize)> = Vec::new(); // (added, removed) per hunk
    let mut cur_add = 0usize;
    let mut cur_rem = 0usize;
    let mut in_hunk = false;
    for line in &lines {
        if line.starts_with("@@") {
            if in_hunk {
                hunk_stats.push((cur_add, cur_rem));
            }
            cur_add = 0;
            cur_rem = 0;
            in_hunk = true;
            continue;
        }
        if !in_hunk {
            continue;
        }
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if line.starts_with('+') {
            cur_add += 1;
        } else if line.starts_with('-') {
            cur_rem += 1;
        }
    }
    if in_hunk {
        hunk_stats.push((cur_add, cur_rem));
    }

    // Second pass: emit, collapsing context runs.
    let mut out: Vec<String> = Vec::new();
    let hunk_idx: usize = 0;
    let mut in_context_run = false;
    let mut had_any_context = false;
    for line in &lines {
        if line.starts_with("@@") || line.starts_with("---") || line.starts_with("+++") {
            in_context_run = false;
            out.push((*line).to_string());
            continue;
        }
        if line.starts_with('+') || line.starts_with('-') {
            in_context_run = false;
            out.push((*line).to_string());
            continue;
        }
        // Treat any other line (including " ..." context, blank) as context.
        had_any_context = true;
        if !in_context_run {
            in_context_run = true;
            // Emit a collapsed-context marker referencing the current hunk's totals.
            let (a, r) = hunk_stats
                .get(hunk_idx.saturating_sub(0))
                .copied()
                .unwrap_or((0, 0));
            // Hunk index advances when we encounter an `@@` header; for context
            // lines belonging to the current hunk we reuse the current totals.
            let _ = hunk_idx; // explicit no-op to avoid unused warnings if logic shifts.
            out.push(format!("@@ ... @@ [+{a} added, -{r} removed]"));
        }
        // Drop the context line itself.
        let _ = line;
    }

    if !had_any_context {
        // Passthrough: nothing to compress.
        return output.to_string();
    }
    out.join("\n")
}

/// Strip author/date/body lines from `git log` output, keeping commit headers.
///
/// Why: Long logs are mostly metadata; the SHA + subject is enough for
/// most LLM reasoning.
/// What: Keeps lines matching `commit <7+ hex chars>` (and the very next
/// non-blank non-metadata line as the subject). Drops Author:, Date:,
/// Merge:, and indented body lines.
/// Test: `filter_git_log_strips_author_date`, `filter_git_log_passthrough_short`.
pub fn filter_git_log(output: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut expect_subject = false;
    for line in output.lines() {
        if is_commit_header(line) {
            out.push(line.to_string());
            expect_subject = true;
            continue;
        }
        if expect_subject {
            // Subject is the first indented non-metadata line after author/date.
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.starts_with("Author:")
                || trimmed.starts_with("Date:")
                || trimmed.starts_with("Merge:")
                || trimmed.starts_with("commit ")
            {
                continue;
            }
            // First real content line — treat as subject.
            out.push(line.to_string());
            expect_subject = false;
        }
    }
    out.join("\n")
}

fn is_commit_header(line: &str) -> bool {
    // `commit <hash>` where hash is 7+ hex chars.
    let stripped = match line.strip_prefix("commit ") {
        Some(s) => s.trim(),
        None => return false,
    };
    let hex_len = stripped
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .count();
    hex_len >= 7
}

/// Strip blank and comment-only lines from large file reads.
///
/// Why: When the model loads a 500-line file, blanks and bare comments are
/// often non-essential.
/// What: Drops lines that are blank, or trim-start with `//` or `#`. If the
/// result is < 20 lines (over-filtered, perhaps an all-comment file), returns
/// the original to avoid hiding too much.
/// Test: `filter_file_read_strips_blank_comment_lines`,
/// `filter_file_read_no_over_filter`.
pub fn filter_file_read(output: &str) -> String {
    let kept: Vec<&str> = output
        .lines()
        .filter(|line| {
            let t = line.trim_start();
            if t.is_empty() {
                return false;
            }
            if t.starts_with("//") {
                return false;
            }
            if t.starts_with('#') {
                return false;
            }
            true
        })
        .collect();
    if kept.len() < 20 {
        return output.to_string();
    }
    kept.join("\n")
}

/// Strip `Compiling` and `Finished` chatter from `cargo check`/`cargo clippy`.
///
/// Why: These lines are progress noise; warnings/errors are the signal.
/// What: Drops lines beginning with `   Compiling ` or `    Finished `.
/// Keeps everything else verbatim.
/// Test: `filter_cargo_check_strips_compiling`, `filter_cargo_check_keeps_warnings`.
pub fn filter_cargo_check(output: &str) -> String {
    output
        .lines()
        .filter(|line| !line.starts_with("   Compiling ") && !line.starts_with("    Finished "))
        .collect::<Vec<_>>()
        .join("\n")
}
