//! Per-turn file-change budget for `tm hook --pm-guard` (Bob's directive,
//! 2026-07-17; see issue #2918).
//!
//! Why: prior to this, a PM direct source-code edit ([`super::pm_guard::SOURCE_EDIT_REASON`])
//! or shell-based file edit ([`super::pm_guard_bash::SHELL_EDIT_REASON`]) was an
//! absolute, always-deny prohibition — forcing a full Task/Agent delegation
//! round-trip even for a trivial one-line fix. Bob's directive relaxes this to
//! a budget: the PM may make up to [`DEFAULT_FILE_CHANGE_BUDGET`] such file
//! changes in a turn before the guard hard-blocks. A `PreToolUse` hook is
//! stateless and per-invocation — Claude Code's hook payload carries a
//! `session_id` but no `turn_id` (confirmed against the live hooks reference,
//! <https://code.claude.com/docs/en/hooks>, 2026-07-17: hooks do not expose a
//! turn/message boundary). Wiring a true turn-boundary signal would mean
//! registering a NEW `UserPromptSubmit` hook entry solely to reset a counter —
//! a much larger, riskier change to `session_launch::settings::write_project_hooks`
//! for this PR's scope. Per the task's own documented fallback, this module
//! instead uses a short-TTL counter file keyed by `session_id`: the count
//! resets once [`TURN_WINDOW_SECS`] elapses since the window started. This is
//! a heuristic approximation of "per turn" (a very slow-paced single turn could
//! see its budget silently reset mid-turn, and a rapid-fire sequence of
//! genuinely separate turns within the window shares one budget) — documented
//! here as the known limitation; a precise fix is tracked as follow-up
//! (register a `UserPromptSubmit` reset hook) if the heuristic proves
//! insufficient in practice.
//! What: [`record_file_change`] is the I/O-performing entry point `pm_guard`
//! calls once it has already decided a call WOULD be denied as a budget-
//! eligible file change; it loads (or initializes) the session's
//! [`BudgetState`] from `<FrameworkPaths::default().root>/state/pm_guard_turn_budget/<session>.json`,
//! runs the pure [`advance_budget`] core, persists the updated state, and
//! returns a [`BudgetDecision`]. Any I/O failure (unreadable/unwritable state
//! dir) fails OPEN — [`BudgetDecision::Allowed`] — consistent with the rest of
//! `pm_guard`'s fail-open posture; a budget that cannot be tracked must never
//! itself become a spurious hard-block.
//! Test: `advance_budget_*` (pure core), `record_file_change_*` (I/O
//! round-trip, using a temp dir).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use trusty_mpm::core::paths::FrameworkPaths;

/// Default per-turn file-change budget (Bob's directive: 3).
///
/// Why: no pm_guard-specific entry exists yet in the general
/// `TrustyToolsConfig`/`config_read` surface (issue #2918 tracks adding one if
/// operators need to tune this); per the task's own fallback this ships as a
/// hardcoded constant.
/// What: the number of budget-eligible file changes (source-code Edit/Write/
/// MultiEdit/NotebookEdit, or a shell-based file edit) allowed per turn-window
/// before [`advance_budget`] returns [`BudgetDecision::Exhausted`].
pub(crate) const DEFAULT_FILE_CHANGE_BUDGET: usize = 3;

/// Width of the turn-window heuristic, in seconds.
///
/// Why: see the module doc — this approximates "since the PM's current turn
/// began" in the absence of a true turn-boundary hook signal. Ten minutes
/// comfortably spans a burst of same-turn tool calls (which fire seconds
/// apart) while still resetting well within an operator's normal between-turn
/// pacing.
const TURN_WINDOW_SECS: u64 = 600;

/// Persisted per-session budget counter.
///
/// Why: must survive across the short-lived `tm hook --pm-guard` process
/// invocations (one per tool call), so it is a small JSON file, not in-memory
/// state.
/// What: `count` is the number of budget-eligible file changes recorded so far
/// in the current window; `window_started_at` is the unix-seconds timestamp
/// the window opened. [`advance_budget`] resets both when the window has
/// expired.
/// Test: covered via `advance_budget_*` and the round-trip
/// `record_file_change_*` tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct BudgetState {
    count: usize,
    window_started_at: u64,
}

