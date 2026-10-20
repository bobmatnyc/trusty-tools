//! The palace-lock stall verdict `tm doctor` gives the trusty-memory row (#8751).
//!
//! Why: `memory.health` reports the longest-held palace lock as
//! `worker.stalled_lock`, under the daemon's wedge line too. `tm doctor` read
//! `worker.wedged` only, so a lock held past the point where queued writers time
//! out left the row green with "workers progressing", while `trusty-memory
//! doctor` warned on the same payload.
//! What: [`lock_stall_threshold`] resolves the one stall line both doctors share
//! (`TRUSTY_DOCTOR_LOCK_STALL_SECS`, default the write-lock timeout, 60 s);
//! [`stalled_lock_check`] turns a health body's `stalled_lock` block into a
//! `Warn` naming the lock, palace, age and a remedy once it crosses that line,
//! and into `Unknown` when the block is present but unreadable.
//! Test: `doctor_lock_stall_tests.rs`, plus `memory_stalled_palace_lock_is_not_ok`
//! in `doctor_tests.rs`.

use std::time::Duration;

use crate::core::doctor::{CheckStatus, DoctorCheck};

/// Env var that moves the stall line, in whole seconds. The same variable
/// `trusty-memory doctor` reads — one setting, not two (#8751).
pub(super) const LOCK_STALL_ENV: &str = "TRUSTY_DOCTOR_LOCK_STALL_SECS";

/// Env var trusty-memory bounds a write-lock wait with; the stall line's default.
const WRITE_LOCK_ENV: &str = "TRUSTY_WRITE_LOCK_TIMEOUT_SECS";

/// trusty-memory's `DEFAULT_WRITE_LOCK_SECS`, mirrored because that crate is an
/// optional dependency of this one.
const DEFAULT_WRITE_LOCK_SECS: u64 = 60;

/// How long a palace lock may be held before the memory row warns.
///
/// Why: both doctors must draw the line in the same place, or `tm doctor` and
/// `trusty-memory doctor` disagree on one payload.
/// What: `TRUSTY_DOCTOR_LOCK_STALL_SECS` if set and parseable, else
/// `TRUSTY_WRITE_LOCK_TIMEOUT_SECS` if set and parseable, else 60 s — the same
/// resolution as `trusty-memory`'s `lock_stall_threshold`.
/// Test: `threshold_resolves_override_then_write_lock_then_default`.
pub(super) fn lock_stall_threshold() -> Duration {
    derive_threshold(|k| std::env::var(k).ok())
}

/// [`lock_stall_threshold`] over a supplied lookup, so a test reads no env.
fn derive_threshold(lookup: impl Fn(&str) -> Option<String>) -> Duration {
    let secs = |key: &str, trim: bool| {
        lookup(key).and_then(|v| if trim { v.trim() } else { v.as_str() }.parse::<u64>().ok())
    };
    let write_lock = secs(WRITE_LOCK_ENV, false).unwrap_or(DEFAULT_WRITE_LOCK_SECS);
    Duration::from_secs(secs(LOCK_STALL_ENV, true).unwrap_or(write_lock))
}

/// What an operator does about a lock held past the line.
///
/// Why: a call blocked inside the HNSW graph runs on a blocking thread no
/// timeout can cancel, so only a daemon restart clears it; a held handle mutex
/// may still be released by its holder, so it keeps the inspect-first advice.
/// What: for `hnsw`, the launchd bootout-then-bootstrap restart, never
/// `kickstart -k`; otherwise a thread-sample instruction.
/// Test: `an_hnsw_lock_names_the_restart_command`.
fn remedy(lock: &str) -> String {
    if lock == "hnsw" {
        let label = trusty_common::launchd_labels::MEMORY;
        return format!(
            "A call blocked inside the HNSW graph cannot be cancelled; recover with a daemon \
             restart: `launchctl bootout gui/$(id -u)/{label}`, wait for the old pid to exit, \
             then `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/{label}.plist` \
             (never `launchctl kickstart -k`)."
        );
    }
    "Run `trusty-memory doctor` for the same view and inspect the daemon with a thread \
     sample (`sample <pid>`) before restarting it."
        .to_string()
}

/// The non-`Ok` verdict for the daemon's longest-held palace lock, if it earns one.
///
/// Why: a held lock under the wedge line is invisible in the pass line, and a
/// block this client cannot read must not fall through to a pass either.
/// What: `None` when `body.worker.stalled_lock` is absent (the daemon found no
/// lock held) or its age is at or under `threshold`, leaving the caller's pass
/// line. `Warn` naming the lock kind, palace, age, threshold and a remedy when
/// the age is over it. `Unknown` when the field is present but its `palace`,
/// `lock` or `age_secs` cannot be read. A daemon that predates the field is
/// already `Unknown` upstream, through `stall_tracking_ok`.
/// Test: `a_lock_held_past_the_threshold_warns_naming_lock_palace_age_and_remedy`,
/// `a_lock_held_up_to_the_threshold_keeps_the_pass_line`,
/// `an_absent_stalled_lock_keeps_the_pass_line`,
/// `an_unreadable_stalled_lock_is_unknown`.
pub(super) fn stalled_lock_check(
    check: &str,
    service: &str,
    addr: &str,
    body: &serde_json::Value,
    threshold: Duration,
) -> Option<DoctorCheck> {
    // #8751: `Some(Null)` is present-but-unreadable, not absent.
    let stalled = body.get("worker")?.get("stalled_lock")?;
    let text = |key: &str| stalled.get(key).and_then(serde_json::Value::as_str);
    let age = stalled.get("age_secs").and_then(serde_json::Value::as_u64);
    let (Some(palace), Some(lock), Some(age)) = (text("palace"), text("lock"), age) else {
        return Some(DoctorCheck::new(
            check,
            CheckStatus::Unknown,
            format!(
                "{service} at {addr} reports a held palace lock `tm doctor` cannot read \
                 ({stalled}) — whether a palace write has stalled is UNKNOWN"
            ),
        ));
    };
    let limit = threshold.as_secs();
    (age > limit).then(|| {
        DoctorCheck::new(
            check,
            CheckStatus::Warn,
            format!(
                "{service} at {addr} has a STALLED palace lock: the {lock} lock of palace \
                 '{palace}' has been held for {age}s, past the {limit}s stall threshold \
                 ({LOCK_STALL_ENV}). Writers queued behind it time out. {}",
                remedy(lock)
            ),
        )
    })
}

#[cfg(test)]
#[path = "doctor_lock_stall_tests.rs"]
mod doctor_lock_stall_tests;
