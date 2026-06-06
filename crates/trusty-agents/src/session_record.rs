//! Cross-project session record store at `~/.trusty-agents/sessions/runs.jsonl`.
//!
//! Why: CTRL (the top-level controller) needs to answer "what did I do last
//! week in project X?" without scanning each project's local perf/runs tree.
//! A single append-only JSONL file keyed by timestamp gives us fast substring
//! search and trivial durability — each workflow run records one line, and
//! CTRL's `search_sessions` tool greps the file.
//! What: `SessionRecord` is the projected subset of a workflow run we care
//! about cross-project (task preview, status, cost, duration, files, build).
//! `append_run_record` serializes one record as a single JSON line. `search`
//! reads the file and filters by case-insensitive substring match on
//! `task`, `project_path`, and `status`.
//! Test: `append_then_search_finds_record`, `search_missing_file_is_empty`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::perf::PerfRecord;
use crate::state_writer;

/// One row in `~/.trusty-agents/sessions/runs.jsonl`.
///
/// Why: Captures exactly the fields CTRL surfaces in search results — no
/// phase-level detail. Keeps the line short so greps stay cheap and users
/// can eyeball the file.
/// What: JSON-serializable; all fields optional-ish so future schema changes
/// don't break older lines (serde `default` on additions).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub timestamp: String,
    pub project_path: String,
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_level: Option<String>,
    pub workflow: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<String>,
    pub cost_usd: f64,
    pub duration_mins: u64,
    #[serde(default)]
    pub files_modified: Vec<String>,
    pub build_id: String,
}

/// Resolve `~/.trusty-agents/sessions/runs.jsonl`.
///
/// Why: Centralizes home-dir resolution; callers in main + ctrl both need it.
/// What: Errors if `$HOME` is unset.
/// Test: `runs_path_under_home`.
pub fn runs_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("no home dir")?;
    Ok(home
        .join(".trusty-agents")
        .join("sessions")
        .join("runs.jsonl"))
}

/// Build a `SessionRecord` from a workflow `PerfRecord` + context.
///
/// Why: The workflow engine produces `PerfRecord`; projecting here keeps the
/// translation logic in one place so main.rs stays focused on orchestration.
/// What: Reads totals.cost_usd / total_duration_ms from `PerfRecord`, derives
/// `task_level` from the task-file stem (e.g. `level-3.txt` → `L3`), and
/// formats the build id as `buildN`.
/// Test: `record_from_perf_extracts_fields`.
pub fn record_from_perf(
    perf: &PerfRecord,
    project_path: &Path,
    task_file: Option<&str>,
    files_modified: Vec<String>,
    score: Option<String>,
) -> SessionRecord {
    let task_level = task_file
        .and_then(|f| std::path::Path::new(f).file_stem())
        .and_then(|s| s.to_str())
        .and_then(|stem| stem.strip_prefix("level-").map(|n| format!("L{n}")));

    SessionRecord {
        timestamp: perf.started_at.clone(),
        project_path: project_path.to_string_lossy().to_string(),
        task: perf.task_preview.clone(),
        task_level,
        workflow: perf.workflow.clone(),
        status: perf.status.clone(),
        score,
        cost_usd: perf.totals.cost_usd,
        duration_mins: perf.total_duration_ms / 60_000,
        files_modified,
        build_id: format!("build{}", perf.build),
    }
}

/// Append one record as a JSON line to `~/.trusty-agents/sessions/runs.jsonl`.
///
/// Why: JSONL is append-only and tolerant of concurrent writers at one-line
/// granularity; we don't need a DB to answer CTRL's cross-project queries.
/// What: Creates the parent dir, opens the file in append mode, writes
/// `serde_json::to_string(record)` + '\n'.
/// Test: `append_then_search_finds_record`.
pub async fn append_run_record(record: &SessionRecord) -> Result<()> {
    append_to(&runs_path()?, record).await
}

/// Testable inner routine: append one record to the given path.
///
/// Why: Tests can't safely mutate `$HOME` on a multi-threaded runtime (unsafe
/// in 2024 edition); taking an explicit path avoids the global env var race.
/// What: Creates the parent dir and appends one JSON line.
/// Test: `append_then_search_finds_record`.
pub async fn append_to(path: &Path, record: &SessionRecord) -> Result<()> {
    let line = serde_json::to_string(record)?;
    let path_owned = path.to_path_buf();
    // #198: Use state_writer's advisory-locked atomic append so concurrent
    // trusty-agents processes (API server + CLI + GUI) cannot interleave bytes
    // within a single JSONL row. The lock acquisition is short-lived; we
    // hop to a blocking thread to avoid stalling the tokio runtime.
    tokio::task::spawn_blocking(move || state_writer::atomic_append_line(&path_owned, &line))
        .await
        .context("spawn_blocking for session_record append")??;
    Ok(())
}

/// Read all records, filtering by case-insensitive substring match.
///
/// Why: CTRL's `search_sessions` tool; a simple grep gives good-enough
/// recall for the handful of projects a user actually touches.
/// What: Reads the file line-by-line, parses each as `SessionRecord`
/// (skipping unparseable lines with a trace log), keeps rows whose
/// `task | project_path | status` contain the lowercased `query`. An
/// empty query returns all rows. Returns most-recent-first by file order
/// reversed.
/// Test: `append_then_search_finds_record`, `search_missing_file_is_empty`.
pub async fn search(query: &str) -> Result<Vec<SessionRecord>> {
    search_in(&runs_path()?, query).await
}

