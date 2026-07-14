//! Delivery status of the turnkey `--task` pane injection (issue #2364,
//! follow-up from the #2361 review).
//!
//! Why: split out of `record.rs` (which was pushed over the 500-SLOC
//! production cap by this addition) purely to keep that file under the cap —
//! no behavior change from the split itself. Before this field existed,
//! `SessionManager::inject_task_when_ready` was fire-and-forget — a backoff
//! exhaustion (or the session dying mid-wait) was only ever logged as a
//! `warn!`, with no way for a caller to check whether the injected task
//! actually reached the pane. Callers polling `tm session activity`/`tm
//! session info` had no signal to distinguish "still waiting for readiness"
//! from "gave up — task sitting unread as metadata".
//! What: [`InjectionStatus`] — five states covering the full injection
//! lifecycle, plus its `Display` impl (the wire/log string form).
//! Test: `injection_status_display`, `injection_status_default_is_not_applicable`
//! in this module's tests; the `SessionRecord` field integration is tested by
//! `record_without_injection_status_field_defaults_to_not_applicable` /
//! `record_round_trips_injection_status` in `super::record`'s tests; the
//! state-transition coverage lives in `session_manager::task_inject`'s
//! `inject_when_ready_*` tests.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Delivery status of the turnkey `--task` pane injection (#2364).
///
/// Why: see the module-level doc for the full observability rationale.
/// What: `NotApplicable` (the `#[default]`) covers every session for which
/// injection was never attempted — opted out (`--no-inject`), an empty task,
/// a non-`ClaudeCode` runtime (`tcode` injects in its own launch command), or
/// a spawn that never reached `Active` — see
/// [`super::task_inject::should_inject_task`]. `Pending` is set the instant
/// [`super::SessionManager::inject_task_when_ready`] begins polling
/// readiness; it resolves to exactly one of `Success` (delivered),
/// `FailedTimeout` (backoff exhausted — the runtime never showed a ready,
/// non-modal prompt within budget), or `FailedSessionDied` (the session
/// transitioned to `Stopped`/`Errored`/`Decommissioned` while injection was
/// waiting). In every `Failed*` case the task remains on the session record
/// as metadata, still deliverable via `tm session send`.
/// Test: `injection_status_display`, `injection_status_default_is_not_applicable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InjectionStatus {
    /// Injection was never attempted for this session (opted out, empty task,
    /// non-Claude-Code runtime, or the spawn never reached `Active`).
    #[default]
    NotApplicable,
    /// Readiness polling is in progress; the task has not yet been typed in.
    Pending,
    /// The task was successfully typed into the ready pane.
    Success,
    /// Readiness polling exhausted its bounded backoff without the pane ever
    /// showing a ready, non-modal prompt. The task remains as metadata.
    FailedTimeout,
    /// The session transitioned to `Stopped`/`Errored`/`Decommissioned` while
    /// injection was waiting for readiness. The task remains as metadata.
    FailedSessionDied,
}

impl fmt::Display for InjectionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::NotApplicable => "not_applicable",
            Self::Pending => "pending",
            Self::Success => "success",
            Self::FailedTimeout => "failed_timeout",
            Self::FailedSessionDied => "failed_session_died",
        };
        write!(f, "{s}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injection_status_display() {
        assert_eq!(InjectionStatus::NotApplicable.to_string(), "not_applicable");
        assert_eq!(InjectionStatus::Pending.to_string(), "pending");
        assert_eq!(InjectionStatus::Success.to_string(), "success");
        assert_eq!(InjectionStatus::FailedTimeout.to_string(), "failed_timeout");
        assert_eq!(
            InjectionStatus::FailedSessionDied.to_string(),
            "failed_session_died"
        );
    }

    #[test]
    fn injection_status_default_is_not_applicable() {
        assert_eq!(InjectionStatus::default(), InjectionStatus::NotApplicable);
    }
}
