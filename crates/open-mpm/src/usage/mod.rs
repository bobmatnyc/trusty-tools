//! Per-dispatch usage logging (#281).
//!
//! Why: open-mpm dispatches LLM calls through several paths (claude CLI,
//! Anthropic direct, OpenRouter, Bedrock) and currently has no structured,
//! easy-to-grep record of which model handled which call, how many tokens
//! were spent, or how long the round-trip took. Inspired by Kilo.ai's
//! cost-transparency UX, this module appends a single JSONL line per
//! dispatch to `.open-mpm/state/usage.jsonl` so operators can audit cost
//! and routing decisions after the fact.
//! What: Defines the `UsageRecord` struct and an async `append_usage`
//! helper. Best-effort: any I/O or serialization failure is logged at debug
//! level and swallowed so usage logging can never block a real dispatch.
//! Test: see `tests` module — round-trip serialization, task_prefix
//! truncation, file create + append semantics.

pub mod daily;

use serde::Serialize;
use std::path::Path;
use tokio::io::AsyncWriteExt;

/// One row of the per-dispatch usage log.
///
/// Why: A flat, all-string-or-numeric shape keeps the JSONL trivially
/// grep-able and trivially loadable into pandas / DuckDB / jq.
/// What: ts (RFC3339), agent name, model, runner tag, token counts,
/// duration, and a 60-char task prefix.
/// Test: `usage_record_serializes_to_valid_jsonl`.
#[derive(Debug, Clone, Serialize)]
pub struct UsageRecord {
    /// RFC3339 timestamp in UTC of when the dispatch *completed*.
    pub ts: String,
    /// Agent name from TOML config — `"ctrl"` for direct ctrl turns,
    /// `"pm"` for the orchestrator, etc. Never empty in practice; we
    /// fall back to `"unknown"` at call sites that lack the name.
    pub agent: String,
    /// Model id as actually dispatched (e.g. `anthropic/claude-sonnet-4-6`).
    pub model: String,
    /// One of `"claude-code" | "anthropic-direct" | "openrouter" | "bedrock"`.
    pub runner: String,
    /// Prompt / input tokens reported by the provider. `0` when unavailable
    /// (e.g. older `claude` CLI versions that omit the `usage` block).
    pub input_tokens: u32,
    /// Completion / output tokens. `0` when unavailable.
    pub output_tokens: u32,
    /// Wall-clock milliseconds for the LLM call.
    pub duration_ms: u64,
    /// First 60 chars of the task string. Strictly for human readability
    /// when tailing the log; not load-bearing for any tooling.
    pub task_prefix: String,
}

impl UsageRecord {
    /// Build a `UsageRecord` with the timestamp set to "now" and `task_prefix`
    /// truncated at 60 *characters* (not bytes — we collect from `chars()`
    /// so multi-byte UTF-8 doesn't split a code point).
    ///
    /// Why: Centralizes the trimming + timestamp logic so every call site
    /// gets it right.
    /// What: Returns the record with `chrono::Utc::now().to_rfc3339()` as
    /// `ts` and `task.chars().take(60).collect()` as `task_prefix`.
    /// Test: `task_prefix_truncates_at_60`.
    pub fn new(
        agent: impl Into<String>,
        model: impl Into<String>,
        runner: impl Into<String>,
        input_tokens: u32,
        output_tokens: u32,
        duration_ms: u64,
        task: &str,
    ) -> Self {
        Self {
            ts: chrono::Utc::now().to_rfc3339(),
            agent: agent.into(),
            model: model.into(),
            runner: runner.into(),
            input_tokens,
            output_tokens,
            duration_ms,
            task_prefix: task.chars().take(60).collect(),
        }
    }
}

/// Append a `UsageRecord` as a single JSONL line to
/// `<project_dir>/.open-mpm/state/usage.jsonl`.
///
/// Why: Append-only JSONL is the simplest durable format that survives
/// concurrent writes from multi-agent runs (each line is atomic on POSIX
/// when written in one syscall, which a single short JSON line is). We
/// never propagate errors: usage logging is observability, not control
/// flow, and a full disk should not break a real LLM dispatch.
/// What: Best-effort `mkdir -p` of `.open-mpm/state`, then opens the file
/// in `create + append` mode and writes one `serde_json::to_string(&record)`
/// line followed by `\n`, then flushes to guarantee the bytes are visible
/// to subsequent readers. Any I/O failure logs at debug level and returns.
/// Test: `append_usage_creates_file`, `append_usage_appends`.
pub async fn append_usage(project_dir: &Path, record: &UsageRecord) {
    let state_dir = project_dir.join(".open-mpm").join("state");
    let path = state_dir.join("usage.jsonl");
    let _ = tokio::fs::create_dir_all(&state_dir).await;
    let line = match serde_json::to_string(record) {
        Ok(s) => format!("{}\n", s),
        Err(e) => {
            tracing::debug!(error = %e, "usage: serialize failed");
            return;
        }
    };
    match tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await
    {
        Ok(mut f) => {
            if let Err(e) = f.write_all(line.as_bytes()).await {
                tracing::debug!(error = %e, path = %path.display(), "usage: write failed");
                return;
            }
            // Flush ensures the write is visible to subsequent reads within
            // the same process. Tokio's File uses spawn_blocking internally;
            // without an explicit flush the OS buffer may not be committed
            // before the future resolves, causing test races.
            if let Err(e) = f.flush().await {
                tracing::debug!(error = %e, path = %path.display(), "usage: flush failed");
            }
        }
        Err(e) => {
            tracing::debug!(error = %e, path = %path.display(), "usage: open failed");
        }
    }
}

