//! `tctl upgrade` — confirm-then-apply upgrades, then restart daemons (#1316).
//!
//! Why: The mutating half of the update flow. The #1316 added requirement makes
//! the safety property explicit: present the available updates and apply them
//! ONLY after confirmation (`--yes` skips the prompt for automation). Daemons
//! are restarted cleanly after upgrade via `upgrade_and_restart` so launchd-
//! supervised members come back on the new binary.
//!
//! What: Gathers candidates (`update_engine`), runs the pure confirm-then-apply
//! gate (`decide_apply`), and — only on `Apply` — upgrades each member. Daemons
//! go through `upgrade_and_restart` (cargo install + health-gate + connection-
//! safe restart); non-daemons go through `perform_upgrade` + `verify_installed_
//! binary`. Progress rows render via `trusty-progress`. `--check` is a read-only
//! alias for `tctl updates`.
//!
//! Test: `tests` covers the decision routing and the JSON report shaping; the
//! actual `cargo install` + restart path is side-effecting.

use serde::Serialize;
use tokio::runtime::Handle;
use trusty_progress::{Component, ComponentTracker};

use super::progress_ui::{is_tty, narrator, prompt_yes_no};
use super::runtime::with_runtime;
use super::stable_set::select_members;
use super::update_engine::{
    decide_apply, gather_candidates, ApplyDecision, ApplyInputs, UpdateCandidate,
};
use crate::output::render_json;

/// One member's upgrade outcome for the report.
///
/// Why: Typed per-member result keeps machine output stable + testable.
/// What: `member` crate name; `ok` success; `detail` human note / error / hint.
/// Test: `tests::report_serialises`.
#[derive(Clone, Debug, Serialize)]
pub struct UpgradeOutcome {
    /// Crate name upgraded.
    pub member: String,
    /// Whether the upgrade (+ restart for daemons) succeeded.
    pub ok: bool,
    /// Human detail: applied version, restart hint, or error.
    pub detail: String,
}

/// The aggregate upgrade report.
///
/// Why: `--json` consumers want the whole rollup + a `status` discriminator that
/// distinguishes "applied", "declined", "needs-confirmation", and "nothing".
/// What: Holds the status string, the candidates considered, the per-member
/// outcomes, and `all_ok`.
/// Test: `tests::report_serialises`.
#[derive(Clone, Debug, Serialize)]
pub struct UpgradeReport {
    /// Fixed command tag.
    pub command: &'static str,
    /// `applied` | `declined` | `needs_confirmation` | `nothing_to_do`.
    pub status: &'static str,
    /// The candidates considered (always populated for transparency).
    pub candidates: Vec<UpdateCandidate>,
    /// Per-member outcomes (only populated when `status == "applied"`).
    pub members: Vec<UpgradeOutcome>,
    /// Whether every applied member succeeded (true when nothing was applied).
    pub all_ok: bool,
}

impl UpgradeReport {
    /// Build a non-applying report (declined / needs-confirmation / nothing).
    ///
    /// Why: The three non-mutating outcomes share a shape (no member outcomes).
    /// What: Sets `members = []`, `all_ok = true`.
    /// Test: `tests::report_serialises`.
    fn non_applying(status: &'static str, candidates: Vec<UpdateCandidate>) -> Self {
        Self {
            command: "upgrade",
            status,
            candidates,
            members: Vec::new(),
            all_ok: true,
        }
    }

    /// Build an applied report from per-member outcomes.
    ///
    /// Why: Centralises the `all_ok` derivation for the applied path.
    /// What: Sets `status = "applied"`, `all_ok = every outcome ok`.
    /// Test: `tests::applied_report_all_ok`.
    fn applied(candidates: Vec<UpdateCandidate>, members: Vec<UpgradeOutcome>) -> Self {
        let all_ok = members.iter().all(|m| m.ok);
        Self {
            command: "upgrade",
            status: "applied",
            candidates,
            members,
            all_ok,
        }
    }

    /// Process exit code per status.
    ///
    /// Why: Automation branches on this.
    /// What: `applied` → 0 if all_ok else 2; `needs_confirmation` → 3 (could not
    /// confirm — automation should pass `--yes`); `declined` → 4 (user said no);
    /// `nothing_to_do` → 0.
    /// Test: `tests::exit_codes_per_status`.
    fn exit_code(&self) -> i32 {
        match self.status {
            "applied" => {
                if self.all_ok {
                    0
                } else {
                    2
                }
            }
            "needs_confirmation" => 3,
            "declined" => 4,
            _ => 0,
        }
    }
}

