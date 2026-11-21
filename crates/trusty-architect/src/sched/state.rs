//! The scheduler's one state file.
//!
//! Why: a restart must not rerun a slot that already ran, must keep the daily
//! cap and must remember misses; the contract keeps the scheduler out of the
//! IL (`meta_due.py` owns it), so the scheduler has its own file (R5).
//! What: [`SchedState`] holds, per job, the next due slot, last run, last
//! result, misses, overruns, today's run count and the failure-backoff
//! fields, plus the shadow poll job's next due time. [`SchedState::load`]
//! reads it (a missing file is a fresh state; a corrupt one is an error, not a
//! silent reset) and [`SchedState::save`] writes a temp file then renames it,
//! as the poller's state does.
//! Test: `state_round_trips_through_an_atomic_save`,
//! `corrupt_state_is_an_error_not_a_reset`, `empty_state_file_is_an_error_not_a_reset`, `layout_paths_follow_the_contract`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::SchedError;

/// One job's state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobState {
    /// The next slot (Unix seconds, before jitter).
    pub next_due: i64,
    /// The slot of the latest run or skip.
    #[serde(default)]
    pub last_slot: i64,
    /// Start of the last run.
    #[serde(default)]
    pub last_run: Option<i64>,
    /// `ok`, `fail`, `timeout`, `overrun` or `skipped`.
    #[serde(default)]
    pub last_result: Option<String>,
    /// Slots missed or found stale.
    #[serde(default)]
    pub misses: u32,
    /// Slots skipped because the previous run was alive.
    #[serde(default)]
    pub overruns: u32,
    /// The UTC day `runs_today` counts.
    #[serde(default)]
    pub day: String,
    /// Runs started on `day`.
    #[serde(default)]
    pub runs_today: u32,
    /// Failed or timed-out runs in a row.
    #[serde(default)]
    pub fail_streak: u32,
    /// No new run before this time (exponential backoff, at most 1 h).
    #[serde(default)]
    pub backoff_until: i64,
    /// An alert already went out for this streak or miss run.
    #[serde(default)]
    pub alerted: bool,
    /// The slot whose freshness was last checked.
    #[serde(default)]
    pub checked_slot: i64,
}

/// `sched-state.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchedState {
    /// Per job id.
    #[serde(default)]
    pub jobs: BTreeMap<String, JobState>,
    /// The poll job's next due time (Unix seconds).
    #[serde(default)]
    pub poll_next: i64,
}

impl SchedState {
    /// Reads the state at `path`; absent means a fresh state.
    ///
    /// # Errors
    /// [`SchedError::Io`] when the file exists but cannot be read,
    /// [`SchedError::Parse`] when it is not a state document.
    pub fn load(path: &Path) -> Result<Self, SchedError> {
        match std::fs::read_to_string(path) {
            // An empty file is a torn write, not a fresh start: defaulting
            // would reset runs_today, backoff, fail_streak, misses and poll_next.
            Ok(text) if text.trim().is_empty() => Err(SchedError::Parse {
                what: path.display().to_string(),
                detail: "the state file is empty".to_owned(),
            }),
            Ok(text) => serde_json::from_str(&text).map_err(|e| SchedError::Parse {
                what: path.display().to_string(),
                detail: e.to_string(),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(SchedError::io(format!("read {}", path.display()), e)),
        }
    }

    /// Writes the state atomically (temp file, then rename).
    ///
    /// # Errors
    /// [`SchedError::Io`] when the directory, temp file or rename fails.
    pub fn save(&self, path: &Path) -> Result<(), SchedError> {
        let text = serde_json::to_string_pretty(self).map_err(|e| SchedError::Parse {
            what: path.display().to_string(),
            detail: e.to_string(),
        })?;
        write_atomic(path, text.as_bytes())
    }
}

/// Writes `bytes` to `path` durably: temp file, `fsync`, rename, then `fsync`
/// of the parent directory, so a crash leaves the old or the new file whole.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), SchedError> {
    use std::io::Write;
    let io = |what: &str, e| SchedError::io(format!("{what} {}", path.display()), e);
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty());
    if let Some(dir) = dir {
        std::fs::create_dir_all(dir).map_err(|e| io("create dir for", e))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let mut file = std::fs::File::create(&tmp).map_err(|e| io("create", e))?;
    file.write_all(bytes).map_err(|e| io("write", e))?;
    file.sync_all().map_err(|e| io("sync", e))?;
    drop(file);
    std::fs::rename(&tmp, path).map_err(|e| io("rename onto", e))?;
    let parent = dir.unwrap_or_else(|| Path::new("."));
    std::fs::File::open(parent)
        .and_then(|d| d.sync_all())
        .map_err(|e| io("sync dir of", e))
}
