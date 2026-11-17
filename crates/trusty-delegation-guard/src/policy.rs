//! The per-project policy bundle and the `policy` block of `dg.event.v1`.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::error::{DgError, Result};
use crate::hash::{Sha256Hex, canonical_sha256};
use crate::project::ProjectId;
use crate::redact::cap_bytes;

/// Maximum rules in one bundle (§3).
pub const MAX_RULES: usize = 40;
/// Maximum stored rule text in bytes (§3).
pub const RULE_TEXT_MAX_BYTES: usize = 1024;
const RULE_ID_MAX: usize = 128;

/// Who stated a rule. Only `owner_verified` carries authority; an
/// `architect_note` is shown to the evaluator as non-authoritative evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authority {
    /// Ledger `source=owner` with a verified cite, a dashboard answer, or a
    /// DOC-87 typed record.
    OwnerVerified,
    /// Anything architect-sourced or read from a handoff or summary file.
    ArchitectNote,
}

/// Where a rule's span was read from (§3, Q7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Architect ledger or datg item.
    Ledger,
    /// Channel-verified dashboard answer.
    Dashboard,
    /// DOC-87 typed constraint record (once shipped).
    TypedConstraint,
    /// The project's ancestor CLAUDE.md chain.
    ClaudeMd,
}

/// Lifecycle state of a rule at bundle time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleStatus {
    /// In force.
    Active,
    /// Replaced by a later rule.
    Superseded,
    /// Withdrawn by a later owner decision.
    Reversed,
}

/// Byte range of a rule inside its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Span {
    /// Byte offset in the source.
    pub start: u64,
    /// Byte length of the full span (the stored `text` may be shorter).
    pub len: u64,
}

/// One rule, as stored in an event and submitted to the evaluator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyRule {
    /// Stable id the verdict's `rule_refs` cite.
    pub rule_id: String,
    /// Kind of source the span came from.
    pub source_kind: SourceKind,
    /// Path or record id of the source.
    pub source_ref: String,
    /// Location of the span in the source.
    pub span: Span,
    /// sha256 of the full span bytes.
    pub span_sha256: Sha256Hex,
    /// Span text verbatim, capped at [`RULE_TEXT_MAX_BYTES`].
    pub text: String,
    /// Who stated the rule.
    pub authority: Authority,
    /// Whether the rule applies at capture time.
    pub effective: bool,
    /// Lifecycle state.
    pub status: RuleStatus,
}

/// The `policy` block of `dg.event.v1`: `{policy_rev, rules}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicySnapshot {
    /// Canonical sha256 of `rules`.
    pub policy_rev: Sha256Hex,
    /// The rules, in bundle order.
    pub rules: Vec<PolicyRule>,
}

impl PolicySnapshot {
    /// Checks the §3 limits, rule-id uniqueness and the `policy_rev` digest.
    ///
    /// Test: `policy_rev_mismatch_is_rejected`, `rule_limits_are_enforced`.
    pub fn validate(&self) -> Result<()> {
        validate_rules(&self.policy_rev, &self.rules)
    }

    /// True when `rule_id` names a rule in this snapshot.
    pub fn has_rule(&self, rule_id: &str) -> bool {
        self.rules.iter().any(|r| r.rule_id == rule_id)
    }
}

fn validate_rules(policy_rev: &Sha256Hex, rules: &[PolicyRule]) -> Result<()> {
    if rules.len() > MAX_RULES {
        return Err(DgError::LimitExceeded("more than 40 rules"));
    }
    let mut seen = HashSet::new();
    for rule in rules {
        if rule.rule_id.is_empty() {
            return Err(DgError::EmptyField("rule_id"));
        }
        if rule.rule_id.len() > RULE_ID_MAX {
            return Err(DgError::LimitExceeded("rule_id over 128 bytes"));
        }
        if !seen.insert(rule.rule_id.as_str()) {
            return Err(DgError::Inconsistent("duplicate rule_id"));
        }
        if rule.text.len() > RULE_TEXT_MAX_BYTES {
            return Err(DgError::LimitExceeded("rule text over 1 KiB"));
        }
        if rule.source_ref.is_empty() {
            return Err(DgError::EmptyField("source_ref"));
        }
    }
    if &canonical_sha256(rules)? != policy_rev {
        return Err(DgError::DerivedMismatch("policy_rev"));
    }
    Ok(())
}

