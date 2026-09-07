//! `trusty-review version` — the DOC-1 capability-discovery envelope tctl reads.
//!
//! Why (#6913): `tctl doctor --self-check trusty-review` spawns
//! `trusty-review version --json` and requires `contract_version` plus a
//! non-empty `verbs[]` at the TOP LEVEL of that reply
//! (`trusty-installer::commands::probe::validate_version_envelope`,
//! ADR-0007/DOC-1). trusty-review had no `version` subcommand, so clap exited
//! 2 with a usage error and the probe reported
//! "trusty-review version --json exited with exit status: 2" before it could
//! parse anything. Same defect trusty-analyze carried until #6631; this is
//! that fix, ported unchanged.
//!
//! What: [`envelope`] builds the JSON object; [`run`] prints it, or a one-line
//! human summary without `--json`. `verbs` names only `"version"` itself —
//! `run --json` emits a serialised `ReviewResult` (`trusty_review::run_output`),
//! not a DOC-1 envelope, and advertising a verb this binary cannot answer
//! in-contract would be the opposite defect: a controller trusting `verbs[]`
//! (D3b) and getting a review payload instead.
//!
//! Test: `envelope_satisfies_the_doc1_self_check`,
//! `envelope_carries_the_crate_version`; the subprocess-level
//! `version_json_parses_and_carries_the_crate_version` in
//! `tests/version_cli.rs` proves the real binary, not just this function.

use serde_json::{Value, json};

/// DOC-1 `contract_version` baseline
/// (`docs/trusty-installer/research/02-design/01-tool-contract.md`,
/// ADR-0007). Bumped only for a non-additive envelope or verb-`data` shape
/// change — never for adding a verb (D3, restated in that doc's ledger).
const CONTRACT_VERSION: u32 = 1;

/// Build the `version --json` envelope tctl's self-check parses.
///
/// Why/What: see the module doc — `contract_version` and `verbs` sit at the
/// TOP level, not nested under a `data` key, because that is what
/// `validate_version_envelope` actually reads off the spawned process's
/// stdout.
/// Test: `envelope_satisfies_the_doc1_self_check`,
/// `envelope_carries_the_crate_version`.
pub fn envelope() -> Value {
    json!({
        "contract_version": CONTRACT_VERSION,
        "tool": "trusty-review",
        "tool_version": env!("CARGO_PKG_VERSION"),
        "verb": "version",
        "status": "ok",
        "verbs": ["version"],
        "messages": [],
    })
}

/// Entry point for `trusty-review version [--json]`.
///
/// What: `--json` prints [`envelope`]; otherwise a one-line human summary
/// matching the other trusty-* binaries' plain-text `version` output.
/// Test: `version_json_parses_and_carries_the_crate_version` and
/// `version_without_json_prints_the_crate_version` in `tests/version_cli.rs`.
pub fn run(json: bool) {
    if json {
        println!("{}", envelope());
    } else {
        println!("trusty-review v{}", env!("CARGO_PKG_VERSION"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why (#6913): this is the literal check `doctor --self-check` runs —
    /// pinning it here means a future edit that drops either field fails in
    /// this crate's own test run, not three hops away in trusty-installer's.
    /// What: `contract_version` is a positive integer and `verbs` is a
    /// non-empty array, both at the top level.
    /// Test: itself.
    #[test]
    fn envelope_satisfies_the_doc1_self_check() {
        let v = envelope();
        assert!(
            v["contract_version"].as_u64().is_some_and(|n| n >= 1),
            "contract_version must be a positive integer: {v}"
        );
        assert!(
            v["verbs"].as_array().is_some_and(|a| !a.is_empty()),
            "verbs must be a non-empty array: {v}"
        );
    }

    /// Why: tctl's self-check and any operator reading `version --json` need
    /// the crate's actual release version, not a placeholder.
    /// What: `tool_version` equals `CARGO_PKG_VERSION`, and `tool` names this
    /// binary rather than the crate this pattern was copied from.
    /// Test: itself.
    #[test]
    fn envelope_carries_the_crate_version() {
        let v = envelope();
        assert_eq!(v["tool_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(v["tool"], "trusty-review");
    }
}
