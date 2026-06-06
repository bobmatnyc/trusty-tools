//! Per-session and global interaction log for PM/CTRL/user turns.
//!
//! Why: `session_record` captures one row per workflow run (cost, status,
//! duration) — enough for "what did I run?" but not "what did I say?". Many
//! CTRL workflows (search prior conversations, replay PM-to-PM relays,
//! reconstruct context after a crash) need the actual back-and-forth text.
//! A second JSONL log keyed by session-id, mirrored to a global file,
//! reuses the `session_record::append_to`/`search_in` pattern so the
//! grep-friendly story stays consistent across both stores.
//! What: `Interaction` is one role-tagged content turn with optional tool
//! call names. `InteractionLog::append` writes the line to BOTH the
//! per-project session file (`<project>/.trusty-agents/state/interactions/<sid>.jsonl`)
//! and the global cross-project log (`~/.trusty-agents/sessions/interactions.jsonl`).
//! `search` does case-insensitive substring matching on `content` and `role`;
//! `recent(n)` returns the last N entries.
//! Test: `append_creates_both_files`, `search_finds_by_content`,
//! `recent_returns_last_n`, `interaction_roundtrip_json`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::state_writer;

/// One turn in a CTRL/PM/user conversation.
///
/// Why: Captures the minimum to reconstruct "who said what when" — a JSONL
/// row keyed by ISO-8601 timestamp + session_id is enough for grep-style
/// retrieval without a database.
/// What: `role` is a free-form string (`"user" | "pm" | "ctrl" | "pm-to-pm"`),
/// `content` is the raw text turn, `tool_calls` records the names of any
/// tools the assistant invoked during the turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Interaction {
    /// ISO-8601 timestamp (UTC) of when this turn was recorded.
    pub timestamp: String,
    /// Session identifier — ties together turns within one PM/CTRL run.
    pub session_id: String,
    /// Absolute path of the project this interaction belongs to.
    pub project_path: String,
    /// Speaker role: `"user" | "pm" | "ctrl" | "pm-to-pm"` (free-form).
    pub role: String,
    /// Raw textual content of the turn.
    pub content: String,
    /// Names of tools invoked by the assistant in this turn, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<String>>,
}

/// Per-session + global interaction log with append/search/recent operations.
///
/// Why: Bundling the two destination paths in one struct keeps `append`
/// atomic from the caller's perspective ("write the turn") and centralizes
/// path resolution for tests.
/// What: `local_path` lives under the project tree; `global_path` lives
/// under `$HOME/.trusty-agents/sessions/`. `session_id` and `project_path` are
/// stamped onto every appended `Interaction`.
pub struct InteractionLog {
    /// Per-project log path: `<project>/.trusty-agents/state/interactions/<session_id>.jsonl`.
    pub local_path: PathBuf,
    /// Global cross-project log: `~/.trusty-agents/sessions/interactions.jsonl`.
    pub global_path: PathBuf,
    /// Session identifier stamped on every appended turn.
    pub session_id: String,
    /// Project root path (string form) stamped on every appended turn.
    pub project_path: String,
}

#[allow(dead_code)]
impl InteractionLog {
    /// Build an `InteractionLog` rooted at `project_path` for `session_id`.
    ///
    /// Why: Centralizes the "where do interaction logs live?" answer so callers
    /// in main.rs and ctrl/mod.rs don't duplicate path arithmetic.
    /// What: `local_path` =
    /// `<project>/.trusty-agents/state/interactions/<session_id>.jsonl`.
    /// `global_path` falls back to `./.trusty-agents/sessions/interactions.jsonl`
    /// when `$HOME` is unset (tests).
    /// Test: `append_creates_both_files` constructs via `new`.
    pub fn new(project_path: &Path, session_id: &str) -> Self {
        let local_path = project_path
            .join(".trusty-agents")
            .join("state")
            .join("interactions")
            .join(format!("{session_id}.jsonl"));
        let global_path = global_log_path()
            .unwrap_or_else(|_| PathBuf::from(".trusty-agents/sessions/interactions.jsonl"));
        Self {
            local_path,
            global_path,
            session_id: session_id.to_string(),
            project_path: project_path.to_string_lossy().to_string(),
        }
    }

