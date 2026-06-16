//! `tctl install` — install the agreed STABLE trusty tool set (#1316, DOC-8).
//!
//! Why: The one-time entry point that brings a machine to a fully-installed
//! stack. Installs the canonical, topologically-ordered stable set as a unit so
//! the platform comes up coherent rather than drifting member-by-member.
//! Idempotent: an already-installed member is re-run through `cargo install`
//! (cheap no-op when current) and reported.
//!
//! What: Resolves the requested members against [`super::stable_set`], installs
//! each in order via `trusty_common::update::perform_upgrade` (= `cargo install
//! <crate> --locked`), health-gates each freshly-installed binary with
//! `verify_installed_binary`, and renders rustup-style per-tool component rows
//! via `trusty-progress`. Honours `--yes` (non-interactive) and `--json`
//! (machine output). Returns a process exit code: 0 all installed, 2 one or more
//! failed.
//!
//! Test: `tests` covers the JSON envelope shaping (`build_report`) and the
//! unknown-member handling; the network/`cargo install` path is side-effecting
//! and validated manually.

use serde::Serialize;
use trusty_progress::{Component, ComponentTracker};

use super::progress_ui::{narrator, prompt_yes_no};
use super::runtime::block_on;
use super::stable_set::{select_members, StableMember};
use crate::output::render_json;

/// One member's install outcome for the `--json` report.
///
/// Why: A typed per-member result keeps the machine output stable and testable.
/// What: `member` is the crate name; `ok` whether it installed + health-gated;
/// `detail` a human note (e.g. the error on failure).
/// Test: `tests::report_serialises`.
#[derive(Clone, Debug, Serialize)]
pub struct InstallOutcome {
    /// Crate name installed.
    pub member: String,
    /// Whether install + health-gate succeeded.
    pub ok: bool,
    /// Human detail / error message.
    pub detail: String,
}

/// The aggregate install report.
///
/// Why: `--json` consumers want the whole rollup in one object with an overall
/// verdict; the human path renders the same data as a component table.
/// What: Holds per-member outcomes and the computed `all_ok`.
/// Test: `tests::report_serialises`, `tests::report_all_ok`.
#[derive(Clone, Debug, Serialize)]
pub struct InstallReport {
    /// Fixed command tag for JSON consumers.
    pub command: &'static str,
    /// Per-member install outcomes in install order.
    pub members: Vec<InstallOutcome>,
    /// Whether every member installed successfully.
    pub all_ok: bool,
}

impl InstallReport {
    /// Build a report from per-member outcomes.
    ///
    /// Why: Centralises the `all_ok` derivation so the exit code and JSON agree.
    /// What: Sets `command = "install"` and `all_ok = every outcome ok`.
    /// Test: `tests::report_all_ok`.
    fn build(members: Vec<InstallOutcome>) -> Self {
        let all_ok = members.iter().all(|m| m.ok);
        Self {
            command: "install",
            members,
            all_ok,
        }
    }

    /// Process exit code: 0 when all installed, 2 when any failed.
    ///
    /// Why: A boot/automation script branches on this.
    /// What: `0` if `all_ok`, else `2` (partial/total failure — stack degraded).
    /// Test: `tests::exit_code_reflects_all_ok`.
    fn exit_code(&self) -> i32 {
        if self.all_ok {
            0
        } else {
            2
        }
    }
}

/// Handle `tctl install [<members>…]`.
///
/// Why: Phase-1 entry point for the system install flow (#1316 priority 1).
///
/// What: Resolves `members` against the stable set (empty = all). On a TTY
/// without `--yes`, confirms the blast radius before installing. Installs each
/// member in topological order, rendering progress rows; emits either the human
/// component table + summary or the `--json` [`InstallReport`]. Returns the
/// process exit code.
///
/// Test: `tests::run_unknown_member_is_error`; the install path itself is
/// side-effecting (`cargo install`) and validated manually.
pub fn run(members: &[String], yes: bool, json: bool) -> i32 {
    let (selected, unknown) = select_members(members);

    if !unknown.is_empty() {
        let msg = format!("unknown member(s): {}", unknown.join(", "));
        if json {
            if render_json(&serde_json::json!({
                "command": "install",
                "error": msg,
            }))
            .is_err()
            {
                eprintln!("tctl install: failed to write JSON output");
                return 1;
            }
        } else {
            eprintln!("tctl install: {msg}");
        }
        return 3;
    }

    // Confirm the blast radius unless --yes or non-interactive (#1316 default-safe).
    if !yes && super::progress_ui::is_tty() && !json {
        let names: Vec<&str> = selected.iter().map(|m| m.crate_name.as_str()).collect();
        let q = format!("Install {} tool(s): {}? ", selected.len(), names.join(", "));
        if !prompt_yes_no(&q) {
            eprintln!("tctl install: aborted (no confirmation).");
            return 3;
        }
    }

    let report = block_on(install_all(&selected, json));
    if json {
        // A failed machine-readable write must not exit 0: automation would
        // read success from the exit code while the JSON never arrived.
        if render_json(&report).is_err() {
            eprintln!("tctl install: failed to write JSON output");
            return 1;
        }
    } else {
        print_human_summary(&report);
    }
    report.exit_code()
}

