//! The correction lifecycle: `deprecate` and `supersede` (SPEC-OKF-01 §2.5,
//! 3c A; §4.8), and the tombstone write rule (§4.8).
//!
//! Why: a published concept is never deleted. A curator retires it in place
//! with `status: deprecated`, and a successor links both ways, so every
//! concept ID, ledger path and search-index path stays valid (3c A). A
//! tombstone must stay detectable by trusty-search, which needs a top-level
//! `source_id`.
//! What: [`Bundle::deprecate`] sets `status: deprecated`: a lifecycle-only
//! write (§2.10 keeps `verified[]` and hashless events), journaled with one
//! `**Deprecated**` `log.md` entry. [`Bundle::supersede`] creates the new
//! concept with `supersedes: /<old>.md` first, then marks the old one
//! `status: deprecated` with `superseded_by: /<new>.md`, then logs one
//! `**Superseded**` entry, as one journaled `Supersede` operation (§4.11
//! "Step order": the add comes before the mark). [`check_tombstone`]
//! refuses a write of `source_status: deleted` without a non-empty
//! top-level `source_id`. Slice S6 of #10746.
//! Test: `c40_supersede_keeps_the_old_file_and_links_both_ways`,
//! `deprecate_is_lifecycle_only_and_logged`,
//! `c38_tombstone_without_source_id_is_refused`,
//! `c143_hashless_events_move_on_content_change_and_stay_otherwise`,
//! `c106_supersede_and_migrate_crash_at_every_boundary`.

use chrono::{DateTime, FixedOffset};
use serde_yaml::Value;

use crate::OkfError;
use crate::bundle::Bundle;
use crate::bundle::commit::{LogMode, PutPlan};
use crate::bundle::log_md::prepare_entry;
use crate::bundle::ops::{self, OpKind, OpPlan};
use crate::bundle::put::{PutOutcome, WriteKind, conflict};
use crate::bundle::record::{instant_string, record_rel};
use crate::format::concept::Concept;
use crate::format::fields::Actor;
use crate::format::id::{ConceptId, ConceptRef};
use crate::format::validate::Finding;
use crate::format::write::{WriteMode, foreign_refusal};
use crate::state::revisions::REVISIONS_REL;

/// Refuse `source_status: deleted` without a non-empty top-level
/// `source_id` (§4.8): trusty-search's `is_tombstone` would not see it.
///
/// Test: `c38_tombstone_without_source_id_is_refused`.
pub(crate) fn check_tombstone(id: &ConceptId, c: &Concept) -> Result<(), OkfError> {
    let deleted = c.get("source_status").and_then(Value::as_str) == Some("deleted");
    let has_source = c
        .get("source_id")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    if deleted && !has_source {
        return Err(conflict(id, "tombstone_without_source_id"));
    }
    Ok(())
}

fn link(id: &ConceptId) -> Value {
    Value::String(format!("/{}", id.file_name()))
}

impl Bundle {
    /// Retire a concept in place (SPEC-OKF-01 §2.5 rule 1, §4.8).
    ///
    /// Why/What: see the module doc. A concept that is already deprecated
    /// returns `Unchanged` and writes nothing. `NotFound` for a missing
    /// concept; every `put` refusal applies.
    /// Test: `deprecate_is_lifecycle_only_and_logged`,
    /// `c143_hashless_events_move_on_content_change_and_stay_otherwise`.
    pub fn deprecate(
        &mut self,
        id: &ConceptId,
        reason: &str,
        by: Actor,
        now: DateTime<FixedOffset>,
    ) -> Result<(PutOutcome, Vec<Finding>), OkfError> {
        // #10746 S6: refusals first without the lock, as `put` (§4.4).
        let _lock = if self.held.is_none() {
            if self.plan_deprecate(id, reason, &by, &now)?.outcome == PutOutcome::Unchanged {
                return Ok((PutOutcome::Unchanged, Vec::new()));
            }
            self.write_lock()?
        } else {
            None
        };
        let plan = self.plan_deprecate(id, reason, &by, &now)?;
        let mut out = (plan.outcome, plan.findings.clone());
        if plan.outcome != PutOutcome::Unchanged {
            out.1
                .extend(self.commit_op(plan, &by, &now, LogMode::Entry, OpKind::Deprecate)?);
        }
        Ok(out)
    }

