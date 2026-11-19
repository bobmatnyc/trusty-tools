//! Exploitability / reachability question for security findings (#11526).
//!
//! Why: the verifier asked only whether a finding is grounded in the diff, so
//! a true-but-unreachable security finding came back CONFIRMED and was posted
//! as a real vulnerability. Owner ruling D52 adds the exploitability test from
//! the DryRun case study: can this be reached and abused given the cited
//! context? Non-security findings keep the prompt and schema they had.
//! What: [`is_security_finding`] picks the findings that get the question;
//! [`security_verify_response_schema`] adds `reachability` and
//! `reachability_reason` to the single-finding schema; [`parse_reachability`]
//! reads them back; [`apply_reachability`] turns a CONFIRMED-but-UNREACHABLE
//! answer into `Unverifiable`, whose reason carries the rationale.
//! Test: `verify_security_tests.rs`.

use serde::Deserialize;

use crate::{
    llm::ResponseSchema,
    models::{Finding, VerifyOutcome},
};

/// Name of the security verifier's forced-output schema.
///
/// Why: `verify_batch` reads the request's schema name to know whether the
/// answer carries a reachability field, so the name must differ from
/// `VERIFY_SCHEMA_NAME`.
pub const SECURITY_VERIFY_SCHEMA_NAME: &str = "security_verification_judgment";

/// Output-token floor for a security verification call.
///
/// Why: the answer carries a second sentence (the reachability rationale), and
/// the 128-token single-finding budget truncated a two-field answer once
/// already (#726).
pub(crate) const SECURITY_VERIFY_MAX_TOKENS: u32 = 256;

/// The question appended to a security finding's verifier message.
///
/// Why: DryRun's exploitability test (owner ruling D52, #11526).
/// What: asks for REACHABLE / UNREACHABLE / UNKNOWN plus one sentence of
/// rationale, and states that an unreachable finding is not confirmed.
pub(crate) const SECURITY_REACHABILITY_QUESTION: &str = "\n\n## Exploitability (security finding)\n\
This is a security finding. Besides the judgment, decide whether the flaw can \
actually be reached and abused given the diff and the context shown: is the \
vulnerable code on a path an attacker-controlled input can reach, with no \
check, guard, or caller restriction shown in the diff that blocks it? Set \
`reachability` to REACHABLE, UNREACHABLE (the code is real but no shown path \
lets an attacker reach or abuse it), or UNKNOWN (the evidence is not in the \
diff), and give one short sentence of rationale in `reachability_reason`. \
A finding you judge UNREACHABLE will not be confirmed.";

/// Whether `finding` is a security finding and gets the reachability question.
///
/// Why: the question only makes sense for an abuse claim; asking it of a
/// logic or style finding would invite the verifier to refute real bugs for
/// being "unexploitable".
/// What: true when the finding's `kind` contains `security` (case-insensitive),
/// the kind the reviewer schema uses for security findings.
/// Test: `security_kind_detection`.
pub(crate) fn is_security_finding(finding: &Finding) -> bool {
    finding.kind.to_ascii_lowercase().contains("security")
}

/// The single-finding verifier schema plus the two reachability fields.
///
/// Why: the rationale must come back structured so it can be recorded on the
/// outcome instead of being guessed from prose.
/// What: `judgment` and `reason` as in `verify_response_schema`, plus
/// `reachability` ∈ {REACHABLE, UNREACHABLE, UNKNOWN} and `reachability_reason`.
/// Built through `ResponseSchema::new`, which applies strict mode.
/// Test: `security_schema_enumerates_reachability`.
pub fn security_verify_response_schema() -> ResponseSchema {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "judgment": {
                "type": "string",
                "enum": ["CONFIRMED", "REFUTED", "UNVERIFIABLE"],
                "description": "Verification judgment for the finding"
            },
            "reason": {
                "type": "string",
                "description": "One short sentence justifying the judgment"
            },
            "reachability": {
                "type": "string",
                "enum": ["REACHABLE", "UNREACHABLE", "UNKNOWN"],
                "description": "Whether an attacker can reach and abuse the flaw"
            },
            "reachability_reason": {
                "type": "string",
                "description": "One short sentence justifying the reachability"
            }
        }
    });
    ResponseSchema::new(SECURITY_VERIFY_SCHEMA_NAME, schema)
}

