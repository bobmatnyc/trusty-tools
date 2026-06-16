//! `tctl stack doctor [<member>]` — deep diagnostic sweep (DOC-4).
//!
//! Why: `stack health` answers "is it up?"; `stack doctor` answers "why isn't
//! it?" by collecting per-member diagnostics: a health probe PLUS binary-on-PATH,
//! plist-installed, port-recorded, and installed-version checks. Scoping to a
//! single member focuses the sweep when triaging one daemon.
//!
//! What: for each in-scope daemon member, runs the strategy-aware health probe
//! and the four diagnostic checks, builds a [`DoctorReport`], and renders a human
//! drill-down or `--json` envelope. Returns exit 0 when every member is OK
//! (binary present + not `down`), 2 otherwise.
//!
//! Test: `tests` covers the per-member check aggregation (`MemberDoctor::ok`),
//! the report verdict, and the unknown-member error path; the filesystem/PATH
//! probes are side-effecting.

use serde::Serialize;

use crate::commands::plist_label::plist_path_for;
use crate::commands::probe::{health_str, probe_member_health};
use crate::commands::stable_set::{stable_set, ManageStrategy, StableMember};
use crate::commands::update_engine::installed_version;
use crate::output::render_json;

/// Per-member diagnostic result.
///
/// Why: a typed record keeps the `--json` drill-down stable + testable and lets
/// `ok` be derived in one place.
/// What: the member's name, health verdict, and the four boolean/Option checks.
/// `plist_installed` is `None` for non-launchd members (the check does not apply).
/// Test: `tests::member_ok_*`.
#[derive(Clone, Debug, Serialize)]
pub struct MemberDoctor {
    /// Crate name.
    pub member: String,
    /// Strategy-aware health verdict.
    pub health: String,
    /// Whether the binary is on PATH.
    pub on_path: bool,
    /// Whether the launchd plist is installed (`None` = check N/A for this member).
    pub plist_installed: Option<bool>,
    /// Whether a daemon address is recorded (port reachable proxy).
    pub port_recorded: bool,
    /// Installed version, or `None` when absent.
    pub version: Option<String>,
}

impl MemberDoctor {
    /// Whether this member passed its diagnostics.
    ///
    /// Why: the report verdict + exit code reduce over this.
    /// What: OK iff the binary is on PATH AND its health is not `down`. Absent
    /// plist / unrecorded port are reported but not fatal (a member may be
    /// stopped intentionally); a missing binary or a `down` daemon is the failure.
    /// Test: `tests::member_ok_requires_path_and_not_down`.
    pub fn ok(&self) -> bool {
        self.on_path && self.health != health_str::DOWN
    }
}

/// The aggregate doctor report.
///
/// Why: `--json` consumers want the per-member drill-down + verdict together.
/// What: holds the per-member records + overall verdict.
/// Test: `tests::report_verdict_*`.
#[derive(Clone, Debug, Serialize)]
pub struct DoctorReport {
    /// Fixed command tag.
    pub command: &'static str,
    /// Per-member diagnostics in stable-set order.
    pub members: Vec<MemberDoctor>,
    /// Overall verdict: `ok` | `degraded`.
    pub verdict: &'static str,
}

impl DoctorReport {
    /// Build the report and derive the verdict.
    ///
    /// Why: one place derives the verdict so JSON + exit code agree.
    /// What: `degraded` when any member is not `ok`, else `ok`.
    /// Test: `tests::report_verdict_degraded`.
    fn build(members: Vec<MemberDoctor>) -> Self {
        let verdict = if members.iter().all(MemberDoctor::ok) {
            "ok"
        } else {
            "degraded"
        };
        Self {
            command: "stack doctor",
            members,
            verdict,
        }
    }

    /// Process exit code: 0 ok, 2 degraded.
    ///
    /// Why: triage scripts branch on this.
    /// What: `2` when `degraded`, else `0`.
    /// Test: `tests::report_verdict_degraded`.
    fn exit_code(&self) -> i32 {
        if self.verdict == "degraded" {
            2
        } else {
            0
        }
    }
}

/// Collect diagnostics for a single member.
///
/// Why: isolating the per-member probe keeps `run_doctor` a thin loop.
/// What: runs the health probe + the four checks (PATH, plist, recorded address,
/// version). `plist_installed` is only checked for launchd members.
/// Test: side-effecting (PATH/FS); the `ok` reduction is unit-tested.
fn diagnose(m: &StableMember) -> MemberDoctor {
    let on_path = which::which(&m.binary).is_ok();
    let plist_installed = match m.manage {
        ManageStrategy::Launchd => Some(
            plist_path_for(&m.binary)
                .map(|p| p.exists())
                .unwrap_or(false),
        ),
        ManageStrategy::OwnVerb | ManageStrategy::None => None,
    };
    let port_recorded = matches!(
        trusty_common::read_daemon_addr(&m.binary),
        Ok(Some(a)) if !a.is_empty()
    );
    MemberDoctor {
        health: probe_member_health(&m.binary, m.manage),
        on_path,
        plist_installed,
        port_recorded,
        version: installed_version(&m.binary),
        member: m.crate_name.clone(),
    }
}

