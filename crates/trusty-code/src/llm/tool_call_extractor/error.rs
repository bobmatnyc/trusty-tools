//! Error type for [`super::ToolCallExtractor`] (#1023).
//!
//! Why: Extraction and validation can fail in several distinct, actionable
//! ways (nothing found, malformed JSON, schema mismatch, unknown tool) plus
//! one terminal way (the bounded repair loop in `super::repair` ran out of
//! attempts). Callers — the agent loop's dispatch path today, and any future
//! caller — need to branch on which happened without string-matching, and the
//! repair loop needs a structured value to build its corrective message from.
//! What: Defines `ToolCallExtractError` deriving `thiserror::Error`. No
//! variant panics or unwraps on construction; this is purely a data carrier.
//! Test: `error::tests::*` — display strings for each variant.

use thiserror::Error;

use super::ExtractionStrategy;
use super::schema::SchemaViolation;
use crate::llm::LlmError;

/// Failure modes of tool-call extraction and validation.
///
/// Why: Distinguishes "no candidate call found at all" from "found a call but
/// its arguments are broken" from "found valid JSON but it fails the tool's
/// schema" — each warrants a different corrective message from
/// `repair::build_corrective_message`. `Unrepairable` is the terminal variant
/// the bounded repair loop returns once `max_attempts` is exhausted.
/// Test: Constructed and matched throughout `mod.rs`, `repair.rs` tests.
#[derive(Debug, Error)]
pub enum ToolCallExtractError {
    /// No strategy in the configured order recovered a tool call.
    ///
    /// Why: The model may have answered in plain prose with no embedded call
    /// at all, or used a convention none of the four strategies recognise.
    /// What: Carries the strategies that were attempted, in order, for
    /// diagnostics and for the corrective message.
    #[error("no tool call found in response (tried strategies: {tried:?})")]
    NoCallFound {
        /// Strategies attempted, in the order they were tried.
        tried: Vec<ExtractionStrategy>,
    },

    /// A candidate call's argument text failed to parse as JSON.
    ///
    /// Why: The native wire format carries `arguments` as a JSON-encoded
    /// string (see `llm::FunctionCall::arguments`); a model can emit
    /// syntactically broken JSON there even when the surrounding envelope is
    /// well-formed.
    /// What: Carries the tool `name` (best-effort — the name itself parses
    /// independently of the arguments) and the underlying `serde_json` error.
    #[error("tool call '{name}' arguments are not valid JSON: {source}")]
    MalformedArguments {
        /// The tool name the (unparseable) arguments were destined for.
        name: String,
        /// The underlying JSON parse error.
        source: serde_json::Error,
    },

    /// The extracted call names a tool with no registered schema.
    ///
    /// Why: Validation needs a schema to check against; a hallucinated tool
    /// name has none. This is distinct from `SchemaInvalid` because there is
    /// nothing to validate the arguments AGAINST, not merely a mismatch.
    /// What: Carries the unrecognised tool name.
    #[error("no schema registered for tool '{name}'")]
    UnknownTool {
        /// The hallucinated or misspelled tool name.
        name: String,
    },

    /// The extracted call's arguments failed schema validation.
    ///
    /// Why: The most common repairable failure — the model called a real
    /// tool but got the argument shape wrong (missing required field, wrong
    /// type, disallowed extra key).
    /// What: Carries the tool `name` and every [`SchemaViolation`] found (not
    /// just the first) so a single corrective message can address them all.
    #[error("tool call '{name}' failed schema validation: {violations:?}")]
    SchemaInvalid {
        /// The tool the arguments were destined for.
        name: String,
        /// All violations found; see `schema::validate_args`.
        violations: Vec<SchemaViolation>,
    },

    /// The retry callback's chat call failed (transport/API error).
    ///
    /// Why: `repair::extract_with_repair`'s retry closure typically wraps a
    /// real `LlmClientTrait::chat` call; that call can fail independently of
    /// extraction. Wrapping it here lets the repair loop propagate it with
    /// `?` instead of requiring every caller to define its own error type.
    /// What: Wraps the underlying `LlmError`.
    #[error("retry chat call failed: {0}")]
    Retry(#[from] LlmError),

    /// The bounded repair loop exhausted its attempts without success.
    ///
    /// Why: The terminal, structured (never-panicking) error the repair loop
    /// returns once `max_attempts` corrective round-trips have all failed —
    /// this is the "Unrepairable → structured error" acceptance criterion.
    /// What: Carries how many attempts were made and the last failure seen.
    #[error("unrepairable after {attempts} attempt(s): {last_error}")]
    Unrepairable {
        /// Number of repair attempts made (corrective round-trips), not
        /// counting the initial extraction attempt.
        attempts: u32,
        /// The failure from the final attempt.
        last_error: Box<ToolCallExtractError>,
    },
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `NoCallFound` lists the attempted strategies in its display string.
    ///
    /// Why: Diagnostics/logs should show what was tried without needing to
    /// inspect the struct fields.
    /// What: Construct with two strategies, assert both appear.
    /// Test: this test.
    #[test]
    fn no_call_found_display_lists_strategies() {
        let err = ToolCallExtractError::NoCallFound {
            tried: vec![
                ExtractionStrategy::FencedJson,
                ExtractionStrategy::AngleBracket,
            ],
        };
        let s = err.to_string();
        assert!(s.contains("FencedJson"));
        assert!(s.contains("AngleBracket"));
    }

    /// `SchemaInvalid` display includes the tool name.
    ///
    /// Why: Operators scanning logs need the tool name without expanding the
    /// full violation list.
    /// What: Construct with a name and one violation; assert name appears.
    /// Test: this test.
    #[test]
    fn schema_invalid_display_includes_name() {
        let err = ToolCallExtractError::SchemaInvalid {
            name: "bash".into(),
            violations: vec![SchemaViolation {
                path: "$.command".into(),
                message: "missing required property 'command'".into(),
            }],
        };
        assert!(err.to_string().contains("bash"));
    }

    /// `Unrepairable` display includes the attempt count and nests the source.
    ///
    /// Why: This is the terminal error surfaced to callers; both pieces of
    /// information must be visible without downcasting.
    /// What: Wrap an `UnknownTool` inside `Unrepairable`; assert both appear.
    /// Test: this test.
    #[test]
    fn unrepairable_display_includes_attempts_and_source() {
        let err = ToolCallExtractError::Unrepairable {
            attempts: 2,
            last_error: Box::new(ToolCallExtractError::UnknownTool {
                name: "frobnicate".into(),
            }),
        };
        let s = err.to_string();
        assert!(s.contains('2'));
        assert!(s.contains("frobnicate"));
    }
}
