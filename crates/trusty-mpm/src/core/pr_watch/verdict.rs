//! The CI verdict on one head and the transition it makes (#11673 AC2, AC3).
//!
//! Why: a wake must name a real change — green or red on the same head, a new
//! push, or a rerun of a red — and never fire twice for one change. Keeping
//! the rule pure makes every transition testable against canned rollups.
//! What: [`judge`] reduces the required contexts to a [`Verdict`] through
//! [`deciding_runs`], so a superseded run never decides. [`detect`] compares
//! the stored (head, verdict) with the observed pair and names the
//! [`Kind`]. [`dedupe_key`] is the (repo, PR, head, kind) key.
//! Test: `judge_*` and `detect_*` in `tests.rs`.

use serde::{Deserialize, Serialize};

use crate::core::pr_rollup::{RollupEntry, deciding_per_check, deciding_runs};

/// The CI verdict on one head.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// A judged context is unfinished or has not registered.
    Pending,
    /// Every judged context settled and passed.
    Green,
    /// A judged context settled on a non-passing result.
    Red,
}

impl Verdict {
    /// The lowercase name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Green => "green",
            Self::Red => "red",
        }
    }
}

/// The transition a tick observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// The verdict changed to green on the same head.
    Green,
    /// The verdict changed to red on the same head.
    Red,
    /// The PR's head SHA changed.
    NewHead,
    /// Same head, the verdict went from red to pending.
    Rerun,
}

impl Kind {
    /// The snake_case name used in keys and events.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Green => "green",
            Self::Red => "red",
            Self::NewHead => "new_head",
            Self::Rerun => "rerun",
        }
    }
}

/// Judge `required` on `entries`.
///
/// Why: AC2 judges the required contexts only; a non-required check that
/// fails must not wake a PM whose merge it cannot block.
/// What: with `required` empty (an unprotected base), every deciding check is
/// judged, and an empty rollup is pending. Otherwise each name's deciding
/// runs: none registered is pending. Red wins over pending, since a settled
/// failure cannot turn green without a rerun; then pending; else green.
/// Test: `judge_reads_only_required_contexts`,
/// `judge_red_wins_over_pending`, `judge_missing_required_is_pending`,
/// `judge_uses_the_latest_run`.
pub fn judge(entries: &[RollupEntry], required: &[String]) -> Verdict {
    let runs: Vec<&RollupEntry> = if required.is_empty() {
        let all = deciding_per_check(entries);
        if all.is_empty() {
            return Verdict::Pending;
        }
        all
    } else {
        let mut runs = Vec::new();
        let mut missing = false;
        for name in required {
            let chosen = deciding_runs(entries, name);
            missing |= chosen.is_empty();
            runs.extend(chosen);
        }
        if missing && !runs.iter().any(|e| e.failed()) {
            return Verdict::Pending;
        }
        runs
    };
    if runs.iter().any(|e| e.failed()) {
        Verdict::Red
    } else if runs.iter().any(|e| e.is_unfinished()) {
        Verdict::Pending
    } else {
        Verdict::Green
    }
}

/// The transition from the stored (head, verdict) to the observed pair.
///
/// What: a changed head is [`Kind::NewHead`]. On the same head, a verdict
/// that becomes green or red is that kind (from `None` too, so a PR already
/// settled when added wakes once); red then pending is [`Kind::Rerun`].
/// Green then pending, and no change, are `None`.
/// Test: `detect_names_each_transition`, `detect_ignores_no_change`.
pub fn detect(
    stored_head: &str,
    stored: Option<Verdict>,
    head: &str,
    now: Verdict,
) -> Option<Kind> {
    if !head.eq_ignore_ascii_case(stored_head) {
        return Some(Kind::NewHead);
    }
    match (stored, now) {
        (s, Verdict::Green) if s != Some(Verdict::Green) => Some(Kind::Green),
        (s, Verdict::Red) if s != Some(Verdict::Red) => Some(Kind::Red),
        (Some(Verdict::Red), Verdict::Pending) => Some(Kind::Rerun),
        _ => None,
    }
}

/// The dedupe key: `<repo>#<pr>@<head>:<kind>`.
///
/// Test: `detect_names_each_transition`.
pub fn dedupe_key(repo: &str, pr: u64, head: &str, kind: Kind) -> String {
    format!(
        "{}#{pr}@{}:{}",
        repo.to_ascii_lowercase(),
        head.to_ascii_lowercase(),
        kind.as_str()
    )
}
