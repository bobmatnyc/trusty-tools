//! Session recap generation (#371).
//!
//! After every N completed tasks in a session, assembles a structured recap:
//! a one-line prose summary + a table of (step, result) rows drawn from
//! task history. The recap is emitted as a `RecapGenerated` event and stored
//! to `.trusty-agents/state/recaps/{session_id}.json`.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// A (phase_name, phase_result) pair for a single completed phase.
pub type RecapPhase = (String, String);

/// A (task_prompt, narrative, phases_completed) tuple summarising one task.
pub type RecapTask = (String, String, Vec<RecapPhase>);

/// A single row in the recap table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecapRow {
    pub step: String,
    pub result: String,
}

/// A generated session recap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recap {
    pub session_id: String,
    pub summary: String,
    pub rows: Vec<RecapRow>,
    pub generated_at: String, // RFC3339
    pub task_count: usize,    // how many tasks were included
}

/// Config controlling recap generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecapConfig {
    /// Whether recap generation is enabled (default: true).
    pub enabled: bool,
    /// Generate a recap after every N completed tasks (default: 5).
    pub interval: usize,
}

impl Default for RecapConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval: 5,
        }
    }
}

/// Tracks per-session task completion count for recap triggering.
///
/// Why: Recap generation must fire after every N completed tasks per session,
/// without persisting counts across restarts (best-effort telemetry, not
/// control flow). A simple in-memory map keyed by session_id is enough.
/// What: `tick(session_id)` increments the counter and returns true when the
/// configured interval is hit, resetting in the same step.
/// Test: `tracker_triggers_at_interval` exercises the increment-and-reset path.
pub struct RecapTracker {
    pub config: RecapConfig,
    /// session_id -> number of tasks completed since last recap
    counts: std::collections::HashMap<String, usize>,
}

impl RecapTracker {
    pub fn new(config: RecapConfig) -> Self {
        Self {
            config,
            counts: Default::default(),
        }
    }

    /// Increment the task counter for a session. Returns true when a recap
    /// should be generated (count hit the interval).
    pub fn tick(&mut self, session_id: &str) -> bool {
        if !self.config.enabled {
            return false;
        }
        let count = self.counts.entry(session_id.to_string()).or_insert(0);
        *count += 1;
        if *count >= self.config.interval {
            *count = 0;
            true
        } else {
            false
        }
    }

    /// Reset the counter for a session (called when recap is generated).
    pub fn reset(&mut self, session_id: &str) {
        self.counts.insert(session_id.to_string(), 0);
    }
}