/// Outcome of recording one budget-eligible file change.
///
/// Why: [`super::pm_guard::pm_guard`] needs to know both whether to allow the
/// call AND the exact count to splice into the budget-exhausted deny message
/// ("file-change budget 3/3 used this turn").
/// What: `Allowed { used, budget }` — the call is within budget, `used` is the
/// 1-based count AFTER recording this change; `Exhausted { budget }` — the
/// call is the `budget + 1`-th (or later) in the window and must be denied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BudgetDecision {
    Allowed { used: usize, budget: usize },
    Exhausted { budget: usize },
}

/// Pure core: advance a (possibly absent/expired) [`BudgetState`] by one
/// file-change and decide allow/exhaust.
///
/// Why: separated from all I/O so the turn-window and budget-boundary logic is
/// fully unit-testable without touching the filesystem.
/// What: `prev == None`, or `prev` whose `window_started_at` is more than
/// [`TURN_WINDOW_SECS`] before `now`, starts a fresh window at count 1
/// (`Allowed { used: 1, .. }`). Otherwise increments `prev.count`; a result
/// `<= budget` is `Allowed`, a result `> budget` is `Exhausted` and the state
/// is NOT incremented past `budget` (so a sustained stream of denied calls
/// doesn't overflow `count` — it saturates at `budget`, keeping the persisted
/// state small and the decision idempotent for repeated denies in the same
/// window).
/// Test: `advance_budget_starts_fresh_window`,
/// `advance_budget_allows_up_to_budget`,
/// `advance_budget_denies_past_budget`,
/// `advance_budget_resets_after_window_expiry`,
/// `advance_budget_saturates_count_on_repeated_denial`.
fn advance_budget(
    prev: Option<BudgetState>,
    now: u64,
    budget: usize,
) -> (BudgetState, BudgetDecision) {
    let window_live =
        prev.is_some_and(|s| now.saturating_sub(s.window_started_at) < TURN_WINDOW_SECS);
    let base = if window_live {
        prev.expect("window_live implies prev is Some")
    } else {
        BudgetState {
            count: 0,
            window_started_at: now,
        }
    };
    let next_count = base.count + 1;
    if next_count <= budget {
        let state = BudgetState {
            count: next_count,
            window_started_at: base.window_started_at,
        };
        (
            state,
            BudgetDecision::Allowed {
                used: next_count,
                budget,
            },
        )
    } else {
        // Saturate: don't let count grow unbounded across repeated denials.
        let state = BudgetState {
            count: budget,
            window_started_at: base.window_started_at,
        };
        (state, BudgetDecision::Exhausted { budget })
    }
}