/// Handle `tctl upgrade [<members>…] [--check] [--latest] [--exclude-self]`.
///
/// Why: Phase-1 confirm-then-apply upgrade (#1316 priority 2 + 3).
///
/// What: With `--check`, delegates to the read-only `updates` listing. Otherwise
/// gathers candidates, presents them, runs the pure confirm-then-apply gate, and
/// applies only on `Apply`. `exclude_self` drops `trusty-controller`/`tctl` from
/// the set (avoids upgrading the running binary mid-flight). Returns the exit code.
///
/// Test: `tests::run_check_is_readonly` (delegation); the apply path is
/// side-effecting and validated manually.
pub fn run(
    check: bool,
    latest: bool,
    exclude_self: bool,
    yes: bool,
    members: &[String],
    json: bool,
) -> i32 {
    // --check is a read-only alias for `tctl updates`.
    if check {
        return super::updates::run(latest, json);
    }

    let (mut selected, unknown) = select_members(members);
    if !unknown.is_empty() {
        let msg = format!("unknown member(s): {}", unknown.join(", "));
        if json {
            if render_json(&serde_json::json!({"command":"upgrade","error":msg})).is_err() {
                eprintln!("tctl upgrade: failed to write JSON output");
                return 1;
            }
        } else {
            eprintln!("tctl upgrade: {msg}");
        }
        return 3;
    }
    if exclude_self {
        selected.retain(|m| m.crate_name != "trusty-controller" && m.binary != "tctl");
    }

    // Build ONE runtime for the whole command: candidate gathering and applying
    // share the same handle (no per-future runtime construction, no nested
    // runtime panic risk).
    let report = with_runtime(|handle| {
        let candidates = handle.block_on(gather_candidates(&selected));
        resolve_and_apply(handle, candidates, yes, json)
    });
    if json {
        // A failed machine-readable write must not exit 0: automation would
        // read success from the exit code while the JSON never arrived.
        if render_json(&report).is_err() {
            eprintln!("tctl upgrade: failed to write JSON output");
            return 1;
        }
    } else {
        print_human(&report);
    }
    report.exit_code()
}

/// Present candidates, run the confirm-then-apply gate, and apply on `Apply`.
///
/// Why: The single place the #1316 safety property is enforced at the command
/// layer — it threads `decide_apply` between "what's available" and "do it".
///
/// What: Lists candidates (human/stderr), computes the [`ApplyInputs`] from
/// `--yes` + the TTY + (when prompting) an interactive yes/no, and matches the
/// resulting [`ApplyDecision`] into an [`UpgradeReport`]. Only `Apply` mutates.
///
/// Test: `tests::run_check_is_readonly`; the apply branch is side-effecting.
fn resolve_and_apply(
    handle: &Handle,
    candidates: Vec<UpdateCandidate>,
    yes: bool,
    json: bool,
) -> UpgradeReport {
    let has_candidates = !candidates.is_empty();

    if has_candidates && !json {
        let narr = narrator(json);
        let _ = narr.info(&format!("{} update(s) available:", candidates.len()));
        for c in &candidates {
            let installed = c.installed.as_deref().unwrap_or("(absent)");
            let _ = narr.info(&format!("  {} {} → {}", c.crate_name, installed, c.latest));
        }
    }

    // Only prompt when we actually need to (candidates, no --yes, on a TTY).
    let interactive_consent = if has_candidates && !yes && is_tty() && !json {
        prompt_yes_no("Apply these upgrades now?")
    } else {
        false
    };

    let decision = decide_apply(ApplyInputs {
        has_candidates,
        yes,
        is_tty: is_tty(),
        interactive_consent,
    });

    match decision {
        ApplyDecision::NothingToDo => UpgradeReport::non_applying("nothing_to_do", candidates),
        ApplyDecision::NeedsConfirmation => {
            UpgradeReport::non_applying("needs_confirmation", candidates)
        }
        ApplyDecision::Decline => UpgradeReport::non_applying("declined", candidates),
        ApplyDecision::Apply => {
            let outcomes = handle.block_on(apply_all(&candidates, json));
            UpgradeReport::applied(candidates, outcomes)
        }
    }
}

/// Apply every candidate upgrade, restarting daemons cleanly.
///
/// Why: The async apply core; daemons must restart via the connection-safe path.
/// What: For each candidate, daemons go through `upgrade_and_restart` (which may
/// self-restart under launchd — but tctl is not itself launchd-supervised, so it
/// returns the restart hint); non-daemons through `perform_upgrade` +
/// `verify_installed_binary`. Renders a per-member narration line + a final
/// component table.
/// Test: Side-effecting; the report shaping is tested via `UpgradeReport`.
async fn apply_all(candidates: &[UpdateCandidate], json: bool) -> Vec<UpgradeOutcome> {
    let narr = narrator(json);
    let mut tracker = ComponentTracker::new(narr.output());
    let mut outcomes = Vec::with_capacity(candidates.len());

    for c in candidates {
        let _ = narr.info(&format!("upgrading {} → {}", c.crate_name, c.latest));
        let result = if c.daemon {
            trusty_common::update::upgrade_and_restart(&c.crate_name, &c.binary)
                .await
                .map(|hint| hint.unwrap_or_else(|| "restarted".to_owned()))
        } else {
            match trusty_common::update::perform_upgrade(&c.crate_name).await {
                Ok(()) => trusty_common::update::verify_installed_binary(&c.binary)
                    .await
                    .map(|()| format!("upgraded to {}", c.latest)),
                Err(e) => Err(e),
            }
        };

        match result {
            Ok(detail) => {
                outcomes.push(UpgradeOutcome {
                    member: c.crate_name.clone(),
                    ok: true,
                    detail,
                });
                tracker.add(Component::new(c.binary.clone(), 0));
            }
            Err(e) => {
                let _ = narr.error(&format!("{}: {e}", c.crate_name));
                outcomes.push(UpgradeOutcome {
                    member: c.crate_name.clone(),
                    ok: false,
                    detail: e.to_string(),
                });
            }
        }
    }
    if !json {
        let _ = tracker.print();
    }
    outcomes
}

