//! Answer idempotency for a session's pending decision (#9841).
//!
//! Why: two concurrent answers to one pending decision both reached the pane,
//! and two concurrent front-gate answers both launched the runtime. An answer
//! must take the decision exactly once, and a repeat must learn who took it.
//! What: [`DecisionId`] names one pending decision; [`DecisionAnswer`] records
//! who answered it, when, and whether the answer arrived; [`AnswerClaim`] is
//! what a caller brings; [`Claim`] is what the claim step hands back.
//! Test: `decision_tests.rs`.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The most characters of a self-asserted `answered_by` that are kept.
const ANSWERED_BY_MAX_CHARS: usize = 64;

/// Identity of one pending decision (#9841).
///
/// Why: an answer aimed at an earlier decision must never take a later one, so
/// each decision needs a name that a caller can quote back.
/// What: a random UUID, minted each time a decision is set and persisted with
/// the record, so a restart keeps it.
/// Test: `set_pending_decision_mints_a_new_id_each_time`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DecisionId(Uuid);

impl DecisionId {
    /// Mint a fresh decision id.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for DecisionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for DecisionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for DecisionId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(s.trim()).map(Self)
    }
}

/// What became of a claimed answer (#9841).
///
/// Why: a send that fails after the claim may have typed half the answer, so
/// the decision closes either way (D4) and a repeat must say how it ended.
/// What: `Claimed` until the effect returns, then `Delivered` or
/// `DeliveryFailed`. A crash between the two leaves `Claimed`, which a repeat
/// reports as "delivery unconfirmed".
/// Test: `send_failure_after_claim_closes_decision_as_failed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerOutcome {
    /// The answer took the decision; its effect has not reported back.
    Claimed,
    /// The answer reached the pane, or the withheld runtime launched.
    Delivered,
    /// The effect failed after the claim; the decision stays closed.
    DeliveryFailed,
}

/// The last answer a session's decision received (#9841).
///
/// Why: a repeat answer gets a conflict that names the first answerer and the
/// time, which needs the claim to outlive the cleared decision.
/// What: persisted on [`super::SessionRecord::last_answer`]. Holds no answer
/// text. `front_gate` marks a claim that launched the withheld runtime rather
/// than typing into a live pane.
/// Test: `repeat_answer_names_first_answerer_and_time`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionAnswer {
    /// The decision answered; `None` for a decision set before #9841.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_id: Option<DecisionId>,
    /// Who answered, self-asserted (D5).
    pub answered_by: String,
    /// When the claim was taken.
    pub answered_at: DateTime<Utc>,
    /// How the answer ended.
    pub outcome: AnswerOutcome,
    /// Whether the claim launched the front gate's withheld runtime.
    #[serde(default)]
    pub front_gate: bool,
}

/// What a caller brings to an answer (#9841).
///
/// Why: the claim needs the decision the caller saw (optional, D2) and a name
/// for the conflict text (optional and self-asserted, D5).
/// What: an optional [`DecisionId`] and a cleaned `answered_by` label.
/// Test: `answered_by_is_clipped_and_defaults_to_the_transport`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswerClaim {
    /// The decision the caller answers; `None` takes whichever is pending.
    pub decision_id: Option<DecisionId>,
    /// Who answers, as the conflict text will name them.
    pub answered_by: String,
}

impl AnswerClaim {
    /// A claim on whichever decision is pending, made by `answered_by`.
    pub fn by(answered_by: &str) -> Self {
        Self {
            decision_id: None,
            answered_by: clean_answered_by(Some(answered_by), "unknown"),
        }
    }

    /// Build a claim from a request's optional wire fields.
    ///
    /// Why: `/answer` and `mpm.managed.answer` carry the same two optional
    /// fields, and both must clean them the same way.
    /// What: parses `decision_id` as a UUID (an error names the bad value);
    /// cleans `answered_by`, falling back to `transport` when it is absent or
    /// empty.
    /// Test: `answered_by_is_clipped_and_defaults_to_the_transport`.
    pub fn from_wire(
        decision_id: Option<&str>,
        answered_by: Option<&str>,
        transport: &str,
    ) -> Result<Self, String> {
        let decision_id = decision_id
            .map(|raw| {
                raw.parse::<DecisionId>()
                    .map_err(|e| format!("invalid decision_id {raw:?}: {e}"))
            })
            .transpose()?;
        Ok(Self {
            decision_id,
            answered_by: clean_answered_by(answered_by, transport),
        })
    }
}

/// Strip control characters and clip to [`ANSWERED_BY_MAX_CHARS`]; an empty
/// result falls back to `fallback`.
fn clean_answered_by(raw: Option<&str>, fallback: &str) -> String {
    let cleaned: String = raw
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(ANSWERED_BY_MAX_CHARS)
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        fallback.to_owned()
    } else {
        cleaned.to_owned()
    }
}

/// What the claim step hands back (#9841).
///
/// Why: the caller performs the effect only when it holds the claim, and it
/// must know which effect: type into the pane, or launch the runtime.
/// What: `Claimed` names the decision taken and whether the session was still
/// in the front gate; `Unclaimed` means nothing was pending and the answer
/// keeps today's live-pane behaviour (D1 is not ruled).
/// Test: `two_concurrent_answers_reach_the_pane_once`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claim {
    /// This caller took the decision and alone performs its effect.
    Claimed {
        /// The decision taken; `None` for one set before #9841.
        decision_id: Option<DecisionId>,
        /// The session was `Provisioning`: launch the withheld runtime.
        front_gate: bool,
    },
    /// No decision was pending: type the answer into the live pane as before.
    Unclaimed,
}
