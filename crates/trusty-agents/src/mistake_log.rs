//! Per-session and global mistake log for failed agent runs. (#186)
//!
//! Why: `interaction_log` captures successful conversational turns, but when
//! an agent subprocess crashes or an LLM API returns a 4xx/5xx, the failure
//! signal vanishes after the WARN line scrolls off. A grep-friendly JSONL
//! log keyed by session id lets a postmortem agent (or operator) later
//! analyze what went wrong, categorize root causes, and propose fixes.
//! What: `MistakeRecord` is one structured failure event with truncated
//! stdout/stderr/context. `MistakeLog::record` mirrors writes to both the
//! per-project session file and the global cross-project log.
//! Test: `mistake_log_records_nonzero_exit`,
//! `mistake_log_truncates_long_output`, `mistake_type_serializes_correctly`.
//!
//! # #186

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::AsyncWriteExt;

/// Category of failure observed by the harness. (#186)
///
/// Why: Postmortem analysis needs to bucket mistakes so it can apply the
/// right fix template (prompt update vs. skill add vs. infra issue).
/// What: A small closed enum; `Unknown`-style tail is left to the caller
/// to encode via `Other` if needed in future.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MistakeType {
    /// Agent subprocess exited with a non-zero status.
    NonzeroExit,
    /// LLM API returned a 4xx or 5xx response.
    ApiError,
    /// Agent stdout could not be parsed as a valid IPC message.
    MalformedOutput,
    /// A tool invocation reported an error to the agent.
    ToolError,
    /// Subprocess or HTTP request exceeded a configured timeout.
    Timeout,
}

/// One failure event recorded for postmortem analysis. (#186)
///
/// Why: Postmortem needs structured fields (agent name, exit code, phase)
/// rather than free-form log lines so categorization can be automated.
/// What: stdout/stderr are truncated to 2000 chars to keep records small.
/// `context` holds phase/wave + a task preview.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MistakeRecord {
    /// ISO-8601 UTC timestamp.
    pub ts: String,
    /// Session identifier (matches TAGENT_RUN_ID or build label).
    pub session_id: String,
    /// Agent name that produced the failure (e.g. "qa-agent").
    pub agent: String,
    /// Task identifier (uuid or build label) for cross-referencing.
    pub task_id: String,
    /// Categorized failure type.
    pub mistake_type: MistakeType,
    /// Subprocess exit code, if applicable.
    pub exit_code: Option<i32>,
    /// Truncated stderr (max 2000 chars).
    pub stderr: String,
    /// Truncated stdout (max 2000 chars).
    pub stdout: String,
    /// Free-form context: phase, wave number, task preview.
    pub context: String,
}

/// Append-only mistake log spanning per-project + global JSONL files. (#186)
///
/// Why: Mirrors `InteractionLog` so callers have one obvious place to record
/// failures and one obvious place to read them back. Stateless type — all
/// methods are static — keeps the call sites compact (`MistakeLog::record(...)`).
pub struct MistakeLog;

#[allow(dead_code)]
impl MistakeLog {
    /// Append a mistake to both project-local and global logs. (#186)
    ///
    /// Why: Mirroring on every write keeps cross-project search and per-
    /// project replay in sync without a reconciliation step.
    /// What: Writes to
    /// `<project>/.trusty-agents/state/mistakes/<session_id>.jsonl` and
    /// `~/.trusty-agents/sessions/mistakes.jsonl`. Both files are created if
    /// missing.
    /// Test: `mistake_log_records_nonzero_exit` covers the happy path.
    pub async fn record(project_root: &Path, record: &MistakeRecord) -> Result<()> {
        let local = local_log_path(project_root, &record.session_id);
        let global = global_log_path()?;
        append_line(&local, record).await?;
        append_line(&global, record).await?;
        Ok(())
    }

    /// Read all mistakes for a session from the project-local log. (#186)
    ///
    /// Why: Postmortem needs to enumerate one session's failures to produce
    /// a focused report.
    /// What: Reads `<project>/.trusty-agents/state/mistakes/<session_id>.jsonl`,
    /// parses each line, returns most-recent-last (file order). Missing
    /// files yield an empty vec.
    /// Test: `mistake_log_records_nonzero_exit` reads back what it wrote.
    pub fn read_session(project_root: &Path, session_id: &str) -> Result<Vec<MistakeRecord>> {
        let path = local_log_path(project_root, session_id);
        read_records(&path)
    }

    /// Read the N most recent mistakes from the global log. (#186)
    ///
    /// Why: The CLI `postmortem --last N` subcommand needs to look back
    /// across projects when the user doesn't have a specific session id.
    /// What: Reads `~/.trusty-agents/sessions/mistakes.jsonl` and returns the
    /// trailing `n` rows in chronological (oldest-first) order.
    /// Test: Indirectly via the CLI subcommand.
    pub fn read_recent_global(n: usize) -> Result<Vec<MistakeRecord>> {
        if n == 0 {
            return Ok(Vec::new());
        }
        let path = global_log_path()?;
        let all = read_records(&path)?;
        let start = all.len().saturating_sub(n);
        Ok(all.into_iter().skip(start).collect())
    }
}

/// Resolve `<project>/.trusty-agents/state/mistakes/<session_id>.jsonl`.
fn local_log_path(project_root: &Path, session_id: &str) -> PathBuf {
    project_root
        .join(".trusty-agents")
        .join("state")
        .join("mistakes")
        .join(format!("{session_id}.jsonl"))
}

/// Resolve `~/.trusty-agents/sessions/mistakes.jsonl`.
pub fn global_log_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("no home dir")?;
    Ok(home
        .join(".trusty-agents")
        .join("sessions")
        .join("mistakes.jsonl"))
}

