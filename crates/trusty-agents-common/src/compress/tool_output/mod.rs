//! Per-tool output compression filters.
//!
//! Why: Verbose tool outputs (cargo test, git diff, git log, file reads) waste
//! tokens when re-injected into LLM conversation history. Stripping noise
//! preserves signal while shrinking context.
//! What: `compress_tool_output(name, output)` dispatches to a filter based on
//! the tool name, returning a possibly-shorter string. Each filter is a pure
//! `fn` for unit testability. `classify_tool` names that routing decision and
//! `has_filter_for` exposes it as a predicate, so a caller upstream of the
//! dispatch can tell whether a tool name reaches a filter at all (#6566).
//!
//! Module layout (see #366 split):
//! - `mod.rs` — classification, dispatch, and the native per-tool filters
//! - `structured.rs` — JSON/YAML/TOML/CSV passthrough detection
//! - `strategy.rs` — generic `FilterLevel`/`Language`/`FilterStrategy`
//! - `rtk.rs` — RTK subprocess delegation + async wrapper, plus
//!   `CompressionPath` (issue #1956 stats-logging signal)
//! - `tests.rs` — unit tests
//!
//! Hoisted from `trusty-agents::compress::tool_output` into
//! `trusty-agents-common` in issue #1959 so `trusty-mpm` can reach
//! `compress_tool_output_async` without a full `trusty-agents` path
//! dependency; `trusty-agents` re-exports this module's public surface from
//! `trusty_agents::compress` for source-level compatibility.
//!
//! Test: See `tests` — covers each filter and the dispatch table.

mod rtk;
mod strategy;
mod structured;

#[cfg(test)]
mod tests;

// Re-export the full public surface so callers can keep using
// `compress::tool_output::{...}` (and `compress::{compress_tool_output, ...}`).
pub use rtk::{
    CompressionPath, compress_tool_output_async, compress_tool_output_async_with_path,
    compress_via_rtk,
};
pub use strategy::{
    AggressiveFilter, FilterLevel, FilterStrategy, Language, MinimalFilter, NoFilter, get_filter,
};
pub use structured::is_structured_format;

/// Minimum byte length below which compression is skipped.
///
/// Why: Tiny outputs (status codes, short results) are already cheaper than
/// the cognitive cost of compression artifacts. RTK uses 80 bytes.
const SIZE_GATE_BYTES: usize = 80;

/// Line count above which `git log` output is worth filtering.
const GIT_LOG_LINE_GATE: usize = 30;
/// Line count above which a file read is worth filtering.
const FILE_READ_LINE_GATE: usize = 200;

/// Which native filter [`compress_tool_output`] applies to a given tool name.
///
/// Why: #6566 — callers upstream of the dispatch (the `tm hook` `PreToolUse`
/// Bash rewrite in `trusty-mpm`) need to know whether a tool name reaches a
/// filter at all, so they can skip wrapping a command whose output nothing
/// here would shrink. Naming the routing decision as a value lets
/// [`classify_tool`] answer that question and lets the dispatch match on it
/// exhaustively, so a new filter cannot appear in one without the other.
/// What: One variant per filter branch. `Grep` covers `grep`/`rg`/`find`;
/// `Ls` covers `ls`. Both delegate to the same line-list cap but stay
/// separate so each keeps its own named filter entry point.
/// Test: `classify_tool_*` and `has_filter_for_*` in `tests`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ToolFilter {
    /// `cargo test` and other test runners — [`filter_test_runner`].
    TestRunner,
    /// `cargo check` / `cargo clippy` — [`filter_cargo_check`].
    CargoCheck,
    /// Unified diffs — [`filter_git_diff`].
    GitDiff,
    /// `git log` — [`filter_git_log`], above [`GIT_LOG_LINE_GATE`] lines.
    GitLog,
    /// File reads — [`filter_file_read`], above [`FILE_READ_LINE_GATE`] lines.
    FileRead,
    /// `grep`/`rg`/`find` match lists — [`filter_grep_output`].
    Grep,
    /// `ls` directory listings — [`filter_ls_output`].
    Ls,
}