/// Render the human-readable upgrade summary footer.
///
/// Why: Operators want a clear verdict after the confirm-then-apply flow.
/// What: Branches on the report status to print the right one-liner.
/// Test: Side-effect-only; the data is tested via `UpgradeReport`.
fn print_human(report: &UpgradeReport) {
    match report.status {
        "nothing_to_do" => println!("All stable-set tools are up to date."),
        "needs_confirmation" => eprintln!(
            "tctl upgrade: {} update(s) available but not applied — re-run on a terminal or pass --yes to confirm.",
            report.candidates.len()
        ),
        "declined" => eprintln!("tctl upgrade: aborted (no confirmation)."),
        "applied" => {
            let ok = report.members.iter().filter(|m| m.ok).count();
            println!("upgraded {}/{}", ok, report.members.len());
            for m in report.members.iter().filter(|m| !m.ok) {
                eprintln!("  failed: {} — {}", m.member, m.detail);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate() -> UpdateCandidate {
        UpdateCandidate {
            crate_name: "trusty-search".to_owned(),
            binary: "trusty-search".to_owned(),
            installed: Some("0.19.0".to_owned()),
            latest: "0.20.0".to_owned(),
            daemon: true,
        }
    }

    /// Why: The non-applying envelope is a public contract; pin its shape.
    /// What: Builds a needs-confirmation report; asserts keys.
    /// Test: This is the test.
    #[test]
    fn report_serialises() {
        let report = UpgradeReport::non_applying("needs_confirmation", vec![candidate()]);
        let v = serde_json::to_value(&report).expect("serialises");
        assert_eq!(v["command"], "upgrade");
        assert_eq!(v["status"], "needs_confirmation");
        assert_eq!(v["candidates"][0]["crate_name"], "trusty-search");
        assert_eq!(v["members"].as_array().expect("array").len(), 0);
    }

    /// Why: An applied report's `all_ok` must reflect member outcomes.
    /// What: Mixes ok + failed; asserts all_ok false and status "applied".
    /// Test: This is the test.
    #[test]
    fn applied_report_all_ok() {
        let report = UpgradeReport::applied(
            vec![candidate()],
            vec![
                UpgradeOutcome {
                    member: "a".to_owned(),
                    ok: true,
                    detail: String::new(),
                },
                UpgradeOutcome {
                    member: "b".to_owned(),
                    ok: false,
                    detail: "boom".to_owned(),
                },
            ],
        );
        assert!(!report.all_ok);
        assert_eq!(report.status, "applied");
    }

    /// Why: Exit codes are the automation contract; pin each status mapping.
    /// What: Asserts the four status → exit-code mappings.
    /// Test: This is the test.
    #[test]
    fn exit_codes_per_status() {
        assert_eq!(
            UpgradeReport::non_applying("nothing_to_do", vec![]).exit_code(),
            0
        );
        assert_eq!(
            UpgradeReport::non_applying("needs_confirmation", vec![candidate()]).exit_code(),
            3
        );
        assert_eq!(
            UpgradeReport::non_applying("declined", vec![candidate()]).exit_code(),
            4
        );
        let applied_ok = UpgradeReport::applied(
            vec![candidate()],
            vec![UpgradeOutcome {
                member: "a".to_owned(),
                ok: true,
                detail: String::new(),
            }],
        );
        assert_eq!(applied_ok.exit_code(), 0);
    }

    /// Why: An unknown member must fail fast at selection (exit 3) and never
    /// reach the network/apply path — a pure, offline-safe guard test.
    /// What: Calls `run` with a bogus member in `--json` mode; asserts exit 3.
    /// Test: This is the test.
    #[test]
    fn run_unknown_member_is_error() {
        let code = run(false, false, false, true, &["not-a-tool".to_owned()], true);
        assert_eq!(code, 3);
    }

    /// Why: `--check` must be read-only (delegates to `updates`) and never reach
    /// the apply path. This performs a live crates.io probe, so it is
    /// `#[ignore]`-tagged to keep CI fast and offline-deterministic.
    /// What: Calls `run` with `check = true`; asserts exit 0 (read-only).
    /// Test: `cargo test -p trusty-controller -- --include-ignored`.
    #[test]
    #[ignore = "performs a live crates.io probe; run with --include-ignored"]
    fn run_check_is_readonly() {
        let code = run(true, false, false, false, &["tga".to_owned()], true);
        assert_eq!(code, 0);
    }
}
