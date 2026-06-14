//! Public types for the scrubber: [`ScrubChange`], [`ScrubResult`], and
//! [`MAX_BODY_BYTES`].
//!
//! Why: isolating the public data types from the implementation helpers keeps
//! the scrubber's API surface easy to find and stable.
//! What: defines the two output types returned by [`super::scrub`] and the
//! constant controlling body truncation.
//! Test: asserted by `tests::scrub_result_summary` and other `tests::*`.

/// Maximum filed body size (16 KiB — generous but well below GitHub's 65 536 B).
pub const MAX_BODY_BYTES: usize = 16 * 1024;

/// Description of one scrubbing substitution made in the text.
///
/// Why: the preview surfaced to the user before filing should enumerate exactly
///      what was removed so they can make an informed consent decision.
/// What: `pattern` names the rule (e.g. `"AbsolutePath"`, `"BearerToken"`);
///       `hint` is a brief human-readable note.
/// Test: returned by `scrub` and inspected in `tests::*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrubChange {
    /// Short name for the scrubbing rule that fired (e.g. `"AbsolutePath"`).
    pub pattern: &'static str,
    /// Human-readable hint about what was removed (e.g. `"1 absolute path(s)"`).
    pub hint: String,
}

/// The result of a scrubbing pass.
///
/// Why: callers (preview builder, filing path) need both the cleaned string and
///      a structured summary to display in the consent UI.
/// What: `text` is the scrubbed string (ready to file); `changes` is the list
///       of [`ScrubChange`] records; `redaction_summary` is a compact
///       human-readable string such as `"12 secrets, 3 paths redacted"` that
///       the preview can surface without listing every change.
/// Test: asserted in `tests::scrub_result_summary`.
#[derive(Debug, Clone)]
pub struct ScrubResult {
    /// The scrubbed string, truncated to at most [`MAX_BODY_BYTES`] bytes.
    pub text: String,
    /// Ordered list of every substitution that was applied.
    pub changes: Vec<ScrubChange>,
    /// Compact human-readable summary, e.g. `"5 secrets, 2 paths redacted"`.
    pub redaction_summary: String,
}
