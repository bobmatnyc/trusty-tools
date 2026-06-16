//! `tctl status` — stack summary: installed versions + daemon health (#1316).
//!
//! Why: A quick "what's installed and is it healthy?" command over the whole
//! stable set, sugar over a full `stack health`. It is the operator's at-a-glance
//! view and the data the console / scripts consume via `--json`.
//!
//! What: For each stable-set member, reports the installed version (via
//! `<binary> --version`) and — for daemons — a coarse health verdict (via
//! `<binary> health --json`, reusing the `up::system_runner::classify_status`
//! mapping). Renders a human table or a `--json` envelope. Returns exit 0 when
//! every daemon is healthy (or absent-but-reported), 2 when a daemon is down.
//!
//! Test: `tests` covers the report shaping + the overall verdict derivation; the
//! probes are side-effecting.

use std::process::Command;

use serde::Serialize;

use super::stable_set::stable_set;
use super::up::member::MemberHealth;
use super::up::system_runner::classify_status;
use super::update_engine::installed_version;
use crate::output::render_json;

/// One member's status line.
///
/// Why: Typed per-member status keeps machine output stable + testable.
/// What: `member` crate name; `installed` version or `None`; `health` a coarse
/// verdict string (`healthy`/`stale`/`down`/`not_installed`/`n/a` for non-daemons).
/// Test: `tests::report_serialises`.
#[derive(Clone, Debug, Serialize)]
pub struct MemberStatus {
    /// Crate name.
    pub member: String,
    /// Installed version, or `None` when the binary is absent.
    pub installed: Option<String>,
    /// Coarse health verdict.
    pub health: String,
    /// Whether this member is a daemon (health is `n/a` otherwise).
    pub daemon: bool,
}

/// The aggregate status report.
///
/// Why: `--json` consumers want the rollup + an overall verdict in one object.
/// What: Holds per-member statuses and the overall verdict string.
/// Test: `tests::report_serialises`, `tests::verdict_*`.
#[derive(Clone, Debug, Serialize)]
pub struct StatusReport {
    /// Fixed command tag.
    pub command: &'static str,
    /// Per-member statuses in stable-set order.
    pub members: Vec<MemberStatus>,
    /// Overall verdict: `ready` | `degraded` | `incomplete`.
    pub verdict: &'static str,
}

impl StatusReport {
    /// Build a report and compute the overall verdict.
    ///
    /// Why: One place derives the verdict so the exit code + JSON agree.
    /// What: `degraded` if any daemon is `down`; else `incomplete` if any member
    /// is not installed; else `ready`.
    /// Test: `tests::verdict_down_is_degraded`, `tests::verdict_missing_is_incomplete`.
    fn build(members: Vec<MemberStatus>) -> Self {
        let any_down = members.iter().any(|m| m.daemon && m.health == "down");
        let any_missing = members.iter().any(|m| m.installed.is_none());
        let verdict = if any_down {
            "degraded"
        } else if any_missing {
            "incomplete"
        } else {
            "ready"
        };
        Self {
            command: "status",
            members,
            verdict,
        }
    }

    /// Process exit code: 0 ready/incomplete, 2 degraded.
    ///
    /// Why: A monitoring script branches on this; a down daemon is the failure.
    /// What: `2` when `degraded`, else `0` (an incomplete install is not a
    /// runtime failure — `tctl install` is the remedy, not an error here).
    /// Test: `tests::exit_code_reflects_verdict`.
    fn exit_code(&self) -> i32 {
        if self.verdict == "degraded" {
            2
        } else {
            0
        }
    }
}

/// Coarse health probe for a daemon member (`<binary> health --json`).
///
/// Why: `status` reports a one-word health per daemon; reuse the same
/// classification the boot path uses so the vocabulary is consistent.
/// What: Spawns `<binary> health --json`; maps the `status` field via
/// `classify_status`. Returns `NotInstalled` when the binary is absent, `Down`
/// on spawn/parse failure.
/// Test: Side-effect-only (subprocess); `classify_status` is unit-tested in `up`.
fn probe_daemon_health(binary: &str) -> MemberHealth {
    if which::which(binary).is_err() {
        return MemberHealth::NotInstalled;
    }
    let out = Command::new(binary).args(["health", "--json"]).output();
    let Ok(out) = out else {
        return MemberHealth::Down;
    };
    if !out.status.success() {
        return MemberHealth::Down;
    }
    match serde_json::from_slice::<serde_json::Value>(&out.stdout) {
        Ok(v) => {
            let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("down");
            classify_status(status)
        }
        Err(_) => MemberHealth::HealthyStale,
    }
}

