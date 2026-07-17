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
//! insufficient in practice. This window heuristic is deliberately left
//! UNCHANGED pending Bob's sign-off — see #2918.
//! What: [`record_file_change`] is the I/O-performing entry point `pm_guard`
//! calls once it has already decided a call WOULD be denied as a budget-
//! eligible file change; it loads (or initializes) the session's
//! [`BudgetState`] from `<FrameworkPaths::default().root>/state/pm_guard_turn_budget/<session>.json`,
//! runs the pure [`advance_budget`] core, persists the updated state, and
//! returns a [`BudgetDecision`].
//!
//! **Storage-failure posture (fails toward DENY, not allow — code-critic
//! finding on PR #2928):** an earlier version of this module failed OPEN on
//! any persist error, silently returning `Allowed` forever — that is a NEW
//! fail-open vector for P1/P5: a state dir that is unwritable (permissions,
//! disk full, `rm -rf`'d mid-session by an ALLOWED command) meant every call
//! re-read a missing file, saw `prev = None`, and unconditionally allowed —
//! unlimited silent source-code/shell edits with zero telemetry. Persisting
//! now goes through [`persist_and_verify`], which writes AND reads the file
//! back to confirm the write actually landed (catches a write that silently
//! no-ops, e.g. a read-only bind mount) before trusting the decision; any
//! failure in that path is logged via `tracing::warn!` and the call is
//! downgraded to [`BudgetDecision::Exhausted`] regardless of what
//! [`advance_budget`] computed — i.e. storage trouble reverts to the
//! pre-#2918 unconditional-deny behavior, never to unconditional-allow. This
//! only affects the rare storage-failure branch; the common path (storage
//! healthy) is unaffected.
//!
//! **Concurrency:** two `tm hook --pm-guard` processes for the SAME session
//! can fire close together (parallel tool calls in one turn). The
//! read-advance-write sequence is protected by [`with_session_lock`], a
//! bounded-retry `O_EXCL` lockfile (`<file>.lock`) so concurrent
//! read-modify-write cycles serialize instead of racing a lost update (which
//! would let more than the budget through). Lock acquisition is capped well
//! under the hook's 10s `PreToolUse` timeout; on a timeout (e.g. a stale lock
//! from a crashed process older than [`STALE_LOCK_SECS`] is force-cleared
//! first, so this should only fire under genuine contention or a wedged
//! holder) the call ALSO fails toward `Exhausted`, consistent with the
//! storage-failure posture above.
//! Test: `advance_budget_*` (pure core), `record_file_change_*` (I/O
//! round-trip, using a temp dir), `persist_and_verify_*`,
//! `verify_roundtrip_*`, `with_session_lock_*`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
/// pacing. Left unchanged pending Bob's sign-off (see module doc).
const TURN_WINDOW_SECS: u64 = 600;

/// Total wall-clock budget for acquiring a session's lockfile, in
/// milliseconds.
///
/// Why: `tm hook --pm-guard` runs under a 10s `PreToolUse` timeout
/// (`session_launch::settings::pm_guard_hook_value`); this must stay a small
/// fraction of that so lock contention can never itself wedge a tool call.
const LOCK_ACQUIRE_BUDGET_MS: u64 = 200;

/// Sleep between lock-acquisition retries, in milliseconds.
const LOCK_RETRY_SLEEP_MS: u64 = 5;

/// A lockfile older than this is treated as abandoned (left behind by a
/// crashed/killed `tm hook --pm-guard` process, which — being a short-lived
/// per-invocation process — can never itself release the lock) and is force-
/// removed before the next acquisition attempt.
const STALE_LOCK_SECS: u64 = 5;

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
/// Why: thin wrapper that supplies the real state dir and clock to
/// [`record_file_change_at`] — see that function's doc, and the module doc's
/// storage-failure/concurrency sections, for the actual fail-toward-deny
/// posture (code-critic PR #2928 HIGH finding).
/// What: delegates to [`record_file_change_at`] with [`state_dir`] and
/// [`now_unix`].
/// Test: `record_file_change_allows_first_three_then_exhausts`,
/// `record_file_change_isolates_by_session`,
/// `record_file_change_denies_on_persist_failure`.
pub(crate) fn record_file_change(session_id: &str, budget: usize) -> BudgetDecision {
    record_file_change_at(&state_dir(), session_id, budget, now_unix())
}

/// Testable core of [`record_file_change`] with the state dir and clock
/// injected.
///
/// Why/What: see the module doc's storage-failure and concurrency sections —
/// this is where both are enforced. Acquires [`with_session_lock`] around the
/// full read-advance-persist cycle; on a lock timeout OR a
/// [`persist_and_verify`] failure, logs via `tracing::warn!` and returns
/// [`BudgetDecision::Exhausted`] regardless of what [`advance_budget`]
/// computed — storage/lock trouble fails toward deny, never toward the
/// unconditional-allow the pre-fix version risked.
/// Test: `record_file_change_allows_first_three_then_exhausts`,
/// `record_file_change_isolates_by_session`,
/// `record_file_change_denies_on_persist_failure`.
fn record_file_change_at(dir: &Path, session_id: &str, budget: usize, now: u64) -> BudgetDecision {
    let sanitized = sanitize_session_id(session_id);
    let file = dir.join(format!("{sanitized}.json"));
    let lock_path = dir.join(format!("{sanitized}.lock"));

    let Some(_guard) = with_session_lock(&lock_path) else {
        tracing::warn!(
            session_id,
            "pm_guard_budget: could not acquire the turn-budget lockfile within \
             {LOCK_ACQUIRE_BUDGET_MS}ms; failing toward deny (treating as \
             budget-exhausted) rather than racing an unprotected read-modify-write"
        );
        return BudgetDecision::Exhausted { budget };
    };

    let prev = std::fs::read_to_string(&file)
        .ok()
        .and_then(|s| serde_json::from_str::<BudgetState>(&s).ok());
    let (state, decision) = advance_budget(prev, now, budget);

    if let Err(err) = persist_and_verify(dir, &file, &state) {
        tracing::warn!(
            session_id,
            error = %err,
            "pm_guard_budget: failed to persist/verify turn-budget state; \
             failing toward deny (treating as budget-exhausted) rather than \
             silently allowing unlimited file changes"
        );
        return BudgetDecision::Exhausted { budget };
    }
    decision
}