/// Truncate a string to at most `max` characters. (#186)
///
/// Why: stdout/stderr from a misbehaving agent can be megabytes; we don't
/// want to bloat the JSONL log.
/// What: Returns `s` unchanged when within bounds, otherwise the first
/// `max` chars + `…` marker.
/// Test: `mistake_log_truncates_long_output`.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// Append one record as a JSON line.
///
/// Why: Mistake records must be visible to the synchronous `read_session` /
/// `read_recent_global` readers that follow immediately in tests and
/// postmortem flows. Without `flush`, Tokio's `File` (which uses
/// `spawn_blocking` internally) may not have committed the buffer by the time
/// the caller reads the file back. Fix mirrors the pattern from PR #532.
/// What: Opens the file in create+append mode, writes `line + "\n"` — bailing
/// out on write failure before flushing — then flushes to guarantee visibility.
/// Test: `mistake_log_records_nonzero_exit` reads back what `record` wrote.
async fn append_line(path: &Path, record: &MistakeRecord) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    let line = serde_json::to_string(record)?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    // Write first; short-circuit before flush on failure so we don't flush a
    // partial write.
    if let Err(e) = file.write_all(line.as_bytes()).await {
        return Err(e.into());
    }
    if let Err(e) = file.write_all(b"\n").await {
        return Err(e.into());
    }
    file.flush().await?;
    Ok(())
}

/// Read all records from a JSONL file (sync; small files).
fn read_records(path: &Path) -> Result<Vec<MistakeRecord>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<MistakeRecord>(line) {
            Ok(r) => out.push(r),
            Err(e) => tracing::debug!(error = %e, "skipping unparseable mistake line"),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record(session_id: &str, agent: &str) -> MistakeRecord {
        MistakeRecord {
            ts: chrono::Utc::now().to_rfc3339(),
            session_id: session_id.to_string(),
            agent: agent.to_string(),
            task_id: "task-1".to_string(),
            mistake_type: MistakeType::NonzeroExit,
            exit_code: Some(1),
            stderr: "boom".to_string(),
            stdout: String::new(),
            context: "phase=code".to_string(),
        }
    }

    // Holding a `std::sync::MutexGuard` across `.await` is intentional here:
    // the whole point of HOME_LOCK is to serialize HOME-mutating tests for
    // their entire duration, including all async I/O. tokio's
    // `await_holding_lock` lint flags the pattern as a deadlock risk for
    // production code, but in this single-purpose test guard there are no
    // other tasks contending for the same lock — so we silence it.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn mistake_log_records_nonzero_exit() {
        // Why: `MistakeLog::record` writes to BOTH a project-local path and
        // a global path resolved via `dirs::home_dir()` (which honours
        // `$HOME`). To make this test hermetic we sandbox `$HOME` to a
        // tempdir. `std::env::set_var` is a process-wide mutation, so
        // concurrent tests in other modules (e.g. `init::tests::*`) that
        // also sandbox HOME would race with this one. `crate::test_env::HOME_LOCK`
        // is a process-wide Mutex shared across modules that serializes
        // any test mutating HOME — without it this test passes in isolation
        // but flakes under `cargo test`.
        let _guard = crate::test_env::HOME_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("HOME");
        // SAFETY: HOME_LOCK is held for the entire test body; restoration
        // runs before the guard is dropped, so no other test observes a
        // half-set HOME.
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }

        let project = tmp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let rec = sample_record("sess-1", "qa-agent");
        MistakeLog::record(&project, &rec).await.unwrap();

        let session_records = MistakeLog::read_session(&project, "sess-1").unwrap();
        assert_eq!(session_records.len(), 1);
        assert_eq!(session_records[0].agent, "qa-agent");
        assert_eq!(session_records[0].mistake_type, MistakeType::NonzeroExit);
        assert_eq!(session_records[0].exit_code, Some(1));

        // Global log mirror — uses the sandboxed HOME, so we know exactly
        // one record exists and it's the one we just wrote.
        let recent = MistakeLog::read_recent_global(10).unwrap();
        assert!(
            recent.iter().any(|r| r.agent == "qa-agent"),
            "global log should contain the recorded agent; recent={recent:?}"
        );

        // SAFETY: HOME_LOCK still held; safe to mutate env.
        unsafe {
            match prev {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[test]
    fn mistake_log_truncates_long_output() {
        let s: String = "a".repeat(5000);
        let t = truncate(&s, 2000);
        // Truncate keeps `max` chars and appends one ellipsis char.
        assert_eq!(t.chars().count(), 2001);
        assert!(t.ends_with('…'));
        // Short strings pass through unchanged.
        assert_eq!(truncate("hi", 2000), "hi");
    }

    #[test]
    fn mistake_type_serializes_correctly() {
        let rec = MistakeRecord {
            ts: "2026-04-24T10:00:00Z".to_string(),
            session_id: "s".to_string(),
            agent: "a".to_string(),
            task_id: "t".to_string(),
            mistake_type: MistakeType::ApiError,
            exit_code: None,
            stderr: String::new(),
            stdout: String::new(),
            context: String::new(),
        };
        let json = serde_json::to_string(&rec).unwrap();
        assert!(
            json.contains("\"mistake_type\":\"api_error\""),
            "expected snake_case mistake_type; got {json}"
        );
        // Round-trip parse.
        let back: MistakeRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.mistake_type, MistakeType::ApiError);

        // Cover the other variants.
        for v in [
            MistakeType::NonzeroExit,
            MistakeType::MalformedOutput,
            MistakeType::ToolError,
            MistakeType::Timeout,
        ] {
            let s = serde_json::to_string(&v).unwrap();
            let back: MistakeType = serde_json::from_str(&s).unwrap();
            assert_eq!(back, v);
        }
    }
}
