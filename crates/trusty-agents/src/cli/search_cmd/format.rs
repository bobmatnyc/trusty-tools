//! Code search result presentation. Test: `format_code_results_json`.
use crate::search::CodeChunk;
use anyhow::{Context, Result};
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
