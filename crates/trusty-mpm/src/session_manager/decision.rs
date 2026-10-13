//! Answer idempotency for a session's pending decision (#9841).
//!
//! Why: two concurrent answers to one pending decision both reached the pane,
//! and two concurrent front-gate answers both launched the runtime. An answer
//! must take the decision exactly once, and a repeat must learn who took it.
//! What: [`DecisionId`] names one pending decision; [`DecisionAnswer`] records
//! who answered it, when, and whether the answer arrived; [`AnswerClaim`] is
//! what a caller brings; [`Claim`] is what the claim step hands back.
//! [`SessionManager::claim_decision`] takes the decision under the store's
//! write guard; the caller performs the effect after the guard is dropped and
//! reports it with [`SessionManager::settle_answer`].
//!
//! Residual (D9): the guard is in-process, so "exactly once" holds within one
//! daemon. Another process that writes back a whole record it read before the
//! claim (the supervisor, #1219) can restore a claimed decision.
//! Test: `decision_tests.rs`; the route arms in
//! `managed_routes::answer_route_tests`.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::warn;
use uuid::Uuid;

use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};
use super::store::{SessionStore, StoreError};

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
/// text.
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

/// A decision this caller took, and alone acts on (#9841).
///
/// Why: the caller performs the effect only when it holds the claim, and it
/// must know which effect: type into the pane, or launch the runtime.
/// What: the decision taken and whether the session was still in the front
/// gate. Since D1 (D73 ruling: strict) there is no claim-less answer.
/// Test: `two_concurrent_answers_reach_the_pane_once`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Claim {
    /// The decision taken; `None` for one set before #9841.
    pub decision_id: Option<DecisionId>,
    /// The session was `Provisioning`: launch the withheld runtime.
    pub front_gate: bool,
}

impl SessionRecord {
    /// The pending decision's id, only while a decision is pending (#9841).
    ///
    /// Why: a teardown clears `pending_decision` and leaves the id behind; a
    /// reader must not show an id for a decision that is gone.
    pub fn live_decision_id(&self) -> Option<DecisionId> {
        self.pending_decision.as_ref().and(self.pending_decision_id)
    }
}

/// `decision <id>`, or a name for a decision set before #9841.
fn decision_label(decision_id: Option<DecisionId>, session: &ManagedSessionId) -> String {
    match decision_id {
        Some(d) => format!("decision {d}"),
        None => format!("the decision on session {session}"),
    }
}

/// The 409 text for an answer to a decision that `last` already took.
fn already_answered(last: &DecisionAnswer, session: &ManagedSessionId) -> ManagedError {
    let how = match last.outcome {
        AnswerOutcome::Claimed => " (delivery unconfirmed)",
        AnswerOutcome::Delivered => "",
        AnswerOutcome::DeliveryFailed => " (delivery failed)",
    };
    ManagedError::DecisionConflict(format!(
        "{} already answered by {} at {}{how}",
        decision_label(last.decision_id, session),
        last.answered_by,
        last.answered_at.to_rfc3339(),
    ))
}

/// The refusal for an answer with no decision for `claim` to take.
///
/// Why: a repeat and a stale id are conflicts, and (D1, D73 ruling: strict)
/// an answer with nothing pending is refused rather than typed into the pane.
/// What: an id the last answer took, or no id after an answer, is "already
/// answered by X at T"; any other id is "not pending"; no id and no earlier
/// answer is [`ManagedError::NoPendingDecision`].
fn refusal(record: &SessionRecord, claim: &AnswerClaim) -> ManagedError {
    match (claim.decision_id, record.last_answer.as_ref()) {
        (Some(asked), Some(last)) if last.decision_id == Some(asked) => {
            already_answered(last, &record.id)
        }
        (Some(asked), _) => ManagedError::DecisionConflict(format!(
            "decision {asked} is not pending on session {}",
            record.id
        )),
        (None, Some(last)) => already_answered(last, &record.id),
        // See #9841 (D1): no record-backed decision, so nothing to answer.
        (None, None) => ManagedError::NoPendingDecision(format!(
            "no pending decision on session {}; use `tm session send` to type into its pane",
            record.id
        )),
    }
}

