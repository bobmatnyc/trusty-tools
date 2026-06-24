//! Session admission control: concurrency cap + launch stagger (§9.2, WI-5).
//!
//! Why: concurrent Max-OAuth sessions share one rate bucket (§9.2), so the
//! daemon must (a) refuse to launch beyond `max_concurrent_sessions` and
//! (b) space successive launches by `launch_stagger_ms` to avoid a thundering
//! herd. Isolating the *decision* (admit vs. reject) into a pure function keeps
//! it deterministically unit-testable — no clock, no sleep, no registry lock —
//! while the registry owns the actual sleep and lock.
//! What: [`AdmissionDecision`] is the outcome of an admission check;
//! [`check_admission`] is the pure predicate (current live count vs. cap);
//! [`AdmissionError`] is the typed rejection surfaced to callers.
//! Test: `admission_*` in the inline `tests` module.

use thiserror::Error;

/// Outcome of an admission check (§9.2).
///
/// Why: callers need to distinguish "go ahead, after this stagger delay" from
/// "rejected, cap reached" without re-deriving the rule. Carrying the stagger
/// in the `Admit` variant means the registry never has to re-read config to
/// know how long to sleep.
/// What: `Admit { stagger_ms }` permits the launch after an optional delay;
/// `Reject { live, cap }` denies it because the cap is reached.
/// Test: `admission_under_cap_admits`, `admission_at_cap_rejects`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionDecision {
    /// The launch is permitted; sleep `stagger_ms` before spawning.
    Admit {
        /// Milliseconds to wait before the actual spawn (§9.2 stagger).
        stagger_ms: u64,
    },
    /// The launch is denied because the concurrency cap is reached.
    Reject {
        /// Number of live sessions at decision time.
        live: u32,
        /// The configured cap.
        cap: u32,
    },
}

/// Typed rejection returned when a launch exceeds the concurrency cap (§9.2).
///
/// Why: library code uses `thiserror` (per CLAUDE.md); a structured error lets
/// the daemon's HTTP layer map the cap rejection to a clear 429-style response
/// and lets tests match on the variant rather than a string.
/// What: a single `CapReached` variant carrying the live count and the cap.
/// Test: `admission_error_display`.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AdmissionError {
    /// The maximum number of concurrent sessions has been reached.
    #[error(
        "session cap reached: {live} live sessions, max_concurrent_sessions = {cap} (§9.2); \
         stop a session or raise the cap before launching"
    )]
    CapReached {
        /// Number of live sessions at decision time.
        live: u32,
        /// The configured cap.
        cap: u32,
    },
}

/// Decide whether a new session may launch given the current live count (§9.2).
///
/// Why: this is the single, pure decision point for the concurrency cap. Keeping
/// it free of locks and clocks makes the cap behavior exhaustively testable and
/// keeps the registry's locked critical section trivial.
/// What: returns [`AdmissionDecision::Admit`] (carrying `stagger_ms`) when
/// `live < cap`, otherwise [`AdmissionDecision::Reject`]. A `cap` of `0` rejects
/// every launch (matches the documented `ControlPlaneConfig` semantics).
/// Test: `admission_under_cap_admits`, `admission_at_cap_rejects`,
/// `admission_zero_cap_always_rejects`.
pub fn check_admission(live: u32, cap: u32, stagger_ms: u64) -> AdmissionDecision {
    if live < cap {
        AdmissionDecision::Admit { stagger_ms }
    } else {
        AdmissionDecision::Reject { live, cap }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_under_cap_admits() {
        assert_eq!(
            check_admission(0, 5, 2_000),
            AdmissionDecision::Admit { stagger_ms: 2_000 }
        );
        assert_eq!(
            check_admission(4, 5, 0),
            AdmissionDecision::Admit { stagger_ms: 0 }
        );
    }

    #[test]
    fn admission_at_cap_rejects() {
        assert_eq!(
            check_admission(5, 5, 2_000),
            AdmissionDecision::Reject { live: 5, cap: 5 }
        );
        // Over cap (should not normally happen, but be defensive) also rejects.
        assert_eq!(
            check_admission(6, 5, 0),
            AdmissionDecision::Reject { live: 6, cap: 5 }
        );
    }

    #[test]
    fn admission_zero_cap_always_rejects() {
        assert_eq!(
            check_admission(0, 0, 0),
            AdmissionDecision::Reject { live: 0, cap: 0 }
        );
    }

    #[test]
    fn admission_admits_exactly_up_to_cap() {
        // With cap=3, launches at live=0,1,2 admit; live=3 rejects. This proves
        // the cap admits EXACTLY `cap` concurrent sessions, no more.
        for live in 0..3 {
            assert!(matches!(
                check_admission(live, 3, 0),
                AdmissionDecision::Admit { .. }
            ));
        }
        assert!(matches!(
            check_admission(3, 3, 0),
            AdmissionDecision::Reject { .. }
        ));
    }

    #[test]
    fn admission_error_display() {
        let err = AdmissionError::CapReached { live: 5, cap: 5 };
        let msg = err.to_string();
        assert!(msg.contains("session cap reached"));
        assert!(msg.contains("max_concurrent_sessions = 5"));
    }
}
