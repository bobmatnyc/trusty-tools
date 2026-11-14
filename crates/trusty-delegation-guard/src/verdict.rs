//! `dg.verdict.v1`: the evaluator's answer to one event (design §4.5, A1).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{DgError, Result};
use crate::event::DgEvent;
use crate::hash::Sha256Hex;

/// Schema version of [`DgVerdict`]. The verdict JSON carries no `schema`
/// field: §4.5 fixes its exact key set, and the model emits it.
pub const VERDICT_SCHEMA: &str = "dg.verdict.v1";

/// Maximum `mismatch` length in chars (§4.5).
pub const MISMATCH_MAX_CHARS: usize = 300;

/// Who paid for the call. A1: always our account; there is no other variant,
/// so a recipient-funded receipt cannot parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FundedBy {
    /// Our inference account.
    Ours,
}

/// Inference receipt recorded on every verdict (§4.3, A1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelReceipt {
    /// Crate-local alias, for example `pinned`.
    pub alias: String,
    /// Provider that served the call.
    pub provider: String,
    /// Model id from the provider response.
    pub id: String,
    /// Account id from the provider response.
    pub account: String,
    /// Always `ours`.
    pub funded_by: FundedBy,
}

/// Whether the evaluator produced a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// A validated verdict follows.
    Ok,
    /// No verdict; `unavailable.reason` says why. Never maps to allow.
    Unavailable,
}

/// Why no verdict exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    /// Attempt timed out.
    Timeout,
    /// Transport or 5xx failure.
    Transport,
    /// Auth failure or wrong-account response.
    Auth,
    /// Provider returned 429.
    RateLimited,
    /// Output failed post-validation.
    Malformed,
    /// Budget breaker open.
    Budget,
    /// Startup probe found another model.
    ModelMismatch,
    /// Host disabled at the evaluator.
    Disabled,
    /// Policy changed under the event.
    StalePolicy,
}

/// The `unavailable` object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unavailable {
    /// The reason.
    pub reason: UnavailableReason,
}

/// The semantic answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The dispatch fits the cited rules.
    Allow,
    /// Would have been flagged: a cited rule is violated.
    Revise,
    /// The evidence cannot decide it.
    InsufficientEvidence,
}

/// The span a rule reference cites.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefSpan {
    /// Source path or record id.
    pub source_ref: String,
    /// Byte offset.
    pub start: u64,
    /// Byte length.
    pub len: u64,
}

/// How a cited rule bears on the verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefRole {
    /// The task breaks the rule.
    Violated,
    /// The task meets the rule.
    Satisfied,
    /// The rule reached the child by inheritance.
    Inherited,
}

/// One citation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleRef {
    /// A rule id from the submitted bundle.
    pub rule_id: String,
    /// The cited span.
    pub span: RefSpan,
    /// Role of the rule.
    pub role: RefRole,
}

/// Failure classes from the regression families (§4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum FailureCategory {
    NestedCapOmitted,
    ScopeLost,
    CensusMissing,
    BackgroundUnfinished,
    ExceptionIneligible,
    OwnerReversed,
    InheritanceUnverified,
    ModelExceptionAltered,
    Other,
}

/// Evidence the evaluator asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum EvidenceNeed {
    PolicySpan,
    WorkerState,
    Tree,
    OwnerCite,
    PromptFull,
}

/// Token counts from the provider response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tokens {
    /// Input tokens (`in` on the wire).
    #[serde(rename = "in")]
    pub input: u64,
    /// Output tokens.
    pub out: u64,
}