    /// Append one interaction to BOTH the local and global JSONL logs.
    ///
    /// Why: Mirroring on every write means CTRL's cross-project search and
    /// per-project replay both stay current without a reconciliation step.
    /// What: Builds an `Interaction`, serializes it to one JSON line, then
    /// calls `append_line` for `local_path` followed by `global_path`.
    /// Errors from the global write are returned (local write succeeds first
    /// because it's the more useful of the two for the active project).
    /// Test: `append_creates_both_files` asserts both files exist after one
    /// append.
    pub async fn append(
        &self,
        role: &str,
        content: &str,
        tool_calls: Option<Vec<String>>,
    ) -> Result<()> {
        let interaction = Interaction {
            timestamp: now_iso8601(),
            session_id: self.session_id.clone(),
            project_path: self.project_path.clone(),
            role: role.to_string(),
            content: content.to_string(),
            tool_calls,
        };
        append_line(&self.local_path, &interaction).await?;
        append_line(&self.global_path, &interaction).await?;
        Ok(())
    }

    /// Case-insensitive substring search across `content` and `role`.
    ///
    /// Why: Mirrors `session_record::search_in` so CTRL's `/sessions`-style
    /// commands can grep both stores with identical semantics.
    /// What: Reads `log_path` line-by-line, parses each as `Interaction`,
    /// keeps rows whose lowercased `content` or `role` contains the
    /// lowercased `query`. An empty query returns all rows. Returns most-
    /// recent-first. Missing files yield an empty vector.
    /// Test: `search_finds_by_content` exercises the substring match.
    pub async fn search(query: &str, log_path: &Path) -> Vec<Interaction> {
        let text = match fs::read_to_string(log_path).await {
            Ok(t) => t,
            Err(_) => return Vec::new(),
        };
        let q = query.to_lowercase();
        let mut hits: Vec<Interaction> = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<Interaction>(line) {
                Ok(r) => {
                    if q.is_empty()
                        || r.content.to_lowercase().contains(&q)
                        || r.role.to_lowercase().contains(&q)
                    {
                        hits.push(r);
                    }
                }
                Err(e) => {
                    tracing::debug!(error = %e, "skipping unparseable interactions line");
                }
            }
        }
        hits.reverse();
        hits
    }

    /// Return the last `n` entries from `log_path`, most-recent-first.
    ///
    /// Why: A simple "show me the tail" affordance for CTRL's status / replay
    /// without forcing callers to read the whole file.
    /// What: Reads the file, parses each line as `Interaction`, takes the
    /// trailing `n` rows, and reverses to most-recent-first. Missing files
    /// yield an empty vector.
    /// Test: `recent_returns_last_n` confirms the limit and ordering.
    pub async fn recent(log_path: &Path, n: usize) -> Vec<Interaction> {
        if n == 0 {
            return Vec::new();
        }
        let text = match fs::read_to_string(log_path).await {
            Ok(t) => t,
            Err(_) => return Vec::new(),
        };
        let mut rows: Vec<Interaction> = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(r) = serde_json::from_str::<Interaction>(line) {
                rows.push(r);
            }
        }
        let start = rows.len().saturating_sub(n);
        let mut tail: Vec<Interaction> = rows.into_iter().skip(start).collect();
        tail.reverse();
        tail
    }
}

/// Resolve the global cross-project interaction log path.
///
/// Why: Centralizes home-dir resolution; mirrors `session_record::runs_path`.
/// What: Returns `~/.trusty-agents/sessions/interactions.jsonl`. Errors if `$HOME`
/// is unset.
/// Test: Indirectly exercised by `append_creates_both_files` (which overrides
/// the path explicitly via `InteractionLog` field assignment).
pub fn global_log_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("no home dir")?;
    Ok(home
        .join(".trusty-agents")
        .join("sessions")
        .join("interactions.jsonl"))
}

/// Append one `Interaction` as a JSON line to `path`.
///
/// Why: Inner routine paralleling `session_record::append_to`; tests use it
/// against a tempdir so no `$HOME` mutation is needed.
/// What: Creates the parent dir, opens the file in create+append mode, writes
/// `serde_json::to_string(interaction) + "\n"`.
/// Test: `append_creates_both_files`, `interaction_roundtrip_json`.
async fn append_line(path: &Path, interaction: &Interaction) -> Result<()> {
    let line = serde_json::to_string(interaction)?;
    let path_owned = path.to_path_buf();
    // #198: Concurrency-safe append so multiple trusty-agents processes sharing
    // the global interactions log cannot interleave bytes within a JSONL
    // row. fs4 syscalls are blocking; hop to a worker thread.
    tokio::task::spawn_blocking(move || state_writer::atomic_append_line(&path_owned, &line))
        .await
        .context("spawn_blocking for interaction_log append")??;
    Ok(())
}