/// One project's rules: the unit the evaluator receives (§3 isolation rule 2).
///
/// Why: a request must carry exactly one project's bundle, and no rule may be
/// copied from one project's bundle to another.
/// What: `project_id` plus a validated `{policy_rev, rules}`; only
/// [`BundleBuilder`] or [`PolicyBundle::from_json`] produce one.
/// Test: `bundle_round_trips_through_json`, `bundle_for_a_from_b_input_is_refused`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyBundle {
    /// The project the bundle is keyed to.
    pub project_id: ProjectId,
    /// Canonical sha256 of `rules`.
    pub policy_rev: Sha256Hex,
    /// The rules.
    pub rules: Vec<PolicyRule>,
}

impl PolicyBundle {
    /// Parses and validates a bundle.
    ///
    /// Test: `bundle_round_trips_through_json`, `policy_rev_mismatch_is_rejected`.
    pub fn from_json(json: &str) -> Result<Self> {
        let bundle: Self = serde_json::from_str(json)?;
        validate_rules(&bundle.policy_rev, &bundle.rules)?;
        Ok(bundle)
    }

    /// The event's `policy` block.
    pub fn snapshot(&self) -> PolicySnapshot {
        PolicySnapshot {
            policy_rev: self.policy_rev.clone(),
            rules: self.rules.clone(),
        }
    }
}

/// Which bundles a source record may enter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleScope {
    /// The record belongs to this project only.
    Project(ProjectId),
    /// The record's scope is literally `fleet` (DOC-87 §3.2); accepted only
    /// with `owner_verified` authority.
    Fleet,
}

/// A rule as read from a policy source, before it enters a bundle.
#[derive(Debug, Clone)]
pub struct RuleSource<'a> {
    /// The project (or fleet) the source record belongs to.
    pub scope: RuleScope,
    /// Stable rule id.
    pub rule_id: &'a str,
    /// Kind of source.
    pub source_kind: SourceKind,
    /// Path or record id of the source.
    pub source_ref: &'a str,
    /// Byte offset of the span in the source.
    pub span_start: u64,
    /// The full span text; hashed whole, stored capped.
    pub span_text: &'a str,
    /// Who stated the rule.
    pub authority: Authority,
    /// Whether the rule applies now.
    pub effective: bool,
    /// Lifecycle state.
    pub status: RuleStatus,
}

/// Builds one project's [`PolicyBundle`] and refuses foreign input.
///
/// Why: §3 test 5 requires that a bundle for project A assembled from
/// project B's input is refused; doing it in the type that assembles the
/// bundle makes every caller inherit the check.
/// What: `add_rule` accepts a source only when its scope is this project, or
/// `fleet` with owner authority; `build` hashes the rules into `policy_rev`.
/// Test: `bundle_for_a_from_b_input_is_refused`,
/// `fleet_rule_needs_owner_authority`, `conflicting_projects_keep_their_own_spans`.
#[derive(Debug)]
pub struct BundleBuilder {
    project_id: ProjectId,
    rules: Vec<PolicyRule>,
}

impl BundleBuilder {
    /// Starts an empty bundle for `project_id`.
    pub fn new(project_id: ProjectId) -> Self {
        Self {
            project_id,
            rules: Vec::new(),
        }
    }

    /// Adds one rule after the scope and limit checks.
    pub fn add_rule(&mut self, source: RuleSource<'_>) -> Result<()> {
        match &source.scope {
            RuleScope::Project(owner) => self.project_id.require_same(owner)?,
            RuleScope::Fleet if source.authority != Authority::OwnerVerified => {
                return Err(DgError::Inconsistent(
                    "fleet rule without owner_verified authority",
                ));
            }
            RuleScope::Fleet => {}
        }
        if self.rules.len() >= MAX_RULES {
            return Err(DgError::LimitExceeded("more than 40 rules"));
        }
        self.rules.push(PolicyRule {
            rule_id: source.rule_id.to_owned(),
            source_kind: source.source_kind,
            source_ref: source.source_ref.to_owned(),
            span: Span {
                start: source.span_start,
                len: source.span_text.len() as u64,
            },
            span_sha256: Sha256Hex::digest(source.span_text.as_bytes()),
            text: cap_bytes(source.span_text, RULE_TEXT_MAX_BYTES).to_owned(),
            authority: source.authority,
            effective: source.effective,
            status: source.status,
        });
        Ok(())
    }

    /// Hashes the rules and returns the validated bundle.
    pub fn build(self) -> Result<PolicyBundle> {
        let policy_rev = canonical_sha256(&self.rules)?;
        validate_rules(&policy_rev, &self.rules)?;
        Ok(PolicyBundle {
            project_id: self.project_id,
            policy_rev,
            rules: self.rules,
        })
    }
}