/// Resolve the project directory used to root the usage log.
///
/// Why: The usage log lives under `.open-mpm/state/`, which is per-project.
/// Most call sites don't carry a project dir; the harness already trusts
/// `OPEN_MPM_PROJECT_DIR` (and falls back to `current_dir()`) for similar
/// per-project paths, so we follow the same convention.
/// What: Returns `OPEN_MPM_PROJECT_DIR` if set, else `std::env::current_dir()`,
/// else the empty path (which `append_usage` will treat as cwd).
/// Test: Indirectly via `append_usage_*` tests passing an explicit tempdir.
pub fn project_dir() -> std::path::PathBuf {
    if let Ok(d) = std::env::var("OPEN_MPM_PROJECT_DIR")
        && !d.is_empty()
    {
        return std::path::PathBuf::from(d);
    }
    std::env::current_dir().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_record_serializes_to_valid_jsonl() {
        let r = UsageRecord {
            ts: "2026-05-01T19:00:00Z".to_string(),
            agent: "engineer".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            runner: "openrouter".to_string(),
            input_tokens: 1420,
            output_tokens: 312,
            duration_ms: 4800,
            task_prefix: "fix credential routing".to_string(),
        };
        let json = serde_json::to_string(&r).expect("serialize");
        // Must be a single line (no embedded newline) — JSONL invariant.
        assert!(!json.contains('\n'));
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["ts"], "2026-05-01T19:00:00Z");
        assert_eq!(parsed["agent"], "engineer");
        assert_eq!(parsed["model"], "claude-sonnet-4-6");
        assert_eq!(parsed["runner"], "openrouter");
        assert_eq!(parsed["input_tokens"], 1420);
        assert_eq!(parsed["output_tokens"], 312);
        assert_eq!(parsed["duration_ms"], 4800);
        assert_eq!(parsed["task_prefix"], "fix credential routing");
    }

    #[test]
    fn task_prefix_truncates_at_60() {
        let long = "x".repeat(100);
        let r = UsageRecord::new("agent", "model", "openrouter", 0, 0, 0, &long);
        assert_eq!(r.task_prefix.chars().count(), 60);
    }

    #[test]
    fn task_prefix_handles_short_task() {
        let r = UsageRecord::new("agent", "model", "openrouter", 0, 0, 0, "hi");
        assert_eq!(r.task_prefix, "hi");
    }

    #[test]
    fn task_prefix_does_not_split_multibyte_codepoints() {
        // 70 emoji × 4 UTF-8 bytes each — must take 60 chars, not 60 bytes.
        let task: String = std::iter::repeat_n('🦀', 70).collect();
        let r = UsageRecord::new("a", "m", "openrouter", 0, 0, 0, &task);
        assert_eq!(r.task_prefix.chars().count(), 60);
    }

    #[tokio::test]
    async fn append_usage_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let r = UsageRecord::new("agent", "model", "openrouter", 10, 20, 100, "task");
        append_usage(dir.path(), &r).await;
        let path = dir.path().join(".open-mpm/state/usage.jsonl");
        assert!(path.exists(), "usage.jsonl should be created");
        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 1, "exactly one line after one append");
        let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["agent"], "agent");
    }

    #[tokio::test]
    async fn append_usage_appends() {
        let dir = tempfile::tempdir().unwrap();
        let r1 = UsageRecord::new("a1", "m1", "openrouter", 1, 2, 3, "first");
        let r2 = UsageRecord::new("a2", "m2", "anthropic-direct", 4, 5, 6, "second");
        append_usage(dir.path(), &r1).await;
        append_usage(dir.path(), &r2).await;
        let path = dir.path().join(".open-mpm/state/usage.jsonl");
        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2, "second append should not overwrite");
        let p1: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        let p2: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(p1["agent"], "a1");
        assert_eq!(p2["agent"], "a2");
    }
}
