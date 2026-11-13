//! `tctl stack health` — fast liveness sweep across the whole stack (DOC-4).
//!
//! Why: The at-a-glance "is everything up?" rollup. It fans the shared
//! strategy-aware health probe across every stable-set daemon and reduces the
//! per-member verdicts to one overall verdict.
//!
//! What: probes each daemon member via `probe::probe_member_health` (an HTTP
//! `GET /health` probe since #4246, not a `health --json` subprocess), builds a
//! [`HealthReport`], renders a human matrix or `--json` envelope, and returns an
//! exit code (0 ready, 2 when any daemon is `down`, 4 when a daemon's health is
//! only `unknown` — #4847: an undetermined health never reports as ready).
//!
//! Test: `tests` covers the report shaping + verdict derivation; the probe is
//! side-effecting (covered in `probe`).

use serde::Serialize;

use std::collections::HashMap;

use crate::commands::deployment_record::{
    audit_for_rollup, fold_verdict, print_human as print_deployment, running_of, DeploymentAudit,
    RunningVersion,
};
use crate::commands::probe::{health_str, probe_member_health};
use crate::commands::stable_set::daemon_members;
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
    /// Overall verdict: `ready` | `undetermined` | `degraded`.
    pub verdict: &'static str,
    /// #10607: what is installed against what install placed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment: Option<DeploymentAudit>,
}

impl HealthReport {
    /// Build a report and compute the verdict.
    ///
    /// Why: one place derives the verdict so the exit code + JSON agree.
    /// What: three verdicts, worst-wins. `down` or `not_installed` on any member
    /// is `degraded` — a `down` daemon is the running-but-broken failure signal,
    /// and a `not_installed` member is a real gap in the resolved stable set (an
    /// operator after a failed install must NOT see green). Otherwise an
    /// `unknown` member makes the verdict `undetermined`: the probe could not
    /// tell, which is a different claim from `ready`. `stale` stays
    /// non-degrading (running, just below the version floor), so an
    /// all-healthy-or-stale sweep is `ready`.
    ///
    /// #4847: `unknown` used to fold into `ready`, so a sweep that determined
    /// nothing about a member still exited 0 and a harness gating on that exit
    /// code went green over a stack that was not up. `unknown` now carries its
    /// own verdict and its own non-zero exit instead of counting as failure, so
    /// a broken daemon (exit 2) stays distinguishable from one that could not be
    /// probed (exit 4).
    /// Test: `tests::verdict_down_is_degraded`,
    /// `tests::verdict_not_installed_is_degraded`,
    /// `tests::verdict_unknown_is_undetermined`,
    /// `tests::verdict_down_outranks_unknown`.
    fn build(members: Vec<HealthRow>) -> Self {
        let degrades = members
            .iter()
            .any(|m| m.health == health_str::DOWN || m.health == health_str::NOT_INSTALLED);
        let undetermined = members.iter().any(|m| m.health == health_str::UNKNOWN);
        let verdict = if degrades {
            "degraded"
        } else if undetermined {
            "undetermined"
        } else {
            "ready"
        };
        Self {
            command: "stack health",
            members,
            verdict,
            deployment: None,
        }
    }

    /// Fold the deployment audit into the verdict (#10607).
    ///
    /// Why: every daemon can answer healthy over a deployment that is not the
    /// one install placed.
    /// What: drift is `degraded`; an audit that could not tell is `undetermined`.
    /// Test: `tests::drift_degrades_a_healthy_sweep`,
    /// `tests::an_undetermined_audit_is_not_ready`.
    fn with_deployment(mut self, audit: DeploymentAudit) -> Self {
        self.verdict = fold_verdict(self.verdict, &audit);
        self.deployment = Some(audit);
        self
    }