/// Assemble a recap from a slice of recent task narratives and phase data.
///
/// Why: The recap surfaces what was actually accomplished — commit hashes,
/// phase outcomes, narrative summary. Centralising the assembly keeps the
/// emit site (server.rs / workflow) trivial and the format consistent.
/// What: `recent_tasks` is a slice of (task_prompt, narrative, phases_completed)
/// tuples — the last N tasks from the session. Builds table rows from phases
/// and any commit hashes found in narratives, then produces a prose summary
/// from the most recent non-empty narrative.
/// Test: `assemble_recap_extracts_commit`, `assemble_recap_summary_from_narrative`.
pub fn assemble_recap(session_id: &str, recent_tasks: &[RecapTask]) -> Recap {
    let mut rows: Vec<RecapRow> = Vec::new();

    for (_, narrative, phases) in recent_tasks {
        // Extract commit hash if present
        if let Some(m) = extract_commit(narrative)
            && !rows.iter().any(|r: &RecapRow| r.step == "Commit")
        {
            rows.push(RecapRow {
                step: "Commit".into(),
                result: m,
            });
        }
        // Add phase rows (deduped by step name)
        for (phase_name, phase_result) in phases {
            let step = capitalise(phase_name);
            if !rows.iter().any(|r| r.step == step) {
                rows.push(RecapRow {
                    step,
                    result: phase_result.clone(),
                });
            }
        }
    }

    // Prose summary: take the most recent non-empty narrative
    let summary = recent_tasks
        .iter()
        .rev()
        .find_map(|(_, n, _)| {
            if !n.trim().is_empty() {
                Some(n.trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| format!("Completed {} task(s).", recent_tasks.len()));

    Recap {
        session_id: session_id.to_string(),
        summary,
        rows,
        generated_at: chrono::Utc::now().to_rfc3339(),
        task_count: recent_tasks.len(),
    }
}

/// Extract a commit-like hex hash from text. Returns the first 7-40 char
/// alphanumeric token that is all hex.
///
/// Why: Narratives often contain commit hashes (e.g. "Committed abc1234 to
/// main"). Surfacing these in the recap table gives users a clickable
/// reference. We avoid pulling in regex setup overhead by scanning whitespace
/// tokens directly.
/// What: Scans whitespace-separated words, strips non-alphanumeric trailing
/// punctuation, returns the first hex token of length 7..=40.
/// Test: covered indirectly by `assemble_recap_extracts_commit`.
fn extract_commit(text: &str) -> Option<String> {
    for word in text.split_whitespace() {
        let w = word.trim_matches(|c: char| !c.is_alphanumeric());
        if w.len() >= 7 && w.len() <= 40 && w.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(w.to_string());
        }
    }
    None
}

/// Capitalise the first character of a string.
fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

/// Persist a recap to `state_dir/recaps/{session_id}.json`.
///
/// Why: Recaps must survive process restart so the GUI can backfill the
/// RecapPanel after reload. A recap is a summary built from task narratives,
/// so it is exactly as sensitive as the `tasks.json` it is derived from — and
/// it used to be written with a bare `std::fs::write` at the process umask,
/// landing world-readable, which is the same gap `persist_tasks` had (#4355).
/// Routing through `state_writer` puts it under the one owner-only,
/// lock + tmp + rename contract every other state file already uses.
/// What: Serializes pretty JSON and hands it to
/// [`crate::state_writer::atomic_write`], which creates the `recaps/` subdir
/// itself — hence no separate `create_dir_all` here. Still returns `Result`
/// and still blocks: the caller (`api::server::task_runner::maybe_emit_recap`)
/// already invoked this synchronously and already logs-and-continues on error,
/// so neither its error handling nor its blocking behavior changes. The
/// advisory lock is per-session (`recaps/{session_id}.json.lock`), not a
/// shared path, so it is effectively uncontended.
/// Test: `save_and_load_recap_roundtrip`, `save_recap_writes_an_owner_only_file`.
pub fn save_recap(state_dir: &Path, recap: &Recap) -> Result<()> {
    let path = state_dir
        .join("recaps")
        .join(format!("{}.json", recap.session_id));
    let json = serde_json::to_string_pretty(recap)?;
    crate::state_writer::atomic_write(&path, json.as_bytes())
}

/// Load the most recent recap for a session, if any.
///
/// Why: Backs the `GET /api/sessions/:id/recap` endpoint so the GUI can
/// render the latest recap on page load.
/// What: Reads `state_dir/recaps/{session_id}.json` and deserialises. Any
/// error (missing file, malformed JSON) returns None; the API maps that to 404.
/// Test: covered by integration; round-trip with `save_recap`.
pub fn load_recap(state_dir: &Path, session_id: &str) -> Option<Recap> {
    let path = state_dir.join("recaps").join(format!("{session_id}.json"));
    let json = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&json).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_triggers_at_interval() {
        let mut t = RecapTracker::new(RecapConfig {
            enabled: true,
            interval: 3,
        });
        assert!(!t.tick("s1"));
        assert!(!t.tick("s1"));
        assert!(t.tick("s1")); // 3rd -> trigger
        assert!(!t.tick("s1")); // resets
    }

    #[test]
    fn tracker_disabled_never_triggers() {
        let mut t = RecapTracker::new(RecapConfig {
            enabled: false,
            interval: 1,
        });
        assert!(!t.tick("s1"));
        assert!(!t.tick("s1"));
    }

    #[test]
    fn tracker_resets_on_demand() {
        let mut t = RecapTracker::new(RecapConfig {
            enabled: true,
            interval: 5,
        });
        t.tick("s1");
        t.tick("s1");
        t.reset("s1");
        // After reset, two more ticks should not trigger (count back at 2/5).
        assert!(!t.tick("s1"));
        assert!(!t.tick("s1"));
    }

    #[test]
    fn assemble_recap_extracts_commit() {
        let tasks = vec![("task".into(), "Committed abc1234 to main".into(), vec![])];
        let r = assemble_recap("sess", &tasks);
        assert!(
            r.rows
                .iter()
                .any(|row| row.step == "Commit" && row.result.contains("abc1234")),
            "rows: {:?}",
            r.rows
        );
    }

    #[test]
    fn assemble_recap_summary_from_narrative() {
        let tasks = vec![(
            "task".into(),
            "Deployed the API. All checks passed.".into(),
            vec![],
        )];
        let r = assemble_recap("sess", &tasks);
        assert!(r.summary.contains("Deployed"));
    }

    #[test]
    fn assemble_recap_includes_phase_rows() {
        let tasks = vec![(
            "task".into(),
            "".into(),
            vec![
                ("research".into(), "found 3 references".into()),
                ("code".into(), "patched src/lib.rs".into()),
            ],
        )];
        let r = assemble_recap("sess", &tasks);
        assert!(r.rows.iter().any(|r| r.step == "Research"));
        assert!(r.rows.iter().any(|r| r.step == "Code"));
    }

    #[test]
    fn assemble_recap_dedupes_steps() {
        let tasks = vec![
            (
                "t1".into(),
                "".into(),
                vec![("code".into(), "first".into())],
            ),
            (
                "t2".into(),
                "".into(),
                vec![("code".into(), "second".into())],
            ),
        ];
        let r = assemble_recap("sess", &tasks);
        let code_rows: Vec<_> = r.rows.iter().filter(|r| r.step == "Code").collect();
        assert_eq!(code_rows.len(), 1);
    }

    #[test]
    fn assemble_recap_falls_back_when_no_narrative() {
        let tasks = vec![("t".into(), "".into(), vec![])];
        let r = assemble_recap("sess", &tasks);
        assert!(r.summary.contains("Completed"));
    }

    #[test]
    fn save_and_load_recap_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().to_path_buf();
        let recap = Recap {
            session_id: "sess1".into(),
            summary: "All good".into(),
            rows: vec![RecapRow {
                step: "Commit".into(),
                result: "abc1234".into(),
            }],
            generated_at: chrono::Utc::now().to_rfc3339(),
            task_count: 5,
        };
        save_recap(&state_dir, &recap).unwrap();
        let loaded = load_recap(&state_dir, "sess1").unwrap();
        assert_eq!(loaded.session_id, "sess1");
        assert_eq!(loaded.rows.len(), 1);
        assert_eq!(loaded.rows[0].result, "abc1234");
    }

    /// #4355: a recap summarises task narratives, so it is as sensitive as the
    /// `tasks.json` it is derived from. Written with a bare `std::fs::write` it
    /// landed at the process umask (0644 — world-readable on a shared host);
    /// routed through `state_writer::atomic_write` it inherits the owner-only
    /// contract every other state file now has.
    #[cfg(unix)]
    #[test]
    fn save_recap_writes_an_owner_only_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let recap = Recap {
            session_id: "sess-perm".into(),
            summary: "sensitive narrative summary".into(),
            rows: Vec::new(),
            generated_at: chrono::Utc::now().to_rfc3339(),
            task_count: 1,
        };
        save_recap(dir.path(), &recap).unwrap();

        let path = dir.path().join("recaps").join("sess-perm.json");
        let mode = std::fs::metadata(&path)
            .expect("recap file must exist")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "recap must not be world-readable");
    }
}
