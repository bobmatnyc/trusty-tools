//! The cross-invocation timeout budget for `tm wait` (#5843).
//!
//! Why: one `tm wait` invocation must return before the harness ceiling, but
//! the HARD timeout it enforces has to span every re-run — otherwise an agent
//! re-issuing the command resets its own deadline and the wait never expires.
//! The budget therefore lives in a small file keyed by the condition, not in
//! process memory.
//! What: [`Budget`] is the record (`started_at` / `updated_at` / `polls` /
//! `timeout_s`); [`path_for`] derives its file name from the condition spec;
//! [`load_or_start`] resumes an existing budget, starts a fresh one, or
//! ABANDONS a stale one; [`save`] and [`clear`] persist and remove it. Every
//! function takes `now` so the tests never depend on the wall clock.
//! Test: `budget_*` in the sibling `tests.rs`.

use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

/// A condition's timeout budget, shared across re-runs.
///
/// Why: `remaining()` is what the "pending" status line reports, and it is the
/// only reason an agent can tell a wait that is progressing from one that is
/// about to expire.
/// What: when the wait began, when it was last polled, how many polls have
/// happened, and the hard timeout in force.
/// Test: `budget_resumes_across_invocations`, `budget_expires_at_the_deadline`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Budget {
    /// The canonical condition spec this budget belongs to.
    pub(crate) spec: String,
    /// Unix seconds at which the first invocation started waiting.
    pub(crate) started_at: u64,
    /// Unix seconds of the most recent poll.
    pub(crate) updated_at: u64,
    /// Total polls across every invocation.
    pub(crate) polls: u64,
    /// The hard timeout in force, in seconds.
    pub(crate) timeout_s: u64,
}

impl Budget {
    /// Seconds spent waiting, across every invocation.
    pub(crate) fn elapsed(&self, now: u64) -> u64 {
        now.saturating_sub(self.started_at)
    }

    /// Seconds of budget left; zero once the hard timeout is exhausted.
    pub(crate) fn remaining(&self, now: u64) -> u64 {
        self.timeout_s.saturating_sub(self.elapsed(now))
    }

    /// Whether the hard timeout has been exhausted.
    pub(crate) fn expired(&self, now: u64) -> bool {
        self.remaining(now) == 0
    }
}

/// Default directory holding budget files.
///
/// Why: a temp dir is the right lifetime — a budget is meaningless once the
/// machine reboots, and nothing here is worth backing up.
/// What: `$TMPDIR/tm-wait`.
/// Test: `path_for_is_stable_and_discriminating`.
pub(crate) fn default_state_dir() -> PathBuf {
    std::env::temp_dir().join("tm-wait")
}

/// Derive the budget file path for a condition spec.
///
/// Why: two invocations naming the same condition must land on the same file,
/// and a spec can contain path separators and spaces that cannot go in a file
/// name. Hashing alone would make the directory unreadable, so the name keeps a
/// sanitised, truncated prefix AND a hash of the FULL spec — truncation can
/// collide, the hash cannot be read, and together they are both legible and
/// discriminating.
/// What: `<dir>/<sanitised spec, ≤64 chars>-<16 hex of hash>.json`.
/// Test: `path_for_is_stable_and_discriminating`.
pub(crate) fn path_for(dir: &Path, spec: &str) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    spec.hash(&mut hasher);
    let digest = hasher.finish();

    let sanitised: String = spec
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .take(64)
        .collect();
    dir.join(format!("{sanitised}-{digest:016x}.json"))
}

/// Resume, start, or abandon-and-restart the budget for a condition.
///
/// Why: three cases have to be told apart. A prompt re-run RESUMES (that is the
/// whole point of the file). A changed `--timeout` RESTARTS, because the agent
/// changed the terms of the wait. And a budget nobody has touched for a long
/// time is ABANDONED — otherwise a leftover file from yesterday's identical
/// wait would make today's first invocation report an instant timeout.
/// What: reads `path`; returns the stored budget when its spec and timeout
/// match and it was updated within `abandon_after` seconds, else a fresh one.
/// An unreadable or corrupt file is treated as absent, never as an error — the
/// budget is an optimisation for the deadline, not a source of truth to fail on.
/// Test: `budget_resumes_across_invocations`, `budget_restarts_on_timeout_change`,
/// `budget_abandons_a_stale_record`, `budget_ignores_corrupt_file`.
pub(crate) fn load_or_start(
    path: &Path,
    spec: &str,
    timeout_s: u64,
    now: u64,
    abandon_after: u64,
) -> Budget {
    let fresh = || Budget {
        spec: spec.to_string(),
        started_at: now,
        updated_at: now,
        polls: 0,
        timeout_s,
    };

    let Ok(raw) = std::fs::read_to_string(path) else {
        return fresh();
    };
    let Ok(stored) = serde_json::from_str::<Budget>(&raw) else {
        return fresh();
    };
    if stored.spec != spec || stored.timeout_s != timeout_s {
        return fresh();
    }
    if now.saturating_sub(stored.updated_at) > abandon_after {
        return fresh();
    }
    // A clock that moved backwards would make `elapsed` wrap to a huge number
    // and fake a timeout; clamp instead.
    if stored.started_at > now {
        return fresh();
    }
    stored
}

/// Persist a budget, creating the state directory when needed.
///
/// Why: the next invocation can only resume what this one wrote.
/// What: creates the parent directory, then writes the record as JSON.
/// Test: `budget_resumes_across_invocations`.
pub(crate) fn save(path: &Path, budget: &Budget) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let json = serde_json::to_string(budget).context("cannot serialise wait budget")?;
    std::fs::write(path, json).with_context(|| format!("cannot write {}", path.display()))
}

/// Remove a budget file once the wait reaches a terminal outcome.
///
/// Why: leaving it behind means the next identical wait inherits an exhausted
/// deadline. Best-effort on purpose — a wait that succeeded must not fail
/// because a temp file could not be unlinked.
/// What: deletes `path`, ignoring any error.
/// Test: `budget_clear_removes_the_file`.
pub(crate) fn clear(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Current unix time in seconds.
///
/// Why: one conversion point, so every caller agrees on the epoch.
/// What: seconds since the unix epoch; 0 if the clock predates it.
/// Test: covered indirectly by the live invocation.
pub(crate) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