/// Whether `claim` takes `record`'s pending decision: one is pending and the
/// claim names it or names none.
fn takes(record: &SessionRecord, claim: &AnswerClaim) -> bool {
    record.pending_decision.is_some()
        && claim
            .decision_id
            .is_none_or(|asked| record.pending_decision_id == Some(asked))
}

/// The conflict an answer meets before its pane is proven (#9841).
///
/// Why: a repeat must get the 409 naming the first answerer even when its
/// pane can no longer be proven, as after a failed front-gate launch marks
/// the record errored; the pane refusal hid it.
/// What: the claim's own refusal, computed on a record the caller read;
/// `None` when the claim would take the decision. The claim re-checks under
/// its guard, so this never grants a claim.
/// Test: `repeat_answer_is_a_conflict_even_when_the_pane_is_gone`,
/// `front_gate_launch_failure_closes_the_decision`,
/// `answer_with_no_pending_decision_refused`.
pub(crate) fn early_conflict(record: &SessionRecord, claim: &AnswerClaim) -> Option<ManagedError> {
    (!takes(record, claim)).then(|| refusal(record, claim))
}

/// Look `id` up through a held store guard, reloading first.
///
/// Why: the claim must see another writer's latest record, and fail closed
/// when it cannot read it rather than fall back to a cached copy.
pub(super) async fn locked_get(
    store: &mut SessionStore,
    id: &ManagedSessionId,
) -> Result<SessionRecord, ManagedError> {
    store.get(id).await.map_err(|e| match e {
        StoreError::NotFound(k) => ManagedError::SessionNotFound(k),
        other => ManagedError::Store(other),
    })
}

impl SessionManager {
    /// Take the session's pending decision for `claim`, once (#9841).
    ///
    /// Why: the old answer path tested the decision with no lock, so two
    /// answers both passed the test and both typed into the pane or both
    /// launched the runtime.
    /// What: under one held store write guard (the `adopt.rs` pattern):
    /// reload, test, clear `pending_decision`, record a `Claimed`
    /// [`DecisionAnswer`], and persist. Every claim of a session is ordered by
    /// that guard, and the first one clears the decision before the guard is
    /// released, so at most one caller gets a [`Claim`]. No tmux call
    /// runs while the guard is held (#3698). A store error fails closed: the
    /// in-memory record is restored and nothing is claimed.
    /// Residual (D9): the guard is in-process; the guarantee is within one
    /// daemon.
    /// With nothing to take, the refusal is [`ManagedError::DecisionConflict`]
    /// or (D1) [`ManagedError::NoPendingDecision`].
    /// Test: `two_concurrent_answers_reach_the_pane_once`,
    /// `stale_decision_id_conflicts`,
    /// `claim_persist_failure_sends_nothing_and_keeps_the_decision`,
    /// `two_concurrent_front_gate_answers_spawn_once`,
    /// `answer_decision_without_a_pending_decision_is_refused`.
    pub async fn claim_decision(
        &self,
        id: &ManagedSessionId,
        claim: &AnswerClaim,
    ) -> Result<Claim, ManagedError> {
        let mut store = self.store.write().await;
        let mut record = locked_get(&mut store, id).await?;
        if !takes(&record, claim) {
            return Err(refusal(&record, claim));
        }
        let front_gate = record.state == ManagedSessionState::Provisioning;
        let decision_id = record.pending_decision_id.take();
        let now = Utc::now();
        record.pending_decision = None;
        record.proposed_default = None;
        record.last_activity_at = Some(now);
        record.last_answer = Some(DecisionAnswer {
            decision_id,
            answered_by: claim.answered_by.clone(),
            answered_at: now,
            outcome: AnswerOutcome::Claimed,
        });
        store.upsert_or_restore(record).await?;
        Ok(Claim {
            decision_id,
            front_gate,
        })
    }

