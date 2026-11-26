//! The cross-invocation timeout budget for `tm wait` (#5843).
//!
//! Why: one `tm wait` invocation must return before the harness ceiling, but
//! the HARD timeout it enforces has to span every re-run — otherwise an agent
//! re-issuing the command resets its own deadline and the wait never expires.
//! The budget therefore lives in a small file, not in process memory.
//!
//! THE KEY (#10668): a budget belongs to the condition spec PLUS, for
//! `--for check`, the PR head SHA the polls observe. A re-run on the same head
//! keeps the budget; the first poll that sees a different head starts a fresh
//! one. Keying by spec alone let a wait on an earlier head of the same PR hand
//! its `started_at` to the wait on the next push, which reported
//! `status=timeout` minutes into the new CI run. The head is not known until
//! the first poll answers, so the file name stays spec-only and the head is a
//! field of the record, checked by [`Budget::observe_head`].
//! What: [`Budget`] is the record (`started_at` / `updated_at` / `polls` /
//! `timeout_s` / `head`); [`path_for`] derives its file name from the condition
//! spec; [`load_or_start`] resumes an existing budget, starts a fresh one, or
//! ABANDONS a stale one; [`Budget::observe_head`] restarts it on a new head;
//! [`save`] and [`clear`] persist and remove it. Every function takes `now` so
//! the tests never depend on the wall clock.
//! Test: `budget_*` in the sibling `tests.rs`; the head key in
//! `rerun_elapsed_tracks_wall_time_across_a_push`,
//! `rerun_on_a_new_head_starts_a_fresh_budget`,
//! `rerun_on_the_same_head_keeps_its_budget`.

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
/// happened, the hard timeout in force, and the current run of consecutive
/// rate-limit refusals.
/// Test: `budget_resumes_across_invocations` proves `remaining` survives a
/// re-run; `loop_returns_timeout_when_budget_exhausted` proves `expired` ends
/// the wait once the budget reaches zero;
/// `rate_limit_streak_spans_slices_and_errors_on_the_fifth_refusal` and
/// `budget_resumes_a_state_file_written_before_the_rate_limit_streak` cover
/// the streak.
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
    /// Consecutive gh shim rate-limit refusals, across every invocation.
    ///
    /// #10550: persisted so a refusal streak spread over several slices still
    /// ends the wait on the fifth refusal; `default` reads an older state file.
    #[serde(default)]
    pub(crate) rate_limited_streak: u32,
    /// The PR head SHA this budget was spent on; `None` until a poll reports
    /// one, and always `None` for `run`/`file`.
    ///
    /// #10668: half of the budget key; see the module doc.
    #[serde(default)]
    pub(crate) head: Option<String>,
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

    /// Record the head SHA a successful poll observed, restarting the budget
    /// when it differs from the head the budget was spent on.
    ///
    /// Why: #10668 — a new head is a new CI run, so the wait on it gets the
    /// whole `--timeout`. A re-run on the same head must keep the budget, or the
    /// deadline never expires.
    /// What: `None` (the poll reported no head, or the verb has none) changes
    /// nothing. The first head a budget sees is adopted without a restart, so
    /// time already spent on this wait (errored polls included) still counts.
    /// A head that differs from the recorded one restarts the budget at `at`
    /// (the start of the poll that saw it): `started_at = updated_at = at`,
    /// `polls = 1` for that poll, and the rate-limit streak cleared. Returns
    /// whether it restarted.
    /// Test: `rerun_on_a_new_head_starts_a_fresh_budget`,
    /// `rerun_on_the_same_head_keeps_its_budget`,
    /// `rerun_elapsed_tracks_wall_time_across_a_push`.
    pub(crate) fn observe_head(&mut self, head: Option<&str>, at: u64) -> bool {
        let Some(head) = head else {
            return false;
        };
        match self.head.as_deref() {
            Some(recorded) if recorded == head => false,
            None => {
                self.head = Some(head.to_string());
                false
            }
            Some(_) => {
                self.head = Some(head.to_string());
                self.started_at = at;
                self.updated_at = at;
                self.polls = 1;
                self.rate_limited_streak = 0;
                true
            }
        }
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
/// The stored `head` comes back with it; this function cannot know the head
/// yet, so the head half of the key is checked after the first poll by
/// [`Budget::observe_head`] (#10668).
/// An unreadable or corrupt file is treated as absent, never as an error: the
/// wait starts a fresh budget, so it still ends at `timeout_s` from now — never
/// a pass (only an `Ok` poll is `met`) and never an endless wait.
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
        rate_limited_streak: 0,
        head: None,
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
