//! `tctl updates` — read-only listing of available updates (#1316, DOC-9 §1).
//!
//! Why: The read-only companion to `tctl upgrade`. It surfaces which stable-set
//! members have a newer published version *without performing any mutation* —
//! the safe "what would change?" view, and the data the opt-in auto-update check
//! reuses.
//!
//! What: Probes crates.io for each stable-set member via
//! `update_engine::gather_candidates`, then renders the installed→latest diff —
//! either a human table (rustup-style narration) or a `--json` envelope. Never
//! mutates anything. Returns exit 0 always (read-only).
//!
//! Test: `tests::report_serialises` covers the JSON shape; the network probe is
//! side-effecting.

use serde::Serialize;

use super::runtime::block_on;
use super::stable_set::stable_set;
use super::update_engine::{gather_candidates, UpdateCandidate};
use crate::output::render_json;

/// The `tctl updates` report.
///
/// Why: A typed rollup keeps the machine output stable; the human path renders
/// the same data.
/// What: Holds the upgrade candidates and the total count.
/// Test: `tests::report_serialises`.
#[derive(Clone, Debug, Serialize)]
pub struct UpdatesReport {
    /// Fixed command tag for JSON consumers.
    pub command: &'static str,
    /// Members with a newer version available.
    pub candidates: Vec<UpdateCandidate>,
    /// Number of candidates (sugar for consumers).
    pub count: usize,
}

impl UpdatesReport {
    /// Build a report from gathered candidates.
    ///
    /// Why: Centralises the `count` derivation.
    /// What: Sets `command = "updates"` and `count = candidates.len()`.
    /// Test: `tests::report_serialises`.
    fn build(candidates: Vec<UpdateCandidate>) -> Self {
        Self {
            command: "updates",
            count: candidates.len(),
            candidates,
        }
    }
}

/// Handle `tctl updates [--latest]`.
///
/// Why: Phase-1 read-only update listing (#1316 priority 2).
///
/// What: Gathers candidates across the stable set and renders them. `latest` is
/// accepted for surface compatibility; the listing always reports the latest
/// crates.io release (there is no separate BOM pin in Phase 1, so `--latest` and
/// the default are equivalent today). Returns 0 (read-only, never fails the
/// process on "updates available") — except exit 1 if a `--json` write to stdout
/// fails, so automation never reads false success from a dropped JSON payload.
///
/// Test: Side-effecting (network); the report shaping is tested via `UpdatesReport`.
pub fn run(latest: bool, json: bool) -> i32 {
    // TODO(#1316): honour --latest once BOM/version pinning lands
    let _ = latest; // Phase 1: latest == default (no BOM pin yet).
    let report = block_on(gather());
    if json {
        // A failed machine-readable write must not exit 0: automation would
        // read success from the exit code while the JSON never arrived.
        if render_json(&report).is_err() {
            eprintln!("tctl updates: failed to write JSON output");
            return 1;
        }
    } else {
        print_human(&report);
    }
    0
}

/// Probe the stable set for candidates and build the report.
///
/// Why: The async core, isolated from the sync CLI shell.
/// What: Calls `gather_candidates(stable_set())`.
/// Test: Side-effecting; covered indirectly.
async fn gather() -> UpdatesReport {
    let candidates = gather_candidates(&stable_set()).await;
    UpdatesReport::build(candidates)
}

/// Render the human-readable updates listing to stdout.
///
/// Why: Operators want a quick scan of what is stale.
/// What: Prints one line per candidate (`name installed → latest`), or a
/// "everything up to date" line when there are none.
/// Test: Side-effect-only; the data is tested via `UpdatesReport`.
fn print_human(report: &UpdatesReport) {
    if report.candidates.is_empty() {
        println!("All stable-set tools are up to date.");
        return;
    }
    println!("{} update(s) available:", report.count);
    for c in &report.candidates {
        let installed = c.installed.as_deref().unwrap_or("(not installed)");
        println!("  {}  {} → {}", c.crate_name, installed, c.latest);
    }
    println!("Run `tctl upgrade` to apply (you will be asked to confirm).");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::update_engine::UpdateCandidate;

    /// Why: The JSON envelope is a public contract; pin its shape and count.
    /// What: Builds a report with one candidate and asserts the keys.
    /// Test: This is the test.
    #[test]
    fn report_serialises() {
        let report = UpdatesReport::build(vec![UpdateCandidate {
            crate_name: "trusty-search".to_owned(),
            binary: "trusty-search".to_owned(),
            installed: Some("0.19.0".to_owned()),
            latest: "0.20.0".to_owned(),
            daemon: true,
        }]);
        let v = serde_json::to_value(&report).expect("serialises");
        assert_eq!(v["command"], "updates");
        assert_eq!(v["count"], 1);
        assert_eq!(v["candidates"][0]["latest"], "0.20.0");
    }

    /// Why: An empty candidate list must report count 0 cleanly.
    /// What: Builds an empty report; asserts count 0.
    /// Test: This is the test.
    #[test]
    fn empty_report_count_zero() {
        let report = UpdatesReport::build(vec![]);
        assert_eq!(report.count, 0);
    }
}