    /// Process exit code: 0 ready, 2 degraded, 4 undetermined.
    ///
    /// Why: monitoring scripts branch on this, so three verdicts need three
    /// codes. A script that treats any non-zero as "not ready" is then right
    /// about `undetermined` too, and one that discriminates can still separate a
    /// broken daemon (2) from an unprobeable one (4) (#4847).
    /// What: `2` when `degraded`, `4` when `undetermined`, else `0`.
    /// Test: `tests::exit_code_reflects_verdict`.
    fn exit_code(&self) -> i32 {
        match self.verdict {
            "degraded" => 2,
            "undetermined" => 4,
            _ => 0,
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
    // The daemon rule is pinned once in `stable_set::daemon_members`.
    let mut running: HashMap<String, RunningVersion> = HashMap::new();
    let members: Vec<HealthRow> = daemon_members()
        .into_iter()
        .map(|m| {
            // #4246: typed probe; the rollup renders the flat word.
            let outcome = probe_member_health(&m.binary, m.manage);
            let health = outcome.health_string().to_owned();
            if health != health_str::NOT_INSTALLED {
                running.insert(m.crate_name.clone(), running_of(&m, &outcome));
            }
            HealthRow {
                health,
                member: m.crate_name,
            }
        })
        .collect();
    let report = HealthReport::build(members).with_deployment(audit_for_rollup(&running));
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
        if let Some(audit) = &report.deployment {
            print_deployment(audit);
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

    /// Why: #4847 — an `unknown` member must not report as `ready`. The probe
    /// determined nothing about it, and a harness that gates on this exit code
    /// passed a stack that was not up. `unknown` is not `degraded` either: it is
    /// its own verdict with its own non-zero exit.
    /// What: one unknown member → `undetermined`, exit 4. Mixing unknown with a
    /// healthy member keeps it `undetermined`, so one determined member cannot
    /// carry the sweep back to green.
    /// Test: This is the test.
    #[test]
    fn verdict_unknown_is_undetermined() {
        let r = HealthReport::build(vec![row("trusty-mpm", "unknown")]);
        assert_eq!(r.verdict, "undetermined");
        assert_eq!(r.exit_code(), 4);

        let mixed = HealthReport::build(vec![
            row("trusty-search", "healthy"),
            row("trusty-mpm", "unknown"),
        ]);
        assert_eq!(mixed.verdict, "undetermined");
        assert_eq!(mixed.exit_code(), 4);
    }

    /// Why: the two non-green verdicts must not mask each other. A broken daemon
    /// is the actionable signal, so `degraded` outranks `undetermined` when both
    /// are present (#4847).
    /// What: down + unknown → `degraded`, exit 2. `stale` alongside unknown does
    /// not degrade, so that mix stays `undetermined`.
    /// Test: This is the test.
    #[test]
    fn verdict_down_outranks_unknown() {
        let both = HealthReport::build(vec![
            row("trusty-search", "down"),
            row("trusty-mpm", "unknown"),
        ]);
        assert_eq!(both.verdict, "degraded");
        assert_eq!(both.exit_code(), 2);

        let stale_and_unknown = HealthReport::build(vec![
            row("trusty-search", "stale"),
            row("trusty-mpm", "unknown"),
        ]);
        assert_eq!(stale_and_unknown.verdict, "undetermined");
        assert_eq!(stale_and_unknown.exit_code(), 4);
    }

    /// Why: #10607 — all daemons healthy with a recorded binary gone read `ready`.
    /// What: a drifted audit makes a healthy sweep `degraded`, exit 2.
    /// Test: This is the test.
    #[test]
    fn drift_degrades_a_healthy_sweep() {
        let r = HealthReport::build(vec![row("trusty-search", "healthy")]).with_deployment(
            DeploymentAudit {
                state: "drifted",
                findings: Vec::new(),
            },
        );
        assert_eq!(r.verdict, "degraded");
        assert_eq!(r.exit_code(), 2);
    }

    /// Why: #10607 Fail-Open Check — an audit that could not compare must not
    /// read as `ready`.
    /// What: an undetermined audit makes a healthy sweep `undetermined`, exit 4.
    /// Test: This is the test.
    #[test]
    fn an_undetermined_audit_is_not_ready() {
        let r = HealthReport::build(vec![row("trusty-search", "healthy")]).with_deployment(
            DeploymentAudit {
                state: "undetermined",
                findings: Vec::new(),
            },
        );
        assert_eq!(r.verdict, "undetermined");
        assert_eq!(r.exit_code(), 4);
    }

    /// Why: the exit code must track the verdict.
    /// What: asserts 0 for ready, 2 for degraded, 4 for undetermined (#4847).
    /// Test: This is the test.
    #[test]
    fn exit_code_reflects_verdict() {
        let ready = HealthReport::build(vec![row("trusty-search", "healthy")]);
        assert_eq!(ready.exit_code(), 0);
        let degraded = HealthReport::build(vec![row("trusty-search", "down")]);
        assert_eq!(degraded.exit_code(), 2);
        let undetermined = HealthReport::build(vec![row("trusty-search", "unknown")]);
        assert_eq!(undetermined.exit_code(), 4);
    }
}
