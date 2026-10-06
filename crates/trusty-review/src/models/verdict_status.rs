//! What kind of outcome a review's verdict reports (#9310).
//!
//! Why: UNKNOWN used to cover a parse failure, a transport error, and a
//! review whose findings were all withheld, so a caller could not tell "no
//! judgment" from "a suppressed rejection", and clean PRs were held.
//! What: [`VerdictStatus`], a closed set serialized as a `snake_case` string,
//! so the JSON field stays a string.
//! Test: `verdict_status_serializes_as_snake_case_strings`,
//! `verdict_status_reads_the_retired_no_verified_findings`.

use serde::{Deserialize, Serialize};

/// The outcome class behind a review's verdict (#9310).
///
/// Why: the verdict alone cannot say whether the reviewer judged the change,
/// failed to answer, or was overruled by the withhold gates.
/// What: `Parsed` — the reviewer's own verdict, from output that parsed;
/// `ParseFailed` — the reply did not parse (UNKNOWN; the cause is in
/// `ReviewResult::error`); `NoReviewerOutput` — no reply at all: the call
/// failed, the reply was empty or truncated, or the review stopped before the
/// call (UNKNOWN); `AllWithheld` — the reviewer approved and every finding was
/// withheld (APPROVE / APPROVE*); `SuppressedReject` — the reviewer asked for
/// changes or blocked, and every finding supporting that was withheld
/// (REQUEST_CHANGES). The retired `"no_verified_findings"` string still reads
/// as `AllWithheld`, so a stored record from 0.38.0 or earlier deserializes.
/// Test: `verdict_status_serializes_as_snake_case_strings`,
/// `verdict_status_reads_the_retired_no_verified_findings`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum VerdictStatus {
    /// The reviewer's own verdict, from output that parsed.
    Parsed,
    /// The reviewer replied, and the reply did not parse.
    ParseFailed,
    /// No reviewer reply: a call error, an empty or truncated reply, or no call.
    NoReviewerOutput,
    /// The reviewer approved, and every finding was withheld.
    // #9310: read the #9188 string a stored record may still carry.
    #[serde(alias = "no_verified_findings")]
    AllWithheld,
    /// The reviewer rejected, and every finding supporting that was withheld.
    SuppressedReject,
}

impl VerdictStatus {
    /// The serialized string, e.g. `"all_withheld"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Parsed => "parsed",
            Self::ParseFailed => "parse_failed",
            Self::NoReviewerOutput => "no_reviewer_output",
            Self::AllWithheld => "all_withheld",
            Self::SuppressedReject => "suppressed_reject",
        }
    }
}

impl std::fmt::Display for VerdictStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::VerdictStatus;

    /// Every variant serializes to the string `as_str` names, and back.
    #[test]
    fn verdict_status_serializes_as_snake_case_strings() {
        for status in [
            VerdictStatus::Parsed,
            VerdictStatus::ParseFailed,
            VerdictStatus::NoReviewerOutput,
            VerdictStatus::AllWithheld,
            VerdictStatus::SuppressedReject,
        ] {
            let json = serde_json::to_value(status).expect("serialize");
            assert_eq!(json, serde_json::Value::String(status.to_string()));
            let back: VerdictStatus = serde_json::from_value(json).expect("deserialize");
            assert_eq!(back, status);
        }
    }

    /// #9310: a record written with the #9188 string still deserializes.
    #[test]
    fn verdict_status_reads_the_retired_no_verified_findings() {
        let status: VerdictStatus =
            serde_json::from_str("\"no_verified_findings\"").expect("deserialize");
        assert_eq!(status, VerdictStatus::AllWithheld);
    }
}
