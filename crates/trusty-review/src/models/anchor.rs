//! The per-finding anchor contract (#10762).
//!
//! Why: a consumer that posts every finding as an annotation needs, for each
//! finding, either an anchor or the reason it has none. `inline_comments`
//! carried no side, severity or finding identity, and nothing said why a
//! finding was not anchored.
//! What: [`AnchorSide`] and [`SubjectType`] qualify an inline anchor;
//! [`UnanchoredReason`] and [`UnanchoredFinding`] record a finding that has
//! no anchor. A `finding_index` counts `findings` first (0..n), then
//! `withheld_findings` (n..n+m), in their order.
//! Test: `unanchored_reason_enum_covers_no_line_off_diff_nit_cap_withheld`,
//! `partition_invariant_with_fixture`.

use serde::{Deserialize, Serialize};

/// The diff side an inline anchor addresses (#10762).
///
/// What: `RIGHT` is the new side (added and context lines); `LEFT` is the
/// old side, for a deleted line with no new-side match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum AnchorSide {
    /// The new side of the diff.
    #[default]
    Right,
    /// The old side of the diff.
    Left,
}

/// What an inline anchor is attached to, GitHub's `subject_type` (#10762).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubjectType {
    /// A line (or line range) of the diff.
    Line,
    /// The whole file.
    File,
}

/// Why a finding has no inline anchor (#10762).
///
/// Why: a consumer must tell "the line is not in the diff" from "the nit cap
/// suppressed it" from "a gate withheld it" without re-deriving the plan.
/// What: serializes as one string — `off_diff`, `no_line`, `deleted_line`,
/// `nit_cap`, or `withheld:<class>` where `<class>` is the
/// `withheld_contract::reason_class` of the withheld reason. An unknown
/// string deserializes as [`UnanchoredReason::Unknown`], the tolerance
/// `serde(other)` gives a unit enum; `serde(other)` itself cannot apply
/// because `withheld:<class>` carries data.
/// Test: `unanchored_reason_enum_covers_no_line_off_diff_nit_cap_withheld`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum UnanchoredReason {
    /// The finding cites a line that is not a commentable line of the diff.
    OffDiff,
    /// The finding cites no line.
    NoLine,
    /// The finding cites a line the change deletes.
    DeletedLine,
    /// A low-severity nit past `MAX_INLINE_NITS`; rendered in the summary.
    NitCap,
    /// A gate withheld the finding; the payload is its reason class.
    Withheld(String),
    /// A reason this version does not know.
    Unknown,
}

/// The `withheld:` prefix of a [`UnanchoredReason::Withheld`] string.
const WITHHELD_PREFIX: &str = "withheld:";

impl From<String> for UnanchoredReason {
    fn from(s: String) -> Self {
        match s.as_str() {
            "off_diff" => Self::OffDiff,
            "no_line" => Self::NoLine,
            "deleted_line" => Self::DeletedLine,
            "nit_cap" => Self::NitCap,
            other => match other.strip_prefix(WITHHELD_PREFIX) {
                Some(class) => Self::Withheld(class.to_string()),
                None => Self::Unknown,
            },
        }
    }
}

impl From<UnanchoredReason> for String {
    fn from(r: UnanchoredReason) -> Self {
        match r {
            UnanchoredReason::OffDiff => "off_diff".to_string(),
            UnanchoredReason::NoLine => "no_line".to_string(),
            UnanchoredReason::DeletedLine => "deleted_line".to_string(),
            UnanchoredReason::NitCap => "nit_cap".to_string(),
            UnanchoredReason::Withheld(class) => format!("{WITHHELD_PREFIX}{class}"),
            UnanchoredReason::Unknown => "unknown".to_string(),
        }
    }
}

/// A finding with no inline anchor, and why (#10762).
///
/// What: `finding_index` is in the combined index space (see the module
/// doc); `reason` is the [`UnanchoredReason`].
/// Test: `partition_invariant_with_fixture`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnanchoredFinding {
    /// Index into `findings`, then `withheld_findings`.
    pub finding_index: usize,
    /// Why the finding has no anchor.
    pub reason: UnanchoredReason,
}