/// The verifier's reachability answer for a security finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reachability {
    /// An attacker can reach and abuse the flaw.
    Reachable,
    /// The flaw is real but no shown path lets an attacker reach it.
    Unreachable,
    /// The evidence is not in the diff.
    Unknown,
}

/// The reachability fields of a security verifier answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReachabilityAnswer {
    /// The parsed reachability.
    pub reachability: Reachability,
    /// The verifier's one-sentence rationale (may be empty).
    pub rationale: String,
}

#[derive(Deserialize)]
struct RawReachability {
    #[serde(default)]
    reachability: String,
    #[serde(default)]
    reachability_reason: String,
}

/// Read the reachability fields from a security verifier answer.
///
/// What: `None` when the answer is not whole JSON or carries no recognised
/// `reachability` value; the caller then keeps the judgment unchanged.
/// Test: `parse_reachability_reads_fields`.
pub(crate) fn parse_reachability(text: &str) -> Option<ReachabilityAnswer> {
    let raw: RawReachability = serde_json::from_str(text.trim()).ok()?;
    let reachability = match raw.reachability.trim().to_ascii_uppercase().as_str() {
        "REACHABLE" => Reachability::Reachable,
        "UNREACHABLE" => Reachability::Unreachable,
        "UNKNOWN" => Reachability::Unknown,
        _ => return None,
    };
    Some(ReachabilityAnswer {
        reachability,
        rationale: raw.reachability_reason.trim().to_string(),
    })
}

/// Combine a judgment with the reachability answer.
///
/// Why: an unreachable-but-true security finding must not be posted as
/// CONFIRMED (#11526). `Unverifiable` is the non-confirming outcome that does
/// not claim the code is correct, and #8904 withholds it from the posted set.
/// What: CONFIRMED + UNREACHABLE → `Unverifiable` whose reason names the
/// rationale; every other pair is returned unchanged. The second value is the
/// note to record on a finding that stays CONFIRMED.
/// Test: `unreachable_confirmed_security_finding_is_not_confirmed`.
pub(crate) fn apply_reachability(
    outcome: VerifyOutcome,
    answer: Option<&ReachabilityAnswer>,
) -> (VerifyOutcome, Option<String>) {
    let Some(answer) = answer else {
        return (outcome, None);
    };
    let rationale = if answer.rationale.is_empty() {
        "no rationale given"
    } else {
        answer.rationale.as_str()
    };
    match (outcome, answer.reachability) {
        (VerifyOutcome::Confirmed, Reachability::Unreachable) => (
            VerifyOutcome::Unverifiable {
                reason: format!(
                    "security finding not confirmed: the verifier judged it unreachable — \
                     {rationale}"
                ),
            },
            None,
        ),
        (VerifyOutcome::Confirmed, reach) => {
            let label = match reach {
                Reachability::Reachable => "reachable",
                _ => "reachability unknown",
            };
            (
                VerifyOutcome::Confirmed,
                Some(format!(
                    "Verifier exploitability check: {label} — {rationale}"
                )),
            )
        }
        (other, _) => (other, None),
    }
}

/// Record a CONFIRMED security finding's reachability note on the finding.
///
/// What: appends the note to `consequence` on its own line, so it travels
/// with the finding to every renderer.
pub(crate) fn record_note(finding: &mut Finding, note: &str) {
    if finding.consequence.is_empty() {
        finding.consequence = note.to_string();
    } else {
        finding.consequence = format!("{}\n{note}", finding.consequence);
    }
}

#[cfg(test)]
#[path = "verify_security_tests.rs"]
mod tests;