/// Install every selected member in order, returning the aggregate report.
///
/// Why: The async core, separated from the sync CLI shell so the runtime is
/// owned in exactly one place (`run`).
/// What: For each member, runs `perform_upgrade` then `verify_installed_binary`,
/// rendering a `trusty-progress` narration line per member and a final component
/// table of the installed binaries (with on-disk sizes when resolvable).
/// Test: Side-effecting; the report shaping is tested via `InstallReport`.
async fn install_all(selected: &[StableMember], json: bool) -> InstallReport {
    let narr = narrator(json);
    let _ = narr.info(&format!("installing {} component(s)", selected.len()));

    let mut outcomes = Vec::with_capacity(selected.len());
    let mut tracker = ComponentTracker::new(narr.output());

    for m in selected {
        let _ = narr.info(&format!("installing {}", m.crate_name));
        match install_one(m).await {
            Ok(()) => {
                outcomes.push(InstallOutcome {
                    member: m.crate_name.clone(),
                    ok: true,
                    detail: "installed".to_owned(),
                });
                tracker.add(Component::new(m.binary.clone(), binary_size(&m.binary)));
            }
            Err(e) => {
                let _ = narr.error(&format!("{}: {e}", m.crate_name));
                outcomes.push(InstallOutcome {
                    member: m.crate_name.clone(),
                    ok: false,
                    detail: e.to_string(),
                });
            }
        }
    }

    // The component table (rustup-style) summarising what landed.
    if !json {
        let _ = tracker.print();
    }
    InstallReport::build(outcomes)
}

/// Install + health-gate a single member.
///
/// Why: One member's full install step, composed from the shared primitives.
/// What: `perform_upgrade(crate)` then `verify_installed_binary(binary)`.
/// Test: Side-effecting; covered indirectly.
async fn install_one(m: &StableMember) -> anyhow::Result<()> {
    trusty_common::update::perform_upgrade(&m.crate_name).await?;
    trusty_common::update::verify_installed_binary(&m.binary).await
}

/// Best-effort on-disk size of an installed binary (for the component row).
///
/// Why: The rustup-style table shows a size column; we look up the installed
/// file's size, defaulting to 0 when it cannot be resolved (the row still renders).
/// What: Checks `~/.cargo/bin/<binary>`; returns its byte length or 0.
/// Test: Side-effect-only (filesystem); the table layout is tested in trusty-progress.
fn binary_size(binary: &str) -> u64 {
    dirs::home_dir()
        .map(|h| h.join(".cargo").join("bin").join(binary))
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|md| md.len())
        .unwrap_or(0)
}

/// Print the human-readable install summary footer.
///
/// Why: After the component table, a one-line verdict tells the operator whether
/// everything landed.
/// What: Prints `installed N/M` and lists any failures to stderr.
/// Test: Side-effect-only; the data it reads is tested via `InstallReport`.
fn print_human_summary(report: &InstallReport) {
    let ok = report.members.iter().filter(|m| m.ok).count();
    let narr = narrator(false);
    let _ = narr.info(&format!("installed {}/{}", ok, report.members.len()));
    for m in report.members.iter().filter(|m| !m.ok) {
        let _ = narr.error(&format!("{}: {}", m.member, m.detail));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: The JSON envelope is a public contract; pin its shape.
    /// What: Builds a report and asserts the serialised keys/values.
    /// Test: This is the test.
    #[test]
    fn report_serialises() {
        let report = InstallReport::build(vec![InstallOutcome {
            member: "trusty-search".to_owned(),
            ok: true,
            detail: "installed".to_owned(),
        }]);
        let v = serde_json::to_value(&report).expect("serialises");
        assert_eq!(v["command"], "install");
        assert_eq!(v["all_ok"], true);
        assert_eq!(v["members"][0]["member"], "trusty-search");
    }

    /// Why: `all_ok` must be false if any member failed.
    /// What: Mixes an ok and a failed outcome; asserts `all_ok = false`.
    /// Test: This is the test.
    #[test]
    fn report_all_ok() {
        let report = InstallReport::build(vec![
            InstallOutcome {
                member: "a".to_owned(),
                ok: true,
                detail: String::new(),
            },
            InstallOutcome {
                member: "b".to_owned(),
                ok: false,
                detail: "boom".to_owned(),
            },
        ]);
        assert!(!report.all_ok);
    }

    /// Why: The exit code must track the verdict for automation.
    /// What: Asserts 0 for all-ok and 2 for a failure.
    /// Test: This is the test.
    #[test]
    fn exit_code_reflects_all_ok() {
        let ok = InstallReport::build(vec![InstallOutcome {
            member: "a".to_owned(),
            ok: true,
            detail: String::new(),
        }]);
        assert_eq!(ok.exit_code(), 0);
        let bad = InstallReport::build(vec![InstallOutcome {
            member: "a".to_owned(),
            ok: false,
            detail: "x".to_owned(),
        }]);
        assert_eq!(bad.exit_code(), 2);
    }

    /// Why: An unknown member must be a clean error (exit 3), not a silent skip.
    /// What: Calls `run` with a bogus member in `--json` mode; asserts exit 3.
    /// Test: This is the test.
    #[test]
    fn run_unknown_member_is_error() {
        let code = run(&["not-a-real-tool".to_owned()], true, true);
        assert_eq!(code, 3);
    }
}
