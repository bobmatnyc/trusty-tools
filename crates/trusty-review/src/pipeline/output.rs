//! Review result output — log file writing and STDOUT rendering.
//!
//! Why: the MVP pipeline is dry-run only (no GitHub comment posting); the
//! output step writes a JSON log file and prints a human-readable summary to
//! STDOUT so operators can inspect results immediately.
//!
//! What: exposes `write_review_log` (writes JSON + Markdown to `LOG_DIR`)
//! and `print_review_result` (formats a review to STDOUT).
//!
//! Test: `write_review_log_creates_json_file`, `print_review_result_includes_verdict`.

use std::{
    io::Write as _,
    path::{Path, PathBuf},
};

use tracing::warn;

use crate::models::ReviewResult;

// ─── Log writing ──────────────────────────────────────────────────────────────

/// Write the `ReviewResult` as a JSON log file and a Markdown summary.
///
/// Why: the pipeline needs a persistent, human-readable record of every review
/// for post-hoc calibration and debugging.
/// What: creates `<log_dir>/<owner>-<repo>-pr<number>-<timestamp>.json` and
/// a `.md` companion.  Both are written atomically (write-then-rename idiom)
/// so partial writes don't corrupt the log.  Errors are logged as warnings;
/// they do not fail the overall pipeline.
/// Test: `write_review_log_creates_json_file`.
pub fn write_review_log(result: &ReviewResult, log_dir: &Path) {
    if let Err(e) = std::fs::create_dir_all(log_dir) {
        warn!(
            dir = %log_dir.display(),
            "failed to create log directory: {e}"
        );
        return;
    }

    let stem = log_stem(result);
    let json_path = log_dir.join(format!("{stem}.json"));
    let md_path = log_dir.join(format!("{stem}.md"));

    // Write JSON.
    match serde_json::to_string_pretty(result) {
        Ok(json) => {
            if let Err(e) = atomic_write(&json_path, json.as_bytes()) {
                warn!(path = %json_path.display(), "failed to write review log JSON: {e}");
            }
        }
        Err(e) => {
            warn!("failed to serialise ReviewResult to JSON: {e}");
        }
    }

    // Write Markdown summary.
    let md = render_markdown_summary(result);
    if let Err(e) = atomic_write(&md_path, md.as_bytes()) {
        warn!(path = %md_path.display(), "failed to write review log Markdown: {e}");
    }
}

/// Build the log file stem for this review.
///
/// Why: we want a deterministic, sortable filename for each review.
/// What: `<owner>-<repo>-pr<number>-<timestamp>` using hyphens and no slashes.
/// Test: `log_stem_format`.
fn log_stem(result: &ReviewResult) -> String {
    let ts = result
        .timestamp
        .replace([':', 'T'], "-")
        .trim_end_matches('Z')
        .to_string();
    format!(
        "{owner}-{repo}-pr{pr}-{ts}",
        owner = sanitize_path(&result.owner),
        repo = sanitize_path(&result.repo),
        pr = result.pr_number,
        ts = ts,
    )
}

/// Replace characters that are unsafe in file names.
fn sanitize_path(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Write `data` to `path` atomically via a temp file in the same directory.
///
/// Why: partial writes corrupt log files; atomic rename prevents that.
/// What: writes to `<path>.tmp`, then renames to the final path.
/// Test: covered transitively by `write_review_log_creates_json_file`.
fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp_path = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp_path)?;
        f.write_all(data)?;
        f.flush()?;
    }
    std::fs::rename(&tmp_path, path)
}

// ─── Markdown rendering ───────────────────────────────────────────────────────

/// Render a human-readable Markdown summary of the review result.
///
/// Why: the Markdown companion makes log files readable in GitHub/Notion
/// without parsing JSON.
/// What: formats verdict, summary, model telemetry, and findings as Markdown.
/// Test: `render_markdown_summary_includes_verdict`.
pub fn render_markdown_summary(result: &ReviewResult) -> String {
    let mut md = String::with_capacity(1024);
    md.push_str(&format!(
        "# trusty-review: {owner}/{repo} PR #{pr}\n\n",
        owner = result.owner,
        repo = result.repo,
        pr = result.pr_number,
    ));
    md.push_str(&format!("**Title:** {}\n\n", result.pr_title));
    md.push_str(&format!(
        "**Verdict:** `{}` | **Model:** `{}`\n\n",
        result.verdict, result.model
    ));
    if !result.review_body.is_empty() {
        // Show first ~300 chars of the review body as a snippet.
        let snippet: String = result.review_body.chars().take(300).collect();
        let ellipsis = if result.review_body.len() > 300 {
            "…"
        } else {
            ""
        };
        md.push_str(&format!("**Summary excerpt:** {snippet}{ellipsis}\n\n"));
    }
    md.push_str(&format!(
        "**Telemetry:** input={} tokens, output={} tokens, cost=${:.6}, latency={}ms\n\n",
        result.input_tokens, result.output_tokens, result.cost_estimate_usd, result.latency_ms
    ));

    if !result.findings.is_empty() {
        md.push_str("## Findings\n\n");
        for (i, f) in result.findings.iter().enumerate() {
            md.push_str(&format!(
                "{}. **{}** (`{}`) — confidence={:.0}%\n   {}\n\n",
                i + 1,
                f.kind,
                f.file,
                f.confidence * 100.0,
                f.description,
            ));
        }
    } else {
        md.push_str("_No findings._\n\n");
    }

    if let Some(ref err) = result.error {
        md.push_str(&format!("**Pipeline error:** {err}\n\n"));
    }

    if result.dry_run {
        md.push_str("_Dry run — no comment posted to GitHub._\n");
    }

    md
}

// ─── STDOUT rendering ─────────────────────────────────────────────────────────