/// Route a tool name to the filter [`compress_tool_output`] would apply.
///
/// Why: The single source of truth for "which filter, if any, covers this
/// tool name". [`compress_tool_output`] dispatches on its result and
/// [`has_filter_for`] tests it, so the coverage question has exactly one
/// answer (#6566, and the common-entry-point rule).
/// What: Substring match against the lowercased name, in the order the
/// dispatch has always used — the `test`/`cargo` family first (with
/// `check`/`clippy` inside it taking the cargo-check filter), then `diff`,
/// `log`, `read`/`cat`. `grep`/`rg`/`find`/`ls` match on the FIRST
/// whitespace token rather than a substring, because `rg` is a substring of
/// unrelated names this sees (`cargo`, `git merge`) that must not misfire
/// (#1957). `None` means no filter covers the name.
/// Test: `classify_tool_routes_known_tool_families`,
/// `classify_tool_returns_none_for_uncovered_tools`.
pub fn classify_tool(tool_name: &str) -> Option<ToolFilter> {
    let n = tool_name.to_ascii_lowercase();
    if n.contains("test") || n.contains("cargo") {
        // Note: "cargo check"/"cargo clippy" go to the cargo_check filter
        // which strips Compiling/Finished lines; "cargo test" goes here.
        if n.contains("check") || n.contains("clippy") {
            return Some(ToolFilter::CargoCheck);
        }
        return Some(ToolFilter::TestRunner);
    }
    if n.contains("diff") {
        return Some(ToolFilter::GitDiff);
    }
    if n.contains("log") {
        return Some(ToolFilter::GitLog);
    }
    if n.contains("read") || n.contains("cat") {
        return Some(ToolFilter::FileRead);
    }
    // #1957: grep/rg/find emit a flat match-or-path list; ls emits a flat
    // directory listing. Matched on the first whitespace token (not
    // `.contains()`, unlike the branches above) — see this function's doc.
    let first_word = n.split_whitespace().next().unwrap_or("");
    if first_word == "grep" || first_word == "rg" || first_word == "find" {
        return Some(ToolFilter::Grep);
    }
    if first_word == "ls" {
        return Some(ToolFilter::Ls);
    }
    if n.contains("check") || n.contains("clippy") {
        return Some(ToolFilter::CargoCheck);
    }
    None
}

/// Whether any native filter covers `tool_name`.
///
/// Why: #6566 — `tm hook`'s `PreToolUse` rewrite appended
/// `| tm compress --tool "<name>"` to every eligible Bash command, but only
/// the handful of names [`classify_tool`] recognises reach a filter. Measured
/// over 48h of `~/.trusty-mpm/compression.jsonl`, 3,415 of 3,643 wrapped
/// invocations (93.7%) returned their input byte-for-byte: each one paid a
/// process spawn to change nothing. This predicate lets the rewrite decide
/// before it wraps.
/// What: `true` exactly when [`classify_tool`] returns a variant. It answers
/// the NAME question only — the size gate ([`SIZE_GATE_BYTES`]), the
/// structured-format passthrough, and the per-filter line gates all depend on
/// the output, which a pre-execution caller cannot see, so a `true` here
/// promises a filter branch is reached, not that bytes will drop.
/// Note the RTK path is out of scope: `compress_tool_output_async` prefers
/// the external `rtk` binary when installed, which can shrink names this
/// returns `false` for. A hook gating on this under-wraps in that setup,
/// which is the safe direction — see #6566.
/// Test: `has_filter_for_true_for_covered_tools`,
/// `has_filter_for_false_for_uncovered_tools`,
/// `has_filter_for_agrees_with_classify_tool`.
pub fn has_filter_for(tool_name: &str) -> bool {
    classify_tool(tool_name).is_some()
}

