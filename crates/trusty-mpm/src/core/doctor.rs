//! Diagnostic report types for the `tm doctor` health check.
//!
//! Why: `tm doctor` verifies the full trusty-mpm stack is correctly wired —
//! instructions deployed, agents and skills present, and the trusty-memory /
//! trusty-search sidecars reachable. Every UI (CLI, Telegram, the HTTP API)
//! renders the *same* diagnostic, so the report shape lives here in `core` as a
//! single shared, serde-stable contract rather than being re-modelled per UI.
//! What: [`DoctorReport`] groups one [`DoctorCheck`] per probe plus an
//! aggregate [`CheckStatus`] (the worst of all checks) and a generation
//! timestamp. The daemon produces the report; the clients only render it.
//! Test: `cargo test -p trusty-mpm-core doctor` covers the worst-status
//! aggregation and a serde round-trip.

use serde::{Deserialize, Serialize};

/// The outcome of a single diagnostic probe.
///
/// Why: a doctor check is not simply pass/fail — some conditions (an empty but
/// present skills directory) are a warning, not a failure, so a three-level
/// status keeps the operator's attention proportional to the problem.
/// What: `Ok` (healthy), `Warn` (degraded but usable), `Fail` (broken). The
/// serde wire form is the lowercase variant name.
/// Test: `worst_picks_the_most_severe`, `status_serde_round_trips`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    /// The probe passed — the component is healthy.
    Ok,
    /// The probe found a degraded-but-usable condition worth flagging.
    Warn,
    /// The probe failed — the component is broken or unreachable.
    Fail,
}

impl CheckStatus {
    /// Severity rank — higher means worse.
    ///
    /// Why: the aggregate report status is the *worst* of every check; ranking
    /// the variants lets [`worst`](Self::worst) compare them with a plain `max`.
    /// What: `Ok = 0`, `Warn = 1`, `Fail = 2`.
    /// Test: `worst_picks_the_most_severe`.
    fn rank(self) -> u8 {
        match self {
            CheckStatus::Ok => 0,
            CheckStatus::Warn => 1,
            CheckStatus::Fail => 2,
        }
    }

    /// Return the more severe of two statuses.
    ///
    /// Why: the overall report status must reflect the worst single check, so
    /// one failing probe makes the whole report fail.
    /// What: returns whichever status has the higher [`rank`](Self::rank).
    /// Test: `worst_picks_the_most_severe`.
    pub fn worst(self, other: CheckStatus) -> CheckStatus {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
}

/// One named diagnostic check and its outcome.
///
/// Why: each probe (instructions, agents, skills, memory, search) reports a
/// stable name, a status, and a human-readable message the UIs render verbatim.
/// What: the check `name`, its [`CheckStatus`], and a one-line `message`
/// describing what was found (and, on failure, a hint at the fix).
/// Test: covered by the daemon's `run_doctor` tests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorCheck {
    /// Short stable name of the check, e.g. `"instructions"` or `"memory"`.
    pub name: String,
    /// The probe's outcome.
    pub status: CheckStatus,
    /// Human-readable description of what the probe found.
    pub message: String,
}

impl DoctorCheck {
    /// Build a check from its three fields.
    ///
    /// Why: the daemon's probes construct many checks; a constructor keeps each
    /// call site a single readable line.
    /// What: returns a [`DoctorCheck`] with `name`/`message` converted to owned
    /// strings.
    /// Test: covered by the daemon's `run_doctor` tests.
    pub fn new(name: impl Into<String>, status: CheckStatus, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status,
            message: message.into(),
        }
    }
}

/// The complete `tm doctor` diagnostic report.
///
/// Why: `tm doctor` runs several independent probes; bundling them with an
/// aggregate status and a timestamp gives every UI one value to render and the
/// operator one verdict to read.
/// What: the `overall` status (the worst of all `checks`), the ordered list of
/// `checks`, and the UTC time the report was generated.
/// Test: `report_overall_is_worst_check`, `report_serde_round_trips`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    /// Aggregate status — the most severe of every check's status.
    pub overall: CheckStatus,
    /// Every diagnostic check, in the order they were run.
    pub checks: Vec<DoctorCheck>,
    /// UTC timestamp the report was generated.
    pub generated_at: chrono::DateTime<chrono::Utc>,
}

impl DoctorReport {
    /// Assemble a report from a set of checks, deriving the aggregate status.
    ///
    /// Why: the `overall` status must always be the worst of `checks` and the
    /// timestamp must be "now"; computing both here keeps every producer
    /// consistent and prevents a stale or hand-set aggregate.
    /// What: folds the checks with [`CheckStatus::worst`] (an empty set is
    /// `Ok`), stamps `generated_at` with the current UTC time, and returns the
    /// [`DoctorReport`].
    /// Test: `report_overall_is_worst_check`.
    pub fn from_checks(checks: Vec<DoctorCheck>) -> Self {
        let overall = checks
            .iter()
            .fold(CheckStatus::Ok, |acc, c| acc.worst(c.status));
        Self {
            overall,
            checks,
            generated_at: chrono::Utc::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worst_picks_the_most_severe() {
        assert_eq!(CheckStatus::Ok.worst(CheckStatus::Warn), CheckStatus::Warn);
        assert_eq!(CheckStatus::Warn.worst(CheckStatus::Ok), CheckStatus::Warn);
        assert_eq!(
            CheckStatus::Warn.worst(CheckStatus::Fail),
            CheckStatus::Fail
        );
        assert_eq!(CheckStatus::Fail.worst(CheckStatus::Ok), CheckStatus::Fail);
        assert_eq!(CheckStatus::Ok.worst(CheckStatus::Ok), CheckStatus::Ok);
    }

    #[test]
    fn status_serde_round_trips() {
        // The wire form is the lowercase variant name.
        let json = serde_json::to_string(&CheckStatus::Warn).unwrap();
        assert_eq!(json, "\"warn\"");
        let back: CheckStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, CheckStatus::Warn);
    }

    #[test]
    fn report_overall_is_worst_check() {
        // An all-Ok set is Ok; a single Fail makes the whole report Fail.
        let ok = DoctorReport::from_checks(vec![
            DoctorCheck::new("a", CheckStatus::Ok, "fine"),
            DoctorCheck::new("b", CheckStatus::Ok, "fine"),
        ]);
        assert_eq!(ok.overall, CheckStatus::Ok);

        let mixed = DoctorReport::from_checks(vec![
            DoctorCheck::new("a", CheckStatus::Ok, "fine"),
            DoctorCheck::new("b", CheckStatus::Warn, "meh"),
            DoctorCheck::new("c", CheckStatus::Fail, "broken"),
        ]);
        assert_eq!(mixed.overall, CheckStatus::Fail);

        // An empty report is vacuously Ok.
        let empty = DoctorReport::from_checks(vec![]);
        assert_eq!(empty.overall, CheckStatus::Ok);
    }

    #[test]
    fn report_serde_round_trips() {
        let report = DoctorReport::from_checks(vec![DoctorCheck::new(
            "instructions",
            CheckStatus::Ok,
            "last-instructions.md present",
        )]);
        let json = serde_json::to_string(&report).unwrap();
        let back: DoctorReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back.overall, CheckStatus::Ok);
        assert_eq!(back.checks.len(), 1);
        assert_eq!(back.checks[0].name, "instructions");
    }
}