/// Best-effort UTC ISO-8601 timestamp.
///
/// Why: Avoids pulling additional date deps; `chrono` is already in the
/// dependency tree.
/// What: Returns `chrono::Utc::now().to_rfc3339()`.
fn now_iso8601() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(tmp: &Path) -> InteractionLog {
        InteractionLog {
            local_path: tmp.join("local.jsonl"),
            global_path: tmp.join("global.jsonl"),
            session_id: "sess-1".to_string(),
            project_path: tmp.to_string_lossy().to_string(),
        }
    }

    #[tokio::test]
    async fn append_creates_both_files() {
        let tmp = tempfile::tempdir().unwrap();
        let log = fixture(tmp.path());
        log.append("user", "hello world", None).await.unwrap();
        assert!(log.local_path.exists(), "local file not created");
        assert!(log.global_path.exists(), "global file not created");
        let local_text = std::fs::read_to_string(&log.local_path).unwrap();
        let global_text = std::fs::read_to_string(&log.global_path).unwrap();
        assert!(local_text.contains("hello world"));
        assert!(global_text.contains("hello world"));
    }

    #[tokio::test]
    async fn search_finds_by_content() {
        let tmp = tempfile::tempdir().unwrap();
        let log = fixture(tmp.path());
        log.append("user", "build a fastapi app", None)
            .await
            .unwrap();
        log.append("pm", "delegating to python-engineer", None)
            .await
            .unwrap();
        log.append("ctrl", "unrelated note", None).await.unwrap();

        let hits = InteractionLog::search("fastapi", &log.local_path).await;
        assert_eq!(hits.len(), 1);
        assert!(hits[0].content.contains("fastapi"));

        // Role match also works (role substring).
        let by_role = InteractionLog::search("ctrl", &log.local_path).await;
        assert_eq!(by_role.len(), 1);

        // Empty query returns all entries.
        let all = InteractionLog::search("", &log.local_path).await;
        assert_eq!(all.len(), 3);

        // Missing file => empty vec, no error.
        let missing = InteractionLog::search("anything", &tmp.path().join("nope.jsonl")).await;
        assert!(missing.is_empty());
    }

    #[tokio::test]
    async fn recent_returns_last_n() {
        let tmp = tempfile::tempdir().unwrap();
        let log = fixture(tmp.path());
        for i in 0..5 {
            log.append("user", &format!("turn-{i}"), None)
                .await
                .unwrap();
        }
        let last2 = InteractionLog::recent(&log.local_path, 2).await;
        assert_eq!(last2.len(), 2);
        // Most-recent-first.
        assert!(last2[0].content.contains("turn-4"));
        assert!(last2[1].content.contains("turn-3"));

        // n=0 returns empty.
        assert!(InteractionLog::recent(&log.local_path, 0).await.is_empty());

        // Missing file => empty.
        assert!(
            InteractionLog::recent(&tmp.path().join("missing.jsonl"), 5)
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn interaction_roundtrip_json() {
        let original = Interaction {
            timestamp: "2026-04-24T10:00:00Z".to_string(),
            session_id: "sess-1".to_string(),
            project_path: "/tmp/proj".to_string(),
            role: "pm-to-pm".to_string(),
            content: "ping".to_string(),
            tool_calls: Some(vec!["delegate_to_agent".to_string()]),
        };
        let line = serde_json::to_string(&original).unwrap();
        let parsed: Interaction = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed.timestamp, original.timestamp);
        assert_eq!(parsed.session_id, original.session_id);
        assert_eq!(parsed.project_path, original.project_path);
        assert_eq!(parsed.role, original.role);
        assert_eq!(parsed.content, original.content);
        assert_eq!(
            parsed.tool_calls.as_deref(),
            Some(&["delegate_to_agent".to_string()][..])
        );

        // Default tool_calls (None) round-trips and is omitted from JSON.
        let no_tools = Interaction {
            tool_calls: None,
            ..original
        };
        let s = serde_json::to_string(&no_tools).unwrap();
        assert!(!s.contains("tool_calls"), "None should be skipped");
        let back: Interaction = serde_json::from_str(&s).unwrap();
        assert!(back.tool_calls.is_none());
    }
}