    fn plan_deprecate(
        &self,
        id: &ConceptId,
        reason: &str,
        by: &Actor,
        now: &DateTime<FixedOffset>,
    ) -> Result<PutPlan, OkfError> {
        let mut c = self.get(id)?;
        c.set("status", Value::String("deprecated".into()));
        let mut plan = self.plan_put(
            c,
            by,
            false,
            WriteMode::Lenient,
            now,
            WriteKind::Caller,
            None,
        )?;
        plan.log = format!("**Deprecated** `/{}` by `{by}`: {reason}", id.file_name());
        Ok(plan)
    }

    /// Replace a concept by a new one, keeping both (SPEC-OKF-01 §2.5 rule
    /// 2, 3c A).
    ///
    /// Why/What: see the module doc. Refusals, with no file changed: a
    /// foreign or equal new ID (`InvalidId`, `supersede_self`), a missing old
    /// concept (`NotFound`), an old concept that already has
    /// `superseded_by` (`superseded`), a new ID that exists (`exists`), and
    /// every `put` refusal; the new concept is written `StrictErrors` (3f B).
    /// Returns the findings of both writes.
    /// Test: `c40_supersede_keeps_the_old_file_and_links_both_ways`,
    /// `c106_supersede_and_migrate_crash_at_every_boundary`.
    pub fn supersede(
        &mut self,
        old: &ConceptId,
        new: Concept,
        reason: &str,
        by: Actor,
        now: DateTime<FixedOffset>,
    ) -> Result<Vec<Finding>, OkfError> {
        let _lock = if self.held.is_none() {
            self.plan_supersede(old, new.clone(), reason, &by, &now)?;
            self.write_lock()?
        } else {
            None
        };
        let (op, mut findings) = self.plan_supersede(old, new, reason, &by, &now)?;
        self.check_recovery(&op.file_paths())?;
        findings.extend(ops::run(self, op)?);
        Ok(findings)
    }

    fn plan_supersede(
        &self,
        old: &ConceptId,
        mut new: Concept,
        reason: &str,
        by: &Actor,
        now: &DateTime<FixedOffset>,
    ) -> Result<(OpPlan, Vec<Finding>), OkfError> {
        let new_id = match &new.id {
            ConceptRef::Id(id) => id.clone(),
            ConceptRef::Foreign(p) => return Err(foreign_refusal(p)),
        };
        if &new_id == old {
            return Err(conflict(old, "supersede_self"));
        }
        let mut prior = self.get(old)?;
        if prior.get("superseded_by").is_some() {
            return Err(conflict(old, "superseded"));
        }
        new.set("supersedes", link(old));
        prior.set("status", Value::String("deprecated".into()));
        prior.set("superseded_by", link(&new_id));
        let kind = WriteKind::Caller;
        let added = self.plan_put(new, by, false, WriteMode::StrictErrors, now, kind, None)?;
        let marked = self.plan_put(prior, by, false, WriteMode::Lenient, now, kind, None)?;
        let mut findings = added.findings.clone();
        findings.extend(marked.findings.iter().cloned());
        let stamp = instant_string(now);
        let mut op = OpPlan::new(OpKind::Supersede, &stamp, &by.to_string());
        // §4.11 "Step order": the new concept before the old one is marked.
        for p in [&added, &marked] {
            for l in &p.lines {
                op.append(&record_rel(&p.id), l.value()?);
            }
            let mut rev = p.revision("supersede", now, "");
            rev.stamp(self.root(), p.pre.as_deref(), false)?;
            let value = serde_json::to_value(&rev).map_err(|e| OkfError::Parse {
                path: REVISIONS_REL.into(),
                line: None,
                message: e.to_string(),
            })?;
            op.append(REVISIONS_REL, value);
            op.file(&p.id.file_name(), p.pre.clone(), Some(p.post.clone()));
        }
        let entry = prepare_entry(
            self.root(),
            now,
            &format!(
                "**Superseded** `/{}` by `/{}` (by `{by}`): {reason}",
                old.file_name(),
                new_id.file_name()
            ),
        )?;
        op.file("log.md", entry.old, Some(entry.text.into_bytes()));
        Ok((op, findings))
    }
}
