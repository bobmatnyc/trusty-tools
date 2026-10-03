//! Doctor's palace-lock stall threshold (#8751).
//!
//! Why: the daemon reports its longest-held palace lock (`worker.stalled_lock`)
//! under its wedge threshold too, but doctor only acted once the daemon called
//! the pool wedged — `2 × write_lock_timeout()`. A lock held past the point
//! where queued writers time out read as "workers progressing".
//! What: [`lock_stall_threshold`] is the doctor-side line, env-overridable like
//! [`crate::worker_liveness::wedge_threshold`]; [`stalled_lock_verdict`] turns a
//! `stalled_lock` block into a warning naming the palace and the age once it
//! crosses that line, and into "undetermined" when the block is unreadable.
//! Test: `lock_stall_tests`.

use std::time::Duration;

use super::CheckResult;

/// Env var that moves doctor's stall line, in whole seconds.
pub(super) const LOCK_STALL_ENV: &str = "TRUSTY_DOCTOR_LOCK_STALL_SECS";

/// How long a palace lock may be held before doctor warns (#8751).
///
/// Why: past `write_lock_timeout()` (default 60 s) a writer queued behind the
/// lock has already given up, so the stall is user-visible; the daemon's own
/// wedge line sits at twice that. Env-overridable so an operator running long
/// deliberate transactions can move it.
/// What: `TRUSTY_DOCTOR_LOCK_STALL_SECS` if set and parseable, else
/// `write_lock_timeout()`.
/// Test: `stall_threshold_defaults_to_the_write_lock_bound`.
pub(super) fn lock_stall_threshold() -> Duration {
    let override_secs = std::env::var(LOCK_STALL_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok());
    derive_lock_stall_threshold(
        override_secs,
        trusty_common::memory_core::timeouts::write_lock_timeout(),
    )
}

/// [`lock_stall_threshold`] with its inputs supplied, so a test reads no env.
pub(super) fn derive_lock_stall_threshold(
    override_secs: Option<u64>,
    write_lock: Duration,
) -> Duration {
    override_secs.map_or(write_lock, Duration::from_secs)
}

/// Verdict for the daemon's longest-held palace lock, if it earns one (#8751).
///
/// Why: a held lock under the wedge line is invisible in the pass line, and a
/// block doctor cannot read must not fall through to a pass either.
/// What: `None` when no lock is reported or its age is at or under
/// `threshold`, leaving the caller's pass line unchanged. `Warn` naming the
/// lock kind, palace and age when the age is over it. `Unknown` when the block
/// is present but its `palace`, `lock` or `age_secs` cannot be read.
/// Test: `a_lock_held_past_the_threshold_warns_with_palace_and_age`,
/// `a_lock_held_up_to_the_threshold_keeps_the_pass_line`,
/// `an_unreadable_stalled_lock_is_undetermined`.
pub(super) fn stalled_lock_verdict(
    label: &str,
    prefix: &str,
    stalled: Option<&serde_json::Value>,
    threshold: Duration,
) -> Option<CheckResult> {
    let stalled = stalled?;
    let palace = stalled.get("palace").and_then(serde_json::Value::as_str);
    let lock = stalled.get("lock").and_then(serde_json::Value::as_str);
    let age = stalled.get("age_secs").and_then(serde_json::Value::as_u64);
    let (Some(palace), Some(lock), Some(age)) = (palace, lock, age) else {
        return Some(CheckResult::unknown(
            label,
            format!(
                "{prefix}, but the daemon reports a held palace lock doctor cannot read \
                 ({stalled}). Whether a palace write has stalled is UNKNOWN."
            ),
        ));
    };
    let limit = threshold.as_secs();
    (age > limit).then(|| {
        CheckResult::warn(
            label,
            format!(
                "{prefix}, but the {lock} lock of palace '{palace}' has been held for {age}s, \
                 past the {limit}s stall threshold ({LOCK_STALL_ENV}). Writers queued behind \
                 it time out. Inspect with a thread sample before restarting."
            ),
        )
    })
}

#[cfg(test)]
#[path = "lock_stall_tests.rs"]
mod lock_stall_tests;