/// Print a formatted review result to STDOUT.
///
/// Why: the CLI `run` command needs to display the review result to the user
/// in a readable format without tracing noise.
/// What: prints verdict, summary, findings table, and telemetry.  All tracing
/// goes to stderr; STDOUT receives only the structured output.
/// Test: `print_review_result_includes_verdict`.
pub fn print_review_result(result: &ReviewResult) {
    println!(
        "\n=== trusty-review: {}/{} PR #{} ===\n",
        result.owner, result.repo, result.pr_number
    );
    println!("Title:   {}", result.pr_title);
    println!("Verdict: {}", result.verdict);
    println!("Model:   {}", result.model);
    println!(
        "Tokens:  {} in / {} out | cost: ${:.6} | latency: {}ms",
        result.input_tokens, result.output_tokens, result.cost_estimate_usd, result.latency_ms
    );
    if !result.review_body.is_empty() {
        let snippet: String = result.review_body.chars().take(400).collect();
        let ellipsis = if result.review_body.len() > 400 {
            "…"
        } else {
            ""
        };
        println!("\nSummary:\n{snippet}{ellipsis}");
    }
    if result.findings.is_empty() {
        println!("\nFindings: none");
    } else {
        println!("\nFindings ({})", result.findings.len());
        for (i, f) in result.findings.iter().enumerate() {
            println!(
                "  {}. [{}] {} — {} (confidence {:.0}%)",
                i + 1,
                f.kind,
                f.file,
                f.description.chars().take(80).collect::<String>(),
                f.confidence * 100.0
            );
        }
    }
    if let Some(ref err) = result.error {
        println!("\nPipeline error: {err}");
    }
    if result.dry_run {
        println!("\n(dry run — not posted to GitHub)");
    }
}

/// Return the path where the log was written, for display purposes.
///
/// Why: the CLI prints the log path after writing so the user knows where
/// to find the raw JSON.
/// What: reconstructs the JSON path from the log_dir and result fields.
/// Test: covered transitively.
pub fn log_json_path(result: &ReviewResult, log_dir: &Path) -> PathBuf {
    log_dir.join(format!("{}.json", log_stem(result)))
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Effort, Finding, Verdict};

    fn sample_result() -> ReviewResult {
        let mut r = ReviewResult::new(
            "acme",
            "backend",
            42,
            "Add feature X",
            "https://github.com/acme/backend/pull/42",
        );
        r.verdict = Verdict::RequestChanges;
        r.model = "openai/gpt-5.4-mini-20260317".to_string();
        r.review_body = "This PR has a SQL injection risk.".to_string();
        r.input_tokens = 1000;
        r.output_tokens = 200;
        r.cost_estimate_usd = 0.001575;
        r.latency_ms = 1234;
        r.findings.push(Finding::new(
            "src/main.rs",
            "security",
            "SQL injection risk",
            "Use parameterised query",
            0.92,
            Effort::Medium,
        ));
        r
    }

    #[test]
    fn log_stem_format() {
        let result = sample_result();
        let stem = log_stem(&result);
        assert!(stem.starts_with("acme-backend-pr42-"), "stem: {stem}");
        assert!(!stem.contains('/'), "stem must not contain slashes");
    }

    #[test]
    fn render_markdown_summary_includes_verdict() {
        let result = sample_result();
        let md = render_markdown_summary(&result);
        assert!(
            md.contains("REQUEST_CHANGES"),
            "markdown must include verdict"
        );
        assert!(
            md.contains("acme/backend"),
            "markdown must include owner/repo"
        );
        assert!(
            md.contains("SQL injection risk"),
            "markdown must include finding"
        );
        assert!(md.contains("gpt-5.4-mini"), "markdown must include model");
    }

    #[test]
    fn render_markdown_summary_empty_findings() {
        let mut result = sample_result();
        result.findings.clear();
        let md = render_markdown_summary(&result);
        assert!(
            md.contains("No findings"),
            "empty findings must note absence"
        );
    }

    #[test]
    fn render_markdown_includes_error_field() {
        let mut result = sample_result();
        result.error = Some("LLM timeout".to_string());
        let md = render_markdown_summary(&result);
        assert!(
            md.contains("LLM timeout"),
            "error field must appear in markdown"
        );
    }

    #[test]
    fn write_review_log_creates_json_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let result = sample_result();
        write_review_log(&result, dir.path());

        // Find the written JSON file.
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
            .collect();
        assert_eq!(entries.len(), 1, "exactly one JSON file should be written");

        let content = std::fs::read_to_string(entries[0].path()).expect("read log");
        let back: ReviewResult = serde_json::from_str(&content).expect("deserialise log");
        assert_eq!(back.owner, "acme");
        assert_eq!(back.verdict, Verdict::RequestChanges);
    }

    #[test]
    fn write_review_log_creates_markdown_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let result = sample_result();
        write_review_log(&result, dir.path());

        let md_entries: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "md").unwrap_or(false))
            .collect();
        assert_eq!(
            md_entries.len(),
            1,
            "exactly one Markdown file should be written"
        );
    }

    #[test]
    fn write_review_log_invalid_dir_does_not_panic() {
        // A path that cannot be created (e.g., file-as-dir) must not panic.
        // We use a path inside a non-existent file to trigger the error path.
        let result = sample_result();
        // This will fail to create the dir but must not panic.
        write_review_log(&result, Path::new("/dev/null/impossible/path"));
    }

    #[test]
    fn log_json_path_correct_extension() {
        let result = sample_result();
        let dir = Path::new("/tmp/trusty-review");
        let path = log_json_path(&result, dir);
        assert!(
            path.extension().map(|x| x == "json").unwrap_or(false),
            "log path must have .json extension: {path:?}"
        );
    }
}