/// Sanitize a `session_id` into a safe filename component.
///
/// Why: `session_id` is attacker-influenced-in-principle payload data (it
/// comes from the `PreToolUse` stdin JSON); it must never be used to construct
/// a path that escapes the state directory.
/// What: keeps ASCII alphanumerics, `-`, and `_`; replaces every other byte
/// with `_`. An empty input becomes `"_default"` so the file is still
/// resolvable (rather than colliding with the directory itself).
/// Test: `sanitize_session_id_*`.
fn sanitize_session_id(session_id: &str) -> String {
    if session_id.is_empty() {
        return "_default".to_string();
    }
    session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The state directory budget counter files live under.
///
/// What: `<FrameworkPaths::default().root>/state/pm_guard_turn_budget/`.
fn state_dir() -> PathBuf {
    FrameworkPaths::default()
        .root
        .join("state/pm_guard_turn_budget")
}

/// Load, advance, and persist the budget counter for `session_id` — the I/O
/// entry point `pm_guard` calls for each budget-eligible file-change denial.
///
/// Why: see the module doc. Fails OPEN on any I/O error — a budget that
/// cannot be durably tracked must never itself become a spurious hard-block;
/// the underlying deny rule (source-code edit / shell edit) already fired
/// before this is consulted, so failing open here means "allow this one
/// change", not "allow everything".
/// What: reads `<state_dir>/<sanitized session_id>.json` (missing/corrupt ⇒
/// `None`, i.e. start fresh), calls [`advance_budget`] with the current unix
/// time, writes the updated state back, and returns the decision. A
/// directory-creation or write failure still returns the computed
/// [`BudgetDecision`] (the in-memory decision is correct even if it could not
/// be persisted — the fail-open direction is "the persisted count resets",
/// not "silently deny").
/// Test: `record_file_change_allows_first_three_then_exhausts`,
/// `record_file_change_isolates_by_session`.
pub(crate) fn record_file_change(session_id: &str, budget: usize) -> BudgetDecision {
    record_file_change_at(&state_dir(), session_id, budget, now_unix())
}

/// Testable core of [`record_file_change`] with the state dir and clock
/// injected.
fn record_file_change_at(dir: &Path, session_id: &str, budget: usize, now: u64) -> BudgetDecision {
    let file = dir.join(format!("{}.json", sanitize_session_id(session_id)));
    let prev = std::fs::read_to_string(&file)
        .ok()
        .and_then(|s| serde_json::from_str::<BudgetState>(&s).ok());
    let (state, decision) = advance_budget(prev, now, budget);
    if std::fs::create_dir_all(dir).is_ok()
        && let Ok(json) = serde_json::to_string(&state)
    {
        let _ = std::fs::write(&file, json);
    }
    decision
}

/// Current unix time in seconds, saturating to 0 on a pre-epoch clock (never
/// realistically hit — a defensive fallback only).
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advance_budget_starts_fresh_window() {
        let (state, decision) = advance_budget(None, 1000, 3);
        assert_eq!(
            state,
            BudgetState {
                count: 1,
                window_started_at: 1000
            }
        );
        assert_eq!(decision, BudgetDecision::Allowed { used: 1, budget: 3 });
    }

    #[test]
    fn advance_budget_allows_up_to_budget() {
        let mut prev = None;
        for expected_used in 1..=3 {
            let (state, decision) = advance_budget(prev, 1000, 3);
            assert_eq!(
                decision,
                BudgetDecision::Allowed {
                    used: expected_used,
                    budget: 3
                }
            );
            prev = Some(state);
        }
    }

    #[test]
    fn advance_budget_denies_past_budget() {
        let mut prev = None;
        for _ in 1..=3 {
            let (state, _) = advance_budget(prev, 1000, 3);
            prev = Some(state);
        }
        let (state, decision) = advance_budget(prev, 1000, 3);
        assert_eq!(decision, BudgetDecision::Exhausted { budget: 3 });
        // Saturates at budget, doesn't keep growing.
        assert_eq!(state.count, 3);
    }

    #[test]
    fn advance_budget_resets_after_window_expiry() {
        let prev = Some(BudgetState {
            count: 3,
            window_started_at: 1000,
        });
        // Exactly at the boundary is still "expired" (>= window, not <).
        let (state, decision) = advance_budget(prev, 1000 + TURN_WINDOW_SECS, 3);
        assert_eq!(decision, BudgetDecision::Allowed { used: 1, budget: 3 });
        assert_eq!(state.count, 1);
    }

    #[test]
    fn advance_budget_saturates_count_on_repeated_denial() {
        let prev = Some(BudgetState {
            count: 3,
            window_started_at: 1000,
        });
        let (state1, d1) = advance_budget(prev, 1005, 3);
        let (state2, d2) = advance_budget(Some(state1), 1010, 3);
        assert_eq!(d1, BudgetDecision::Exhausted { budget: 3 });
        assert_eq!(d2, BudgetDecision::Exhausted { budget: 3 });
        assert_eq!(state1.count, 3);
        assert_eq!(state2.count, 3);
    }

    #[test]
    fn sanitize_session_id_replaces_unsafe_bytes() {
        assert_eq!(sanitize_session_id("abc-123_XYZ"), "abc-123_XYZ");
        assert_eq!(sanitize_session_id("../../etc/passwd"), "______etc_passwd");
        assert_eq!(sanitize_session_id(""), "_default");
    }

    #[test]
    fn record_file_change_allows_first_three_then_exhausts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let now = 5000;
        for expected_used in 1..=3 {
            let decision = record_file_change_at(dir.path(), "sess-a", 3, now);
            assert_eq!(
                decision,
                BudgetDecision::Allowed {
                    used: expected_used,
                    budget: 3
                }
            );
        }
        let decision = record_file_change_at(dir.path(), "sess-a", 3, now);
        assert_eq!(decision, BudgetDecision::Exhausted { budget: 3 });
    }

    #[test]
    fn record_file_change_isolates_by_session() {
        let dir = tempfile::tempdir().expect("tempdir");
        let now = 5000;
        for _ in 1..=3 {
            record_file_change_at(dir.path(), "sess-a", 3, now);
        }
        // A different session has its own independent budget.
        let decision = record_file_change_at(dir.path(), "sess-b", 3, now);
        assert_eq!(decision, BudgetDecision::Allowed { used: 1, budget: 3 });
    }
}