/// Persist `state` to `file` and read it back to confirm the write actually
/// landed durably.
///
/// Why: a bare `fs::write` that silently no-ops (a read-only bind mount, a
/// filesystem that accepts the write call but doesn't commit it) would leave
/// [`record_file_change_at`] believing storage is healthy when it is not —
/// the write-verify round-trip is what lets the caller detect that and fail
/// toward deny instead.
/// What: `create_dir_all(dir)`, serialize `state`, write `file`, read `file`
/// back, and delegate the round-trip comparison to [`verify_roundtrip`]. Every
/// step's error is folded into a single `Err(String)` the caller logs and
/// treats as a persist failure.
/// Test: `persist_and_verify_round_trips`.
fn persist_and_verify(dir: &Path, file: &Path, state: &BudgetState) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create_dir_all({}): {e}", dir.display()))?;
    let json = serde_json::to_string(state).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(file, &json).map_err(|e| format!("write({}): {e}", file.display()))?;
    let readback =
        std::fs::read_to_string(file).map_err(|e| format!("read-back({}): {e}", file.display()))?;
    verify_roundtrip(state, &readback)
}

/// Pure comparison core of [`persist_and_verify`]'s read-back check —
/// separated out so the mismatch/parse-failure branches are unit-testable
/// without needing to provoke a real filesystem race.
///
/// What: parses `readback_json`; `Err` on a parse failure, or when the parsed
/// value differs from `written` (proving the persisted bytes do not represent
/// what was just written — a silently-dropped or corrupted write).
/// Test: `verify_roundtrip_ok_on_match`, `verify_roundtrip_detects_mismatch`,
/// `verify_roundtrip_detects_parse_failure`.
fn verify_roundtrip(written: &BudgetState, readback_json: &str) -> Result<(), String> {
    let parsed: BudgetState =
        serde_json::from_str(readback_json).map_err(|e| format!("read-back parse: {e}"))?;
    if parsed != *written {
        return Err(format!(
            "read-back mismatch: wrote {written:?}, read {parsed:?}"
        ));
    }
    Ok(())
}

/// RAII guard for a held session lockfile — [`Drop`] removes it, releasing
/// the lock even on an early return.
struct SessionLock {
    path: PathBuf,
}

impl Drop for SessionLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Acquire an advisory `O_EXCL` lockfile at `lock_path`, bounded by
/// [`LOCK_ACQUIRE_BUDGET_MS`].
///
/// Why (MEDIUM, code-critic PR #2928): two `tm hook --pm-guard` processes for
/// the same session firing close together would otherwise race the
/// read-advance-write cycle in [`record_file_change_at`], a lost update
/// letting more than [`DEFAULT_FILE_CHANGE_BUDGET`] changes through in one
/// window. `O_EXCL`-create (`create_new(true)`) needs no extra crate — the
/// file's very existence IS the lock.
/// What: loops attempting `OpenOptions::new().write(true).create_new(true)`
/// on `lock_path`; on `AlreadyExists`, force-clears the lock file first if its
/// mtime is older than [`STALE_LOCK_SECS`] (a `tm hook --pm-guard` process is
/// short-lived and per-invocation — it can never itself hold a lock past its
/// own exit, so a stale file only means a prior holder crashed/was killed
/// mid-critical-section), then sleeps [`LOCK_RETRY_SLEEP_MS`] and retries.
/// Gives up once the elapsed time exceeds [`LOCK_ACQUIRE_BUDGET_MS`], returning
/// `None`. A directory-creation failure (parent dir missing/unwritable) also
/// yields `None` immediately rather than retrying.
/// Test: `with_session_lock_acquires_when_free`,
/// `with_session_lock_serializes_against_a_held_lock`,
/// `with_session_lock_clears_a_stale_lock`.
fn with_session_lock(lock_path: &Path) -> Option<SessionLock> {
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    let deadline = Instant::now() + Duration::from_millis(LOCK_ACQUIRE_BUDGET_MS);
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)
        {
            Ok(_) => {
                return Some(SessionLock {
                    path: lock_path.to_path_buf(),
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                clear_if_stale(lock_path);
            }
            Err(_) => return None,
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(LOCK_RETRY_SLEEP_MS));
    }
}

/// Force-remove `lock_path` if its last-modified time is older than
/// [`STALE_LOCK_SECS`] — see [`with_session_lock`]'s doc for why this is safe
/// (a `tm hook --pm-guard` process cannot outlive its own single invocation).
fn clear_if_stale(lock_path: &Path) {
    let Ok(metadata) = std::fs::metadata(lock_path) else {
        return;
    };
    let Ok(modified) = metadata.modified() else {
        return;
    };
    let age = SystemTime::now()
        .duration_since(modified)
        .unwrap_or(Duration::ZERO);
    if age >= Duration::from_secs(STALE_LOCK_SECS) {
        let _ = std::fs::remove_file(lock_path);
    }
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
mod tests;
