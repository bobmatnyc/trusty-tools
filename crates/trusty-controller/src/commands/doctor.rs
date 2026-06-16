//! `tctl doctor --self-check <member>` — conformance self-check of a member.
//!
//! Why: The controller-side conformance audit that validates a member speaks the
//! DOC-1 capability-discovery contract: `version --json` returns an envelope with
//! `contract_version` and a non-empty `verbs[]` (DOC-1 D3b / DOC-6 §8).
//!
//! What: invokes `<binary> version --json`, validates the envelope via
//! `probe::validate_version_envelope`, and reports conformance (human or
//! `--json`). Per-member `doctor` (raw envelope) is reached via the generic
//! passthrough (`tctl <tool> doctor`), not this command — this is exclusively the
//! controller's own conformance audit (DOC-5 §1.4).
//!
//! Test: `tests` covers the result shaping + the missing-member arg guard; the
//! `version --json` spawn is side-effecting (the validation it feeds is tested in
//! `probe`).

use serde::Serialize;

use super::probe::{spawn_member_json, validate_version_envelope, VersionConformance};
use crate::output::render_json;

/// The self-check result for a member.
///
/// Why: a typed result keeps the `--json` output stable and lets the handler
/// render either format from the same data.
/// What: the member name, whether the spawn/parse succeeded, the conformance
/// breakdown (when it did), and a human detail string (the error when it didn't).
/// Test: `tests::result_serialises`.
#[derive(Clone, Debug, Serialize)]
pub struct SelfCheckResult {
    /// Fixed command tag.
    pub command: &'static str,
    /// Member audited.
    pub member: String,
    /// Whether `version --json` was obtained and validated.
    pub probed: bool,
    /// The DOC-1 conformance breakdown (present iff `probed`).
    pub conformance: Option<VersionConformance>,
    /// Human detail / error message.
    pub detail: String,
}

impl SelfCheckResult {
    /// Process exit code: 0 conformant, 2 non-conformant / probe failure.
    ///
    /// Why: CI gates on this.
    /// What: `0` only when probed AND conformant; else `2`.
    /// Test: `tests::exit_code_reflects_conformance`.
    fn exit_code(&self) -> i32 {
        match &self.conformance {
            Some(c) if c.conformant => 0,
            _ => 2,
        }
    }
}

/// Handle `tctl doctor [--self-check <member>]`.
///
/// Why: Phase-2 entry point for the conformance self-check (DOC-6 §8).
/// What: requires `--self-check` AND a member; spawns `<member> version --json`,
/// validates the envelope, and renders the result. Without `--self-check`, or
/// without a member, returns a usage error (exit 3). The per-member raw `doctor`
/// is the passthrough's job, not this command.
/// Test: side-effecting (subprocess); the validation is tested in `probe` and the
/// result shaping via `SelfCheckResult`.
pub fn run(self_check: bool, member: Option<&str>, json: bool) -> i32 {
    let Some(name) = member else {
        if self_check {
            eprintln!("tctl doctor --self-check: a member name is required");
        } else {
            eprintln!(
                "tctl doctor: use `--self-check <member>` for the controller's \
                 conformance audit, or `tctl <member> doctor` for a member's own doctor"
            );
        }
        return 3;
    };

    let result = match spawn_member_json(name, "version") {
        Ok(v) => {
            let conformance = validate_version_envelope(&v);
            let detail = if conformance.conformant {
                format!("conformant ({} verbs advertised)", conformance.verb_count)
            } else {
                format!(
                    "non-conformant (contract_version:{}, verbs:{})",
                    conformance.has_contract_version, conformance.has_verbs
                )
            };
            SelfCheckResult {
                command: "doctor --self-check",
                member: name.to_owned(),
                probed: true,
                conformance: Some(conformance),
                detail,
            }
        }
        Err(e) => SelfCheckResult {
            command: "doctor --self-check",
            member: name.to_owned(),
            probed: false,
            conformance: None,
            detail: e.to_string(),
        },
    };

    if json {
        if render_json(&result).is_err() {
            eprintln!("tctl doctor: failed to write JSON output");
            return 1;
        }
    } else {
        println!(
            "tctl doctor --self-check {}: {}",
            result.member, result.detail
        );
    }
    result.exit_code()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conformant() -> VersionConformance {
        VersionConformance {
            conformant: true,
            has_contract_version: true,
            has_verbs: true,
            verb_count: 3,
        }
    }

    /// Why: the JSON result is a contract; pin its shape.
    /// What: builds a conformant result and asserts keys.
    /// Test: This is the test.
    #[test]
    fn result_serialises() {
        let r = SelfCheckResult {
            command: "doctor --self-check",
            member: "trusty-search".to_owned(),
            probed: true,
            conformance: Some(conformant()),
            detail: "conformant (3 verbs advertised)".to_owned(),
        };
        let v = serde_json::to_value(&r).expect("serialises");
        assert_eq!(v["command"], "doctor --self-check");
        assert_eq!(v["member"], "trusty-search");
        assert_eq!(v["conformance"]["conformant"], true);
    }

    /// Why: exit 0 only when probed AND conformant; everything else is exit 2.
    /// What: asserts the truth table.
    /// Test: This is the test.
    #[test]
    fn exit_code_reflects_conformance() {
        let ok = SelfCheckResult {
            command: "doctor --self-check",
            member: "a".to_owned(),
            probed: true,
            conformance: Some(conformant()),
            detail: String::new(),
        };
        assert_eq!(ok.exit_code(), 0);

        let non_conformant = SelfCheckResult {
            command: "doctor --self-check",
            member: "a".to_owned(),
            probed: true,
            conformance: Some(VersionConformance {
                conformant: false,
                has_contract_version: true,
                has_verbs: false,
                verb_count: 0,
            }),
            detail: String::new(),
        };
        assert_eq!(non_conformant.exit_code(), 2);

        let probe_failed = SelfCheckResult {
            command: "doctor --self-check",
            member: "a".to_owned(),
            probed: false,
            conformance: None,
            detail: "not installed".to_owned(),
        };
        assert_eq!(probe_failed.exit_code(), 2);
    }

    /// Why: a missing member must be a clean usage error (exit 3), not a crash.
    /// What: calls `run` with no member; asserts exit 3.
    /// Test: This is the test.
    #[test]
    fn missing_member_is_usage_error() {
        assert_eq!(run(true, None, true), 3);
        assert_eq!(run(false, None, false), 3);
    }
}