    /// Record how a claimed answer ended (#9841).
    ///
    /// Why: a repeat's conflict says whether the first answer arrived.
    /// What: under the store guard, sets the outcome only if the record's last
    /// answer is still this claim, so a later decision is never touched. A
    /// write error is logged and swallowed: the effect already happened, and
    /// the record keeps `Claimed` ("delivery unconfirmed").
    /// Test: `settle_failure_still_reports_the_delivered_answer`,
    /// `send_failure_after_claim_closes_decision_as_failed`.
    pub(crate) async fn settle_answer(
        &self,
        id: &ManagedSessionId,
        decision_id: Option<DecisionId>,
        outcome: AnswerOutcome,
    ) {
        let mut store = self.store.write().await;
        let settled = match locked_get(&mut store, id).await {
            Ok(mut record) => match record.last_answer.as_mut() {
                Some(last)
                    if last.decision_id == decision_id
                        && last.outcome == AnswerOutcome::Claimed =>
                {
                    last.outcome = outcome;
                    store.upsert_or_restore(record).await.map_err(Into::into)
                }
                _ => Ok(()),
            },
            Err(e) => Err(e),
        };
        if let Err(e) = settled {
            warn!(id = %id, ?outcome, "answer settled but not recorded; it reads as unconfirmed: {e}");
        }
    }

    /// Type a claimed answer into the session's proven pane.
    ///
    /// Why: the effect runs after the claim's guard is dropped, and its
    /// failure must close the decision rather than reopen it (D4).
    /// What: sends `answer` to `pane`, then settles `Delivered` or
    /// `DeliveryFailed`; a failure (including a pane that could not be
    /// proven) returns [`ManagedError::DeliveryFailed`].
    /// Test: `send_failure_after_claim_closes_decision_as_failed`,
    /// `repeat_answer_names_first_answerer_and_time`.
    pub(crate) async fn deliver_answer(
        &self,
        id: &ManagedSessionId,
        tmux_name: &str,
        pane: Result<String, ManagedError>,
        answer: &str,
        claim: Claim,
    ) -> Result<Option<DecisionId>, ManagedError> {
        let sent = pane.and_then(|pane| {
            self.tmux
                .send_line_to_pane(tmux_name, &pane, answer)
                .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))
        });
        let decision_id = claim.decision_id;
        let outcome = match sent {
            Ok(()) => AnswerOutcome::Delivered,
            Err(_) => AnswerOutcome::DeliveryFailed,
        };
        self.settle_answer(id, decision_id, outcome).await;
        sent.map_err(|e| {
            ManagedError::DeliveryFailed(format!(
                "answer to {} claimed but not delivered: {e}; the decision is closed",
                decision_label(decision_id, id)
            ))
        })?;
        Ok(decision_id)
    }

    /// Settle a front-gate claim by its runtime launch (#9841, D4, D6).
    ///
    /// Why: a launch that failed may have typed part of its launch line, so
    /// the decision closes either way.
    /// What: settles `Delivered` or `DeliveryFailed`; an error becomes the
    /// route's 500 text.
    /// Test: `front_gate_launch_failure_closes_the_decision`.
    pub(crate) async fn settle_launch(
        &self,
        id: &ManagedSessionId,
        decision_id: Option<DecisionId>,
        launched: Result<(), String>,
    ) -> Result<Option<DecisionId>, String> {
        let outcome = match launched {
            Ok(()) => AnswerOutcome::Delivered,
            Err(_) => AnswerOutcome::DeliveryFailed,
        };
        self.settle_answer(id, decision_id, outcome).await;
        launched.map(|()| decision_id).map_err(|e| {
            format!(
                "answer to {} claimed but the runtime did not start: {e}; the decision is closed",
                decision_label(decision_id, id)
            )
        })
    }
}