/// Compress a tool's textual output based on its name.
///
/// Why: Centralizes the per-tool filter dispatch so callers don't have to
/// know which filter applies to which tool.
/// What: Applies a size gate and a structured-format passthrough, then routes
/// on [`classify_tool`]. The match is exhaustive over [`ToolFilter`] with no
/// catch-all arm, so adding a filter variant fails to compile until this
/// dispatch handles it — that is what keeps [`has_filter_for`] from drifting
/// away from what the dispatch actually filters (#6566). Unknown tools pass
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
    match classify_tool(tool_name) {
        None => output.to_string(),
        Some(ToolFilter::TestRunner) => filter_test_runner(output),
        Some(ToolFilter::CargoCheck) => filter_cargo_check(output),
        Some(ToolFilter::GitDiff) => filter_git_diff(output),
        Some(ToolFilter::GitLog) => {
            if output.lines().count() > GIT_LOG_LINE_GATE {
                filter_git_log(output)
            } else {
                output.to_string()
            }
        }
        Some(ToolFilter::FileRead) => {
            if output.lines().count() > FILE_READ_LINE_GATE {
                filter_file_read(output)
            } else {
                output.to_string()
            }
        }
        Some(ToolFilter::Grep) => filter_grep_output(output),
        Some(ToolFilter::Ls) => filter_ls_output(output),
    }
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

/// Line count above which [`cap_line_list`] head/tail-caps its input.
const LINE_LIST_CAP: usize = 60;
/// Leading lines kept when [`cap_line_list`] triggers.
const LINE_LIST_HEAD: usize = 25;
/// Trailing lines kept when [`cap_line_list`] triggers.
const LINE_LIST_TAIL: usize = 10;

/// Head/tail-cap a flat, one-entry-per-line output, announcing the drop.
///
/// Why: `grep`/`rg`/`find`/`ls` output has no natural summary line the way
/// `cargo test`'s `test result:` or a diff's hunk header does, so there is
/// nothing to fall back to — the cap must state what it removed rather than
/// dropping it silently (compression that silently drops content is this
/// project's recurring defect; see #1957's PR discussion).
/// What: Passes output through unchanged at or under `LINE_LIST_CAP` lines.
/// Above that, keeps the first `LINE_LIST_HEAD` lines, inserts a
/// `... N lines omitted ...` marker giving the exact count of dropped
/// lines, then the last `LINE_LIST_TAIL` lines.
/// Test: `filter_grep_output_caps_long_match_list`,
/// `filter_ls_output_caps_long_listing`.
fn cap_line_list(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= LINE_LIST_CAP {
        return output.to_string();
    }
    let omitted = lines.len() - LINE_LIST_HEAD - LINE_LIST_TAIL;
    let mut out: Vec<String> = Vec::with_capacity(LINE_LIST_HEAD + LINE_LIST_TAIL + 1);
    out.extend(lines[..LINE_LIST_HEAD].iter().map(|s| (*s).to_string()));
    out.push(format!("... {omitted} lines omitted ..."));
    out.extend(
        lines[lines.len() - LINE_LIST_TAIL..]
            .iter()
            .map(|s| (*s).to_string()),
    );
    out.join("\n")
}

/// Cap long `grep`/`rg`/`find` match-or-path lists.
///
/// Why: These tools emit one match/path per line with no built-in summary —
/// the #1953 spike found grep output passed through `compress_tool_output`
/// with 0% reduction because no filter branch existed for it (#1957).
/// What: Delegates to [`cap_line_list`] — passthrough at/under
/// `LINE_LIST_CAP` lines, head/tail-capped with an explicit omission count
/// above it.
/// Test: `filter_grep_output_caps_long_match_list`,
/// `filter_grep_output_passthrough_short`.
pub fn filter_grep_output(output: &str) -> String {
    cap_line_list(output)
}

/// Cap long `ls` directory listings.
///
/// Why: Same coverage gap as [`filter_grep_output`] for `ls -la` output —
/// the #1953 spike measured 0% reduction because no filter branch existed
/// for it (#1957).
/// What: Delegates to [`cap_line_list`].
/// Test: `filter_ls_output_caps_long_listing`, `filter_ls_output_passthrough_short`.
pub fn filter_ls_output(output: &str) -> String {
    cap_line_list(output)
}
