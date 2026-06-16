//! `tctl stack health` — fast liveness sweep across the whole stack (DOC-4).
//!
//! Why: The at-a-glance "is everything up?" rollup. It fans the shared
//! strategy-aware health probe across every stable-set daemon and reduces the
//! per-member verdicts to one overall verdict.
//!
//! What: probes each daemon member via `probe::probe_member_health`, builds a
//! [`HealthReport`], renders a human matrix or `--json` envelope, and returns an
//! exit code (0 ready, 2 when any daemon is `down`).
//!
//! Test: `tests` covers the report shaping + verdict derivation; the probe is
//! side-effecting (covered in `probe`).

use serde::Serialize;

use crate::commands::probe::{health_str, probe_member_health};
use crate::commands::stable_set::stable_set;
use crate::output::render_json;

/// One member's health row.
///
/// Why: a typed row keeps the `--json` matrix stable + testable.
/// What: `member` crate name + `health` verdict string.
/// Test: `tests::report_serialises`.
#[derive(Clone, Debug, Serialize)]
pub struct HealthRow {
    /// Crate name.
    pub member: String,
    /// Coarse health verdict (`healthy`/`stale`/`down`/`not_installed`/`unknown`).
    pub health: String,
}

/// The aggregate health report.
///
/// Why: `--json` consumers want the rollup + verdict in one object.
/// What: holds the per-member rows + the overall verdict.
/// Test: `tests::report_serialises`, `tests::verdict_*`.
#[derive(Clone, Debug, Serialize)]
pub struct HealthReport {
    /// Fixed command tag.
    pub command: &'static str,
    /// Per-member rows in stable-set order.
    pub members: Vec<HealthRow>,
    /// Overall verdict: `ready` | `degraded`.
    pub verdict: &'static str,
}

impl HealthReport {
    /// Build a report and compute the verdict.
    ///
    /// Why: one place derives the verdict so the exit code + JSON agree.
    /// What: degrade policy — both `down` AND `not_installed` degrade the verdict
    /// to `degraded`. A `down` daemon is the running-but-broken failure signal; a
    /// `not_installed` member is a real gap in the resolved stable set (an
    /// operator after a failed install must NOT see green). Only a genuinely
    /// `unknown` member (e.g. trusty-mpm, which has no `health --json` verb) does
    /// NOT degrade — its health is unprobeable, not absent. `stale` also stays
    /// non-degrading (running, just below the version floor).
    /// Test: `tests::verdict_down_is_degraded`,
    /// `tests::verdict_not_installed_is_degraded`,
    /// `tests::verdict_unknown_is_ready`.
    fn build(members: Vec<HealthRow>) -> Self {
        let degrades = members
            .iter()
            .any(|m| m.health == health_str::DOWN || m.health == health_str::NOT_INSTALLED);
        let verdict = if degrades { "degraded" } else { "ready" };
        Self {
            command: "stack health",
            members,
            verdict,
        }
    }

    /// Process exit code: 0 ready, 2 degraded.
    ///
    /// Why: monitoring scripts branch on this.
    /// What: `2` when `degraded`, else `0`.
    /// Test: `tests::exit_code_reflects_verdict`.
    fn exit_code(&self) -> i32 {
        if self.verdict == "degraded" {
            2
        } else {
            0
        }
    }
}

/// Handle `tctl stack health`.
///
/// Why: Phase-2 stack-wide liveness rollup (DOC-4).
/// What: probes every daemon member, builds + renders the report, returns the
/// exit code.
/// Test: side-effecting (probes); the report logic is tested via `HealthReport`.
pub fn run_health(json: bool) -> i32 {
    let members: Vec<HealthRow> = stable_set()
        .into_iter()
        .filter(|m| m.daemon)
        .map(|m| HealthRow {
            health: probe_member_health(&m.binary, m.manage),
            member: m.crate_name,
        })
        .collect();
    let report = HealthReport::build(members);
    if json {
        if render_json(&report).is_err() {
            eprintln!("tctl stack health: failed to write JSON output");
            return 1;
        }
    } else {
        println!("tctl stack health");
        for m in &report.members {
            println!("  {:<18} {}", m.member, m.health);
        }
        println!("verdict: {} (exit {})", report.verdict, report.exit_code());
    }
    report.exit_code()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(member: &str, health: &str) -> HealthRow {
        HealthRow {
            member: member.to_owned(),
            health: health.to_owned(),
        }
    }

    /// Why: the JSON matrix is a public contract; pin its shape.
    /// What: builds a report and asserts the keys.
    /// Test: This is the test.
    #[test]
    fn report_serialises() {
        let r = HealthReport::build(vec![row("trusty-search", "healthy")]);
        let v = serde_json::to_value(&r).expect("serialises");
        assert_eq!(v["command"], "stack health");
        assert_eq!(v["verdict"], "ready");
        assert_eq!(v["members"][0]["health"], "healthy");
    }

    /// Why: a down daemon degrades the whole verdict.
    /// What: one down member → `degraded`, exit 2.
    /// Test: This is the test.
    #[test]
    fn verdict_down_is_degraded() {
        let r = HealthReport::build(vec![row("trusty-search", "down")]);
        assert_eq!(r.verdict, "degraded");
        assert_eq!(r.exit_code(), 2);
    }

    /// Why: a `not_installed` member is a real gap in the resolved stable set
    /// (e.g. a failed install). It MUST degrade the verdict so an operator does
    /// not see green over a missing daemon.
    /// What: one not_installed member → `degraded`, exit 2.
    /// Test: This is the test.
    #[test]
    fn verdict_not_installed_is_degraded() {
        let r = HealthReport::build(vec![row("trusty-search", "not_installed")]);
        assert_eq!(r.verdict, "degraded");
        assert_eq!(r.exit_code(), 2);
    }

    /// Why: an `unknown` member (mpm) must NOT degrade the verdict — its health
    /// is merely unprobeable via the standard contract, not absent.
    /// What: one unknown member → `ready`, exit 0. A mix of unknown + healthy
    /// also stays ready, proving unknown never falsely degrades.
    /// Test: This is the test.
    #[test]
    fn verdict_unknown_is_ready() {
        let r = HealthReport::build(vec![row("trusty-mpm", "unknown")]);
        assert_eq!(r.verdict, "ready");
        assert_eq!(r.exit_code(), 0);

        let mixed = HealthReport::build(vec![
            row("trusty-search", "healthy"),
            row("trusty-mpm", "unknown"),
        ]);
        assert_eq!(mixed.verdict, "ready");
        assert_eq!(mixed.exit_code(), 0);
    }

    /// Why: the exit code must track the verdict.
    /// What: asserts 0 for ready, 2 for degraded.
    /// Test: This is the test.
    #[test]
    fn exit_code_reflects_verdict() {
        let ready = HealthReport::build(vec![row("trusty-search", "healthy")]);
        assert_eq!(ready.exit_code(), 0);
        let degraded = HealthReport::build(vec![row("trusty-search", "down")]);
        assert_eq!(degraded.exit_code(), 2);
    }
}
