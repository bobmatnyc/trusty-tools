//! Human-table and JSON formatters for memory/code search results.
//!
//! Why: Both the interactive table and the `--json` machine format share the
//! same data; keeping the formatters in one file separates presentation from
//! the I/O handlers and CLI parsing.
//! What: `format_memory_results`, `format_sessions`, `format_session_list`,
//! `format_code_results`, plus the `preview_text` / `truncate_display` /
//! `format_timestamp` text helpers.
//! Test: `format_*` cases in `search_cmd::tests`.

use anyhow::{Context, Result};

use crate::memory::{AgentSession, MemoryResult, SessionMeta};
use crate::search::CodeChunk;

/// Format `MemoryResult`s as either an aligned table or a JSON array.
///
/// Why: The human table is the default for interactive use; `--json` is for
/// piping into other tools. Both paths share this function so the dispatch
/// stays boring.
/// What: JSON = `serde_json::to_string_pretty`. Human = header row +
/// `Agent | Phase | Timestamp | Score | Preview` for each hit. Preview is
/// the first 80 chars of the `response` payload field.
/// Test: `format_memory_results_human`, `format_memory_results_json`.
pub(super) fn format_memory_results(hits: &[MemoryResult], json: bool) -> Result<String> {
    if json {
        return serde_json::to_string_pretty(hits).context("failed to serialize memory hits");
    }
    let mut out = String::new();
    out.push_str(&format!(
        "{:<20} {:<12} {:<17} {:<7} {}\n",
        "Agent", "Phase", "Timestamp", "Score", "Preview"
    ));
    out.push_str(&"-".repeat(100));
    out.push('\n');
    for h in hits {
        let agent = h
            .payload
            .get("agent_name")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)");
        let phase = h
            .payload
            .get("phase")
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        let ts = h
            .payload
            .get("timestamp")
            .and_then(|v| v.as_str())
            .map(format_timestamp)
            .unwrap_or_else(|| "-".to_string());
        let response = h
            .payload
            .get("response")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let preview = preview_text(response, 80);
        out.push_str(&format!(
            "{:<20} {:<12} {:<17} {:<7.3} {}\n",
            truncate_display(agent, 20),
            truncate_display(phase, 12),
            ts,
            h.score,
            preview
        ));
    }
    Ok(out)
}

/// Format an ordered list of `AgentSession`s (from `memory run`) as text or JSON.
pub(super) fn format_sessions(sessions: &[AgentSession], json: bool) -> Result<String> {
    if json {
        return serde_json::to_string_pretty(sessions).context("failed to serialize sessions");
    }
    let mut out = String::new();
    for s in sessions {
        let ts = s.timestamp.format("%Y-%m-%d %H:%M").to_string();
        let preview = preview_text(&s.prompt, 80);
        out.push_str(&format!(
            "[{}] {} ({}): {}\n",
            ts, s.agent_name, s.phase, preview
        ));
    }
    Ok(out)
}

/// Format a list of `SessionMeta` entries as a table or JSON.
///
/// Why: `memory sessions` needs a compact listing for human readers and a
/// stable JSON shape for tooling.
/// What: JSON = `serde_json::to_string_pretty`. Human = header + rows of
/// `<run_id_prefix>  <started_at>  <task_preview>`.
/// Test: `format_session_list_human`.
pub(super) fn format_session_list(sessions: &[SessionMeta], json: bool) -> Result<String> {
    if json {
        return serde_json::to_string_pretty(sessions).context("failed to serialize sessions");
    }
    let mut out = String::new();
    out.push_str(&format!(
        "{:<10} {:<17} {}\n",
        "Run", "Started", "Task preview"
    ));
    out.push_str(&"-".repeat(80));
    out.push('\n');
    for s in sessions {
        let run_short: String = s.run_id.chars().take(8).collect();
        let ts = s.started_at.format("%Y-%m-%d %H:%M").to_string();
        let preview = preview_text(&s.task_preview, 50);
        out.push_str(&format!("{run_short:<10} {ts:<17} {preview}\n"));
    }
    Ok(out)
}

/// Format `CodeChunk`s as either an aligned table or a JSON array.
pub(super) fn format_code_results(chunks: &[CodeChunk], json: bool) -> Result<String> {
    if json {
        return serde_json::to_string_pretty(chunks).context("failed to serialize code chunks");
    }
    let mut out = String::new();
    out.push_str(&format!(
        "{:<40} {:<24} {:<10} {:<7} {}\n",
        "File:Line", "Function", "Lang", "Score", "Snippet"
    ));
    out.push_str(&"-".repeat(120));
    out.push('\n');
    for c in chunks {
        let fname = c
            .file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_else(|| c.file.to_str().unwrap_or("?"));
        let file_line = format!("{}:{}", fname, c.start_line);
        let func = c.function_name.as_deref().unwrap_or("-");
        let snippet = preview_text(&c.text, 80);
        out.push_str(&format!(
            "{:<40} {:<24} {:<10} {:<7.3} {}\n",
            truncate_display(&file_line, 40),
            truncate_display(func, 24),
            truncate_display(&c.language, 10),
            c.score,
            snippet
        ));
    }
    Ok(out)
}

/// First `max` chars of `s` with newlines collapsed to spaces.
pub(super) fn preview_text(s: &str, max: usize) -> String {
    let flat: String = s.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
    if flat.chars().count() <= max {
        flat
    } else {
        flat.chars().take(max).collect()
    }
}

/// Truncate a string for fixed-width display, appending `…` when cut.
fn truncate_display(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else if max == 0 {
        String::new()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}

/// Format an RFC3339 timestamp string as `YYYY-MM-DD HH:MM`.
fn format_timestamp(rfc3339: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(rfc3339) {
        Ok(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
        Err(_) => rfc3339.to_string(),
    }
}