/// One `dg.verdict.v1` record.
///
/// Why: §4.5 post-validation must reject a model answer that adds keys,
/// contradicts itself, or cites rules it was not given, before anything is
/// stored as `ok`. No field holds replacement policy, so nothing executable
/// can be auto-applied.
/// What: the exact §4.5 key set plus the A1 `funded_by` receipt; strict
/// parse, then [`DgVerdict::validate`].
/// Test: `verdict_round_trips_through_json`, `allow_with_failure_category_is_rejected`,
/// `revise_without_rule_ref_is_rejected`, `unknown_verdict_key_is_rejected`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DgVerdict {
    /// The event judged.
    pub event_id: Uuid,
    /// The event's dedupe key.
    pub dedupe_key: Sha256Hex,
    /// sha256 of the prompt file used.
    pub prompt_sha256: Sha256Hex,
    /// Inference receipt.
    pub model: ModelReceipt,
    /// `ok` or `unavailable`.
    pub status: Status,
    /// Present exactly when `status` is `unavailable`.
    pub unavailable: Option<Unavailable>,
    /// Present exactly when `status` is `ok`.
    pub verdict: Option<Verdict>,
    /// Citations.
    pub rule_refs: Vec<RuleRef>,
    /// Failure class; never set with `allow`.
    pub failure_category: Option<FailureCategory>,
    /// At most 300 chars, citing rule ids.
    pub mismatch: String,
    /// Evidence requested.
    pub evidence_needed: Vec<EvidenceNeed>,
    /// Evaluator latency.
    pub latency_ms: u64,
    /// Token counts.
    pub tokens: Tokens,
}

impl DgVerdict {
    /// Parses a verdict strictly and runs [`DgVerdict::validate`].
    ///
    /// Test: `unknown_verdict_key_is_rejected`, `verdict_round_trips_through_json`.
    pub fn from_json(json: &str) -> Result<Self> {
        let verdict: Self = serde_json::from_str(json)?;
        verdict.validate()?;
        Ok(verdict)
    }

    /// Serializes the verdict as JSON.
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    /// The record-internal §4.5 rules (rule 4 and status consistency).
    ///
    /// Test: `allow_with_failure_category_is_rejected`,
    /// `revise_without_rule_ref_is_rejected`, `status_and_verdict_must_agree`.
    pub fn validate(&self) -> Result<()> {
        if self.mismatch.chars().count() > MISMATCH_MAX_CHARS {
            return Err(DgError::MalformedVerdict("mismatch over 300 chars"));
        }
        match (self.status, &self.unavailable, self.verdict) {
            (Status::Ok, None, Some(_)) => {}
            (Status::Unavailable, Some(_), None) => {
                if !self.rule_refs.is_empty() || self.failure_category.is_some() {
                    return Err(DgError::MalformedVerdict(
                        "unavailable verdict carries findings",
                    ));
                }
                return Ok(());
            }
            _ => {
                return Err(DgError::MalformedVerdict(
                    "status, unavailable and verdict disagree",
                ));
            }
        }
        let m = &self.model;
        if [&m.alias, &m.provider, &m.id, &m.account]
            .iter()
            .any(|s| s.is_empty())
        {
            return Err(DgError::MalformedVerdict("ok verdict without full receipt"));
        }
        match self.verdict {
            Some(Verdict::Allow) if self.failure_category.is_some() => {
                Err(DgError::MalformedVerdict("allow with failure_category"))
            }
            Some(Verdict::Revise) if self.rule_refs.is_empty() => {
                Err(DgError::MalformedVerdict("revise without rule_ref"))
            }
            _ => Ok(()),
        }
    }

    /// Checks the verdict against the event it answers: ids match and every
    /// citation names a rule and span in that event's own bundle (§4.5 rule 2).
    ///
    /// Test: `verdict_citing_foreign_rule_is_rejected`.
    pub fn validate_against(&self, event: &DgEvent) -> Result<()> {
        if self.event_id != event.event_id || self.dedupe_key != event.dedupe_key {
            return Err(DgError::MalformedVerdict("verdict answers another event"));
        }
        for r in &self.rule_refs {
            let cited = event.policy.rules.iter().any(|rule| {
                rule.rule_id == r.rule_id
                    && rule.source_ref == r.span.source_ref
                    && rule.span.start == r.span.start
                    && rule.span.len == r.span.len
            });
            if !cited {
                return Err(DgError::MalformedVerdict("citation not in bundle"));
            }
        }
        Ok(())
    }
}