/// Map a [`MemberHealth`] to the status report's coarse string.
///
/// Why: The report uses a flat string vocabulary; centralise the mapping.
/// What: `HealthyVersionOk` → `healthy`, `HealthyStale` → `stale`, `Down` →
/// `down`, `NotInstalled` → `not_installed`.
/// Test: `tests::health_string_mapping`.
fn health_string(h: MemberHealth) -> &'static str {
    match h {
        MemberHealth::HealthyVersionOk => "healthy",
        MemberHealth::HealthyStale => "stale",
        MemberHealth::Down => "down",
        MemberHealth::NotInstalled => "not_installed",
    }
}

/// Handle `tctl status`.
///
/// Why: Phase-1 stack summary (#1316 priority 5).
///
/// What: Probes each stable-set member's installed version and (for daemons)
/// health, builds the report, and renders human or `--json`. Returns the exit code.
///
/// Test: Side-effecting (probes); the report logic is tested via `StatusReport`.
pub fn run(json: bool) -> i32 {
    let mut members = Vec::new();
    for m in stable_set() {
        let installed = installed_version(&m.binary);
        let (health, daemon) = if m.daemon {
            (
                health_string(probe_daemon_health(&m.binary)).to_owned(),
                true,
            )
        } else {
            (
                if installed.is_some() {
                    "n/a"
                } else {
                    "not_installed"
                }
                .to_owned(),
                false,
            )
        };
        members.push(MemberStatus {
            member: m.crate_name,
            installed,
            health,
            daemon,
        });
    }
    let report = StatusReport::build(members);
    if json {
        // A failed machine-readable write must not exit 0: a monitoring script
        // would read health from the exit code while the JSON never arrived.
        if render_json(&report).is_err() {
            eprintln!("tctl status: failed to write JSON output");
            return 1;
        }
    } else {
        print_human(&report);
    }
    report.exit_code()
}

/// Render the human-readable status table to stdout.
///
/// Why: Operators want an at-a-glance table.
/// What: Prints one aligned row per member + the verdict footer.
/// Test: Side-effect-only; the data is tested via `StatusReport`.
fn print_human(report: &StatusReport) {
    println!("tctl status — stack summary");
    for m in &report.members {
        let ver = m.installed.as_deref().unwrap_or("(absent)");
        println!("  {:<18} {:<12} {}", m.member, ver, m.health);
    }
    println!("verdict: {} (exit {})", report.verdict, report.exit_code());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(member: &str, installed: Option<&str>, health: &str, daemon: bool) -> MemberStatus {
        MemberStatus {
            member: member.to_owned(),
            installed: installed.map(str::to_owned),
            health: health.to_owned(),
            daemon,
        }
    }

    /// Why: The JSON envelope is a public contract; pin its shape.
    /// What: Builds a report and asserts the keys.
    /// Test: This is the test.
    #[test]
    fn report_serialises() {
        let report =
            StatusReport::build(vec![ms("trusty-search", Some("0.20.0"), "healthy", true)]);
        let v = serde_json::to_value(&report).expect("serialises");
        assert_eq!(v["command"], "status");
        assert_eq!(v["verdict"], "ready");
        assert_eq!(v["members"][0]["health"], "healthy");
    }

    /// Why: A down daemon must degrade the whole verdict.
    /// What: Builds a report with one down daemon; asserts `degraded`.
    /// Test: This is the test.
    #[test]
    fn verdict_down_is_degraded() {
        let report = StatusReport::build(vec![ms("trusty-search", Some("0.20.0"), "down", true)]);
        assert_eq!(report.verdict, "degraded");
    }

    /// Why: A missing member (but no down daemon) is `incomplete`, not failure.
    /// What: Builds a report with one absent member; asserts `incomplete`.
    /// Test: This is the test.
    #[test]
    fn verdict_missing_is_incomplete() {
        let report = StatusReport::build(vec![ms("tga", None, "not_installed", false)]);
        assert_eq!(report.verdict, "incomplete");
    }

    /// Why: The exit code must track the verdict for monitoring.
    /// What: Asserts 2 for degraded, 0 for ready and incomplete.
    /// Test: This is the test.
    #[test]
    fn exit_code_reflects_verdict() {
        let degraded = StatusReport::build(vec![ms("trusty-search", Some("1.0.0"), "down", true)]);
        assert_eq!(degraded.exit_code(), 2);
        let ready = StatusReport::build(vec![ms("trusty-search", Some("1.0.0"), "healthy", true)]);
        assert_eq!(ready.exit_code(), 0);
        let incomplete = StatusReport::build(vec![ms("tga", None, "not_installed", false)]);
        assert_eq!(incomplete.exit_code(), 0);
    }

    /// Why: The health-string mapping is the report's vocabulary; pin it.
    /// What: Asserts each `MemberHealth` → string.
    /// Test: This is the test.
    #[test]
    fn health_string_mapping() {
        assert_eq!(health_string(MemberHealth::HealthyVersionOk), "healthy");
        assert_eq!(health_string(MemberHealth::HealthyStale), "stale");
        assert_eq!(health_string(MemberHealth::Down), "down");
        assert_eq!(health_string(MemberHealth::NotInstalled), "not_installed");
    }
}