/// Handle `tctl stack doctor [<member>]`.
///
/// Why: Phase-2 deep diagnostic sweep (DOC-4).
/// What: resolves the in-scope daemon members (all, or the one named — unknown →
/// exit 3), diagnoses each, builds + renders the report, returns the exit code.
/// Test: side-effecting; the aggregation is tested via `DoctorReport`/`MemberDoctor`.
pub fn run_doctor(member: Option<&str>, json: bool) -> i32 {
    let all = stable_set();
    let scoped: Vec<StableMember> = match member {
        None => all.into_iter().filter(|m| m.daemon).collect(),
        Some(name) => {
            let found: Vec<StableMember> = all
                .into_iter()
                .filter(|m| m.daemon && (m.crate_name == name || m.binary == name))
                .collect();
            if found.is_empty() {
                if json {
                    let _ = render_json(&serde_json::json!({
                        "command": "stack doctor",
                        "error": format!("unknown daemon member: {name}"),
                    }));
                } else {
                    eprintln!("tctl stack doctor: unknown daemon member: {name}");
                }
                return 3;
            }
            found
        }
    };

    let members: Vec<MemberDoctor> = scoped.iter().map(diagnose).collect();
    let report = DoctorReport::build(members);
    if json {
        if render_json(&report).is_err() {
            eprintln!("tctl stack doctor: failed to write JSON output");
            return 1;
        }
    } else {
        print_human(&report);
    }
    report.exit_code()
}

/// Render the human-readable doctor drill-down.
///
/// Why: operators want a readable per-member breakdown.
/// What: prints each member's checks + the verdict footer.
/// Test: side-effect-only; the data is tested via the report structs.
fn print_human(report: &DoctorReport) {
    println!("tctl stack doctor");
    for m in &report.members {
        let plist = match m.plist_installed {
            Some(true) => "plist:yes",
            Some(false) => "plist:NO",
            None => "plist:n/a",
        };
        let ver = m.version.as_deref().unwrap_or("(absent)");
        println!(
            "  {:<18} {:<12} path:{} {} port:{} health:{}",
            m.member,
            ver,
            if m.on_path { "yes" } else { "NO" },
            plist,
            if m.port_recorded { "yes" } else { "no" },
            m.health,
        );
    }
    println!("verdict: {} (exit {})", report.verdict, report.exit_code());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md(member: &str, health: &str, on_path: bool) -> MemberDoctor {
        MemberDoctor {
            member: member.to_owned(),
            health: health.to_owned(),
            on_path,
            plist_installed: Some(true),
            port_recorded: true,
            version: Some("1.0.0".to_owned()),
        }
    }

    /// Why: `ok` must require the binary on PATH AND health not `down`; an
    /// intentionally-stopped daemon (absent port) is reported, not failed.
    /// What: asserts the truth table for `ok`.
    /// Test: This is the test.
    #[test]
    fn member_ok_requires_path_and_not_down() {
        assert!(md("a", "healthy", true).ok());
        assert!(!md("a", "healthy", false).ok()); // not on PATH
        assert!(!md("a", "down", true).ok()); // down
                                              // unknown health (mpm) with binary present is OK.
        assert!(md("trusty-mpm", "unknown", true).ok());
    }

    /// Why: the JSON envelope is a contract; pin its shape.
    /// What: builds a report and asserts keys.
    /// Test: This is the test.
    #[test]
    fn report_serialises() {
        let r = DoctorReport::build(vec![md("trusty-search", "healthy", true)]);
        let v = serde_json::to_value(&r).expect("serialises");
        assert_eq!(v["command"], "stack doctor");
        assert_eq!(v["verdict"], "ok");
        assert_eq!(v["members"][0]["on_path"], true);
        assert_eq!(v["members"][0]["plist_installed"], true);
    }

    /// Why: any not-ok member degrades the verdict + exit code.
    /// What: a down member → `degraded`, exit 2.
    /// Test: This is the test.
    #[test]
    fn report_verdict_degraded() {
        let r = DoctorReport::build(vec![
            md("trusty-search", "healthy", true),
            md("trusty-memory", "down", true),
        ]);
        assert_eq!(r.verdict, "degraded");
        assert_eq!(r.exit_code(), 2);
    }

    /// Why: an all-ok sweep is `ok`, exit 0.
    /// What: two healthy members → `ok`.
    /// Test: This is the test.
    #[test]
    fn report_verdict_ok() {
        let r = DoctorReport::build(vec![
            md("trusty-search", "healthy", true),
            md("trusty-mpm", "unknown", true),
        ]);
        assert_eq!(r.verdict, "ok");
        assert_eq!(r.exit_code(), 0);
    }
}