/// Testable inner routine: search a specific JSONL file by substring.
///
/// Why: Parallels `append_to` so tests can use a tempdir without touching
/// `$HOME`.
/// What: Same filter logic as `search`; returns an empty vec when absent.
/// Test: `append_then_search_finds_record`.
pub async fn search_in(path: &Path, query: &str) -> Result<Vec<SessionRecord>> {
    let text = match fs::read_to_string(path).await {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let q = query.to_lowercase();
    let mut hits: Vec<SessionRecord> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<SessionRecord>(line) {
            Ok(r) => {
                if q.is_empty()
                    || r.task.to_lowercase().contains(&q)
                    || r.project_path.to_lowercase().contains(&q)
                    || r.status.to_lowercase().contains(&q)
                {
                    hits.push(r);
                }
            }
            Err(e) => {
                tracing::debug!(error = %e, "skipping unparseable runs.jsonl line");
            }
        }
    }
    hits.reverse();
    Ok(hits)
}

/// Extract a "pass/fail" score marker from observe-phase output, if any.
///
/// Why: The observe agent typically writes a line like `Score: 35/35` or
/// `Result: PASS` in its report. Surfacing it in the session record lets
/// CTRL show a one-glance pass/fail without reopening the workflow report.
/// What: Scans for the first line that starts with `Score:`, `Result:`, or
/// contains `/35` / `/25` digit-slash-digit patterns. Returns the trimmed
/// value after the colon, or the full matched substring.
/// Test: `extract_score_finds_score_line`.
pub fn extract_score(observe_output: &str) -> Option<String> {
    for line in observe_output.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed
            .strip_prefix("Score:")
            .or_else(|| trimmed.strip_prefix("score:"))
        {
            let v = rest.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
        if let Some(rest) = trimmed
            .strip_prefix("Result:")
            .or_else(|| trimmed.strip_prefix("result:"))
        {
            let v = rest.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    // Fallback: a bare N/M pattern somewhere.
    for line in observe_output.lines() {
        for word in line.split_whitespace() {
            if let Some((a, b)) = word.split_once('/')
                && !a.is_empty()
                && !b.is_empty()
                && a.chars().all(|c| c.is_ascii_digit())
                && b.chars().all(|c| c.is_ascii_digit())
            {
                return Some(
                    word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '/')
                        .to_string(),
                );
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::perf::{PerfRecord, PerfTotals};

    fn fake_perf() -> PerfRecord {
        PerfRecord {
            build: 142,
            version: "0.1.0".into(),
            workflow: "prescriptive".into(),
            task_preview: "build a weather alerter".into(),
            started_at: "2026-04-24T17:00:00Z".into(),
            total_duration_ms: 46 * 60_000,
            phases: Vec::new(),
            totals: PerfTotals {
                cost_usd: 2.37,
                ..Default::default()
            },
            status: "success".into(),
            failed_phase: None,
            skills_used: Vec::new(),
            skills_considered: Vec::new(),
            tests_passed: None,
            tests_failed: None,
        }
    }

    #[test]
    fn runs_path_under_home() {
        // Only asserts the shape, not the actual HOME.
        if let Ok(p) = runs_path() {
            assert!(p.ends_with("sessions/runs.jsonl"));
        }
    }

    #[test]
    fn record_from_perf_extracts_fields() {
        let perf = fake_perf();
        let rec = record_from_perf(
            &perf,
            Path::new("/Users/x/proj"),
            Some("tasks/level-3.txt"),
            vec!["app.py".into()],
            Some("35/35".into()),
        );
        assert_eq!(rec.project_path, "/Users/x/proj");
        assert_eq!(rec.task_level.as_deref(), Some("L3"));
        assert_eq!(rec.duration_mins, 46);
        assert_eq!(rec.build_id, "build142");
        assert_eq!(rec.cost_usd, 2.37);
        assert_eq!(rec.score.as_deref(), Some("35/35"));
        assert_eq!(rec.files_modified, vec!["app.py".to_string()]);
    }

    #[tokio::test]
    async fn append_then_search_finds_record() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("runs.jsonl");
        let rec = record_from_perf(
            &fake_perf(),
            Path::new("/Users/x/weather"),
            Some("tasks/level-3.txt"),
            vec![],
            None,
        );
        append_to(&path, &rec).await.unwrap();
        let hits = search_in(&path, "weather").await.unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].project_path.contains("weather"));
        // Substring search on status also works.
        assert_eq!(search_in(&path, "success").await.unwrap().len(), 1);
        // Empty query returns all.
        assert_eq!(search_in(&path, "").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn search_missing_file_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nope.jsonl");
        assert!(search_in(&path, "anything").await.unwrap().is_empty());
    }

    #[test]
    fn extract_score_finds_score_line() {
        assert_eq!(
            extract_score("some text\nScore: 35/35\nmore"),
            Some("35/35".into())
        );
        assert_eq!(extract_score("Result: PASS\n..."), Some("PASS".into()));
        assert_eq!(extract_score("no score here"), None);
        // Fallback N/M pattern.
        assert_eq!(
            extract_score("rubric total: 25/25 achieved").as_deref(),
            Some("25/25")
        );
    }
}
