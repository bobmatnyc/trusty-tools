//! `Bundle::put` and `Bundle::verify` (SPEC-OKF-01 §2.2.1, §2.4, §2.10,
//! §2.11, §4.3, §4.8).
//!
//! Why: a write must keep the bytes it does not own, never replace bytes it
//! did not read (#10746 A1), check what it renders (A2), keep the target's
//! mode (A3), clear verification on every content change, and never let an
//! automated writer extend a person's review.
//! What: [`Bundle::put`] refuses, with no file changed: a foreign target
//! (`InvalidId`); a fold-key twin of another file (`fold_collision`); a new
//! concept over an existing file (`exists`) or a stale read (`stale_write`);
//! a caller change to `verified`, which only `verify` writes
//! (`verified_is_owned_by_verify`); a `supersedes`/`superseded_by` value
//! that is not a bundle-absolute concept link (`InvalidId`); a content change
//! to a deprecated concept (`deprecated`); a later `stale_after` on a
//! human-reviewed concept unless the actor is `human:` and the call carries
//! a [`HumanConfirmation`] (`stale_after_extension`). On a content change it applies §2.10: a
//! malformed `verified`/`generated` moves to its `legacy_` key with its raw
//! bytes, every frontmatter event moves to the record, `verified` leaves the
//! frontmatter, and `generated` becomes `{by, at: now}`. Then the shared
//! strict write runs, the record is appended and `log.md` gains one entry.
//! [`Bundle::verify`] binds a new event to the hash the verifier saw, and
//! writes a `human:` event only with a [`HumanConfirmation`] (§2.4).
//! #10746: an `Actor::Human` value alone is never a confirmation; only the
//! `_confirmed` entry points accept one.
//! Order (#10746, §4.4, §4.11): every refusal, the render, and the
//! confinement of the record and `log.md` paths run first, with no file
//! changed. #10746 S5: then one journaled operation (`bundle::commit`)
//! appends the record lines and the revision line, renames the concept into
//! place and writes `log.md`; a crash at any step is finished by the next
//! open, and an in-process step failure rolls the set back (§4.11).
//! Test: `a1_put_never_replaces_an_existing_file_with_a_new_concept`,
//! `a1_put_refuses_a_stale_write`, `a2_strict_check_runs_on_rendered_values`,
//! `a3_put_writes_the_concept_0600_whatever_its_prior_mode`,
//! `c31_body_change_moves_verification_to_the_record`,
//! `c32_description_change_clears_verification`,
//! `c33_status_or_stale_after_change_keeps_verification`,
//! `c35_verify_with_stale_hash_is_conflict_and_writes_nothing`,
//! `c125_malformed_owned_values_move_to_legacy_keys`,
//! `c146_stale_after_extension_needs_a_human_or_a_verify`,
//! `unwritable_record_keeps_the_legacy_event_and_the_concept`,
//! `put_and_verify_refuse_a_symlinked_log_with_no_file_changed`,
//! `human_actor_without_confirmation_cannot_extend_a_review`,
//! `human_verify_without_confirmation_is_refused`.

use chrono::{DateTime, FixedOffset};
use serde_yaml::{Mapping, Value};

use crate::OkfError;
use crate::bundle::Bundle;
use crate::bundle::commit::{LogMode, PutPlan};
use crate::bundle::ops::OpKind;
use crate::bundle::paths::{confined_file, fold_conflict};
use crate::bundle::record::{self, Line, instant_string};
use crate::format::concept::{Concept, parse_concept};
use crate::format::fields::{Actor, Field, Status, Verification};
use crate::format::hash::{ContentHash, content_hash};
use crate::format::id::{ConceptId, ConceptRef};
use crate::format::validate::{Finding, FindingKind, Severity};
use crate::format::write::{
    WriteMode, WriteOutcome, check_not_stale, foreign_refusal, prepare_write_with, read_existing,
};

/// An explicit confirmation from the person a `human:` actor names
/// (SPEC-OKF-01 §2.4).
///
/// Why: §2.4 accepts a `human:` verification, and a later `stale_after` on a
/// human-reviewed concept, only with explicit human confirmation. Any caller
/// can build an `Actor::Human`, so the actor alone never counts (#10746).
/// What: a token with a private field, built only by
/// [`HumanConfirmation::explicit`] and consumed by value by
/// [`Bundle::put_confirmed`] and [`Bundle::verify_confirmed`]. Build it only
/// where a person has just confirmed this one call, never from a default, a
/// setting or model output. It is neither `Default` nor `Clone`, so one
/// confirmation covers one call.
/// Test: `c146_stale_after_extension_needs_a_human_or_a_verify`,
/// `human_actor_without_confirmation_cannot_extend_a_review`,
/// `human_verify_without_confirmation_is_refused`.
#[derive(Debug)]
pub struct HumanConfirmation {
    _private: (),
}

impl HumanConfirmation {
    /// The person named by the call's `human:` actor confirmed this call.
    pub fn explicit() -> Self {
        Self { _private: () }
    }
}

/// Who a crate-private `put_inner` call writes for (#10746).
///
/// What: `Caller` is a public `put`; `LinkRewrite` is a `move_concept` link
/// rewrite (no `log.md` entry; takes a deprecated concept); `SourceReingest`
/// is the build engine writing a source's new content, which a deprecated
/// concept also takes (SPEC-OKF-01 §3 (c2) (i)) while §2.10 and the
/// `log.md` entry still apply.
/// Test: `reingest_over_a_deprecated_concept_writes_content_and_keeps_status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriteKind {
    /// A public `put` or `put_confirmed`.
    Caller,
    /// A `move_concept` link rewrite.
    LinkRewrite,
    /// The build engine writing a source item.
    SourceReingest,
}

/// What a `put` or `verify` did (SPEC-OKF-01 §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PutOutcome {
    /// The concept did not exist and was created.
    Created,
    /// The file's bytes changed.
    Updated {
        /// Whether the §2.10 content hash changed.
        content_changed: bool,
        /// Frontmatter verification events moved to history.
        verifications_cleared: usize,
    },
    /// Nothing changed; no byte was written.
    Unchanged,
}

pub(crate) fn conflict(id: &ConceptId, reason: &str) -> OkfError {
    OkfError::Conflict {
        id: id.clone(),
        reason: reason.to_string(),
    }
}

/// The concept a bundle-absolute link `/<id>.md` names (§2.11 link rule 4).
///
/// Test: `c56_supersession_link_must_parse_as_a_concept_id`.
pub fn link_target(s: &str) -> Result<ConceptId, OkfError> {
    match s.strip_prefix('/').and_then(|r| r.strip_suffix(".md")) {
        Some(inner) => ConceptId::parse(inner),
        None => Err(OkfError::InvalidId {
            input: s.to_string(),
            rule: 0,
        }),
    }
}

/// Whether the stored value of `key` is malformed, so an operation that owns
/// the key must move it to its `legacy_` key (§2.2.1, r5).
fn malformed_owned(c: &Concept, key: &str) -> bool {
    let fm = c.frontmatter();
    match key {
        "verified" => match &fm.verified {
            Field::Malformed(_) => true,
            Field::Valid(list) => list.iter().any(|e| matches!(e, Field::Malformed(_))),
            Field::Absent => false,
        },
        "generated" => matches!(fm.generated, Field::Malformed(_)),
        _ => false,
    }
}

/// Move a malformed owned value to its legacy key, or refuse when that key
/// is taken (§2.2.1 "Malformed owned values").
fn move_to_legacy(
    id: &ConceptId,
    next: &mut Concept,
    key: &str,
    findings: &mut Vec<Finding>,
) -> Result<(), OkfError> {
    let legacy = format!("legacy_{key}");
    if next.get(&legacy).is_some() {
        return Err(conflict(id, &format!("{legacy}_exists")));
    }
    next.rename_key(key, &legacy);
    let mut f = Finding::new(
        Severity::Warning,
        FindingKind::LegacyValueMoved,
        id.file_name(),
        format!("malformed `{key}` moved to `{legacy}` with its bytes unchanged"),
        "SPEC-OKF-01 §2.2.1",
    );
    f.concept = Some(id.clone());
    findings.push(f);
    Ok(())
}

/// Whether `new` moves `stale_after` later than `old` (§2.4). Removing it is
/// later (never stale); a malformed value on either side cannot be proven
/// earlier, so it counts as later.
fn extends_stale_after(old: &Concept, new: &Concept) -> bool {
    if old.get("stale_after") == new.get("stale_after") {
        return false;
    }
    match (
        &old.frontmatter().stale_after,
        &new.frontmatter().stale_after,
    ) {
        (Field::Valid(a), Field::Valid(b)) => b > a,
        (Field::Absent, Field::Valid(_)) => false,
        _ => true,
    }
}

/// Link rule 4: a changed `supersedes`/`superseded_by` must be a
/// bundle-absolute concept link.
fn check_supersession(c: &Concept, old: Option<&Concept>) -> Result<(), OkfError> {
    for key in ["supersedes", "superseded_by"] {
        let v = c.get(key);
        if v.is_none() || v == old.and_then(|o| o.get(key)) {
            continue;
        }
        match v.and_then(Value::as_str) {
            Some(s) => {
                link_target(s)?;
            }
            None => {
                return Err(OkfError::InvalidId {
                    input: format!("{key}: {v:?}"),
                    rule: 0,
                });
            }
        }
    }
    Ok(())
}

fn generated_value(by: &Actor, now: &DateTime<FixedOffset>) -> Value {
    let mut m = Mapping::new();
    m.insert("by".into(), Value::String(by.to_string()));
    m.insert("at".into(), Value::String(instant_string(now)));
    Value::Mapping(m)
}

/// The concept on disk at `bytes`, parsed under `id`.
fn parse_disk(id: &ConceptId, bytes: &[u8]) -> Result<Concept, OkfError> {
    let text = std::str::from_utf8(bytes).map_err(|e| OkfError::Parse {
        path: id.file_name().into(),
        line: None,
        message: format!("not UTF-8: {e}"),
    })?;
    Ok(parse_concept(id.clone(), text)?.0)
}

impl Bundle {
    /// Create or update one concept (SPEC-OKF-01 §4.3).
    ///
    /// Why/What: see the module doc. #10746: returns the write's findings
    /// beside the outcome (the §4.3 sketch returns the outcome only), because
    /// criterion 125 needs the `legacy_value_moved` warnings. #10746: this
    /// call carries no human confirmation, so a `human:` actor here counts
    /// as any other writer under §2.4; see [`Bundle::put_confirmed`].
    /// Test: see the module doc.
    pub fn put(
        &mut self,
        c: Concept,
        by: Actor,
        mode: WriteMode,
        now: DateTime<FixedOffset>,
    ) -> Result<(PutOutcome, Vec<Finding>), OkfError> {
        let (outcome, findings, _) =
            self.put_inner(c, &by, false, mode, &now, WriteKind::Caller, LogMode::Entry)?;
        Ok((outcome, findings))
    }

    /// `put` with the explicit confirmation of the person `by` names (§2.4).
    ///
    /// Why/What: as [`Bundle::put`], except that a `human:` `by` may move
    /// `stale_after` later on a human-reviewed concept. With any other actor
    /// the confirmation changes nothing.
    /// Test: `c146_stale_after_extension_needs_a_human_or_a_verify`.
    pub fn put_confirmed(
        &mut self,
        c: Concept,
        by: Actor,
        confirmation: HumanConfirmation,
        mode: WriteMode,
        now: DateTime<FixedOffset>,
    ) -> Result<(PutOutcome, Vec<Finding>), OkfError> {
        let HumanConfirmation { _private: () } = confirmation;
        let (outcome, findings, _) =
            self.put_inner(c, &by, true, mode, &now, WriteKind::Caller, LogMode::Entry)?;
        Ok((outcome, findings))
    }

    /// `put`; `kind` says who writes (see [`WriteKind`]) and `log` where the
    /// `log.md` entry goes (see [`LogMode`]). Only a `Caller` content change
    /// to a deprecated concept is refused (§4.8 refuses caller edits only).
    /// `confirmed` is true only under a [`HumanConfirmation`].
    /// Returns the outcome, the findings and the target ID.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn put_inner(
        &mut self,
        c: Concept,
        by: &Actor,
        confirmed: bool,
        mode: WriteMode,
        now: &DateTime<FixedOffset>,
        kind: WriteKind,
        log: LogMode<'_>,
    ) -> Result<(PutOutcome, Vec<Finding>, ConceptId), OkfError> {
        // #10746 S5: one writer at a time (§4.4). The refusals run first
        // without the lock, so a refused or unchanged write creates no lock
        // file; the plan is then made again under the lock.
        let _lock = if self.held.is_none() {
            let probe = self.plan_put(c.clone(), by, confirmed, mode, now, kind, None)?;
            if probe.outcome == PutOutcome::Unchanged {
                return Ok((probe.outcome, probe.findings, probe.id));
            }
            self.write_lock()?
        } else {
            None
        };
        let plan = self.plan_put(c, by, confirmed, mode, now, kind, None)?;
        let mut out = (plan.outcome, plan.findings.clone(), plan.id.clone());
        if plan.outcome != PutOutcome::Unchanged {
            out.1
                .extend(self.commit_op(plan, by, now, log, OpKind::Correct)?);
        }
        Ok(out)
    }

    /// Every check and the render of a `put`, writing nothing.
    ///
    /// #10746 S5: split from `put_inner` so a journaled operation can stage
    /// the post-image. `at` overrides the target's bytes and names the
    /// concept whose verification record holds its events: a move plans the
    /// moved concept's rewrite at its new path before any file moves.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn plan_put(
        &self,
        c: Concept,
        by: &Actor,
        confirmed: bool,
        mode: WriteMode,
        now: &DateTime<FixedOffset>,
        kind: WriteKind,
        at: Option<(Option<Vec<u8>>, ConceptId)>,
    ) -> Result<PutPlan, OkfError> {
        let id = match &c.id {
            ConceptRef::Id(id) => id.clone(),
            ConceptRef::Foreign(p) => return Err(foreign_refusal(p)),
        };
        let target = confined_file(&self.root, &id.file_name())?;
        let (existing, record_of) = match at {
            Some((bytes, of)) => (bytes, of),
            None => {
                if fold_conflict(&self.root, &id.file_name())?.is_some() {
                    return Err(conflict(&id, "fold_collision"));
                }
                (read_existing(&target)?, id.clone())
            }
        };
        check_not_stale(&id, &c, existing.as_deref())?;
        let old = existing
            .as_deref()
            .map(|b| parse_disk(&id, b))
            .transpose()?;
        check_supersession(&c, old.as_ref())?;
        if c.get("verified") != old.as_ref().and_then(|o| o.get("verified")) {
            return Err(conflict(&id, "verified_is_owned_by_verify"));
        }
        let mut next = c;
        let mut findings = Vec::new();
        let content_changed = old
            .as_ref()
            .is_none_or(|o| content_hash(o) != content_hash(&next));
        let mut lines: Vec<Line> = Vec::new();
        let mut cleared = 0;
        if let Some(o) = &old {
            let deprecated = matches!(o.frontmatter().status, Field::Valid(Status::Deprecated));
            // #10746: §3 (c2) (i) — a source re-ingest writes the new content
            // and keeps `status: deprecated`; only a caller edit is refused.
            if content_changed && deprecated && kind == WriteKind::Caller {
                return Err(conflict(&id, "deprecated"));
            }
            // #10746: §2.4 needs a `human:` actor AND explicit confirmation.
            if extends_stale_after(o, &next)
                && !(confirmed && is_human(by))
                && matches!(
                    record::state_for(self, &record_of, o)?,
                    record::VerificationState::Verified { human: true, .. }
                )
            {
                return Err(conflict(&id, "stale_after_extension"));
            }
            if content_changed {
                let mut events = Vec::new();
                if malformed_owned(o, "verified") {
                    move_to_legacy(&id, &mut next, "verified", &mut findings)?;
                } else {
                    events = record::frontmatter_events(o.get("verified"));
                    next.remove("verified");
                }
                if malformed_owned(o, "generated") {
                    move_to_legacy(&id, &mut next, "generated", &mut findings)?;
                }
                let folded = record::fold(record::read_lines(self, &record_of)?);
                let new_hash = content_hash(&next);
                lines =
                    record::invalidation_lines(&folded, &events, &instant_string(now), &new_hash);
                cleared = events.len();
            }
        }
        if content_changed {
            next.set("generated", generated_value(by, now));
        }
        let (prepared, write_findings) =
            prepare_write_with(&self.root, &next, mode, existing.as_deref())?;
        findings.extend(write_findings);
        let outcome = match prepared.outcome {
            WriteOutcome::Unchanged => PutOutcome::Unchanged,
            WriteOutcome::Created => PutOutcome::Created,
            WriteOutcome::Updated => PutOutcome::Updated {
                content_changed,
                verifications_cleared: cleared,
            },
        };
        Ok(PutPlan {
            log: log_line(&id, by, &outcome),
            id,
            outcome,
            findings,
            pre: existing,
            post: prepared.bytes().to_vec(),
            content: content_hash(&next),
            lines,
        })
    }

    /// Record a verification of the revision the verifier saw (§2.10).
    ///
    /// Why: a person must not verify a revision that changed after they read
    /// it, and only a record event bound to the current hash counts.
    /// What: a `human:` `v.by` is refused with
    /// `Conflict { reason: "human_not_confirmed" }` and nothing is written;
    /// it needs [`Bundle::verify_confirmed`] (§2.4, #10746). `seen` must
    /// equal the current content hash, else
    /// `Conflict { reason: "stale_verify_hash" }` and nothing is written. A
    /// malformed `verified` first moves to `legacy_verified` (criterion 125;
    /// `Conflict` when that key exists). The event `{by, at}` is appended to
    /// `verified[]`; a `v.stale_after` sets `stale_after` in the same write
    /// and is not projected into the event (§2.4, r7.1). The record gains
    /// the event bound to `seen` before the rename, and `log.md` one entry
    /// after it (module doc, "Order"). The write runs
    /// `Lenient`: it changes only lifecycle keys, so the content hash is
    /// unchanged. #10746: returns findings beside the outcome, as `put`.
    /// Test: `c35_verify_with_stale_hash_is_conflict_and_writes_nothing`,
    /// `c125_malformed_owned_values_move_to_legacy_keys`,
    /// `c146_stale_after_extension_needs_a_human_or_a_verify`,
    /// `human_verify_without_confirmation_is_refused`.
    pub fn verify(
        &mut self,
        id: &ConceptId,
        v: Verification,
        seen: &ContentHash,
    ) -> Result<(PutOutcome, Vec<Finding>), OkfError> {
        self.verify_inner(id, v, seen, false)
    }

    /// `verify` with the explicit confirmation of the person `v.by` names:
    /// the one path that writes a `human:` event (§2.4).
    ///
    /// Test: `c146_stale_after_extension_needs_a_human_or_a_verify`.
    pub fn verify_confirmed(
        &mut self,
        id: &ConceptId,
        v: Verification,
        seen: &ContentHash,
        confirmation: HumanConfirmation,
    ) -> Result<(PutOutcome, Vec<Finding>), OkfError> {
        let HumanConfirmation { _private: () } = confirmation;
        self.verify_inner(id, v, seen, true)
    }

    fn verify_inner(
        &mut self,
        id: &ConceptId,
        v: Verification,
        seen: &ContentHash,
        confirmed: bool,
    ) -> Result<(PutOutcome, Vec<Finding>), OkfError> {
        // #10746 S5: one writer at a time (§4.4). Refusals run first without
        // the lock, so a refused verify creates no lock file; the plan is then
        // made again under the lock.
        let _lock = if self.held.is_none() {
            self.plan_verify(id, &v, seen, confirmed)?;
            self.write_lock()?
        } else {
            None
        };
        let (plan, mut findings) = self.plan_verify(id, &v, seen, confirmed)?;
        let outcome = plan.outcome;
        findings.extend(self.commit_op(plan, &v.by, &v.at, LogMode::Entry, OpKind::Verify)?);
        Ok((outcome, findings))
    }

    /// Every refusal, the render and the record line of a `verify`, writing
    /// nothing (#10746 S5).
    fn plan_verify(
        &self,
        id: &ConceptId,
        v: &Verification,
        seen: &ContentHash,
        confirmed: bool,
    ) -> Result<(PutPlan, Vec<Finding>), OkfError> {
        // #10746: a `human:` actor alone is not explicit confirmation (§2.4).
        if is_human(&v.by) && !confirmed {
            return Err(conflict(id, "human_not_confirmed"));
        }
        let c = self.get(id)?;
        if &content_hash(&c) != seen {
            return Err(conflict(id, "stale_verify_hash"));
        }
        let mut next = c.clone();
        let mut findings = Vec::new();
        if malformed_owned(&c, "verified") {
            move_to_legacy(id, &mut next, "verified", &mut findings)?;
        }
        let mut list = match next.get("verified") {
            Some(Value::Sequence(s)) => s.clone(),
            Some(one @ Value::Mapping(_)) => vec![one.clone()],
            _ => Vec::new(),
        };
        let at = instant_string(&v.at);
        let mut event = Mapping::new();
        event.insert("by".into(), Value::String(v.by.to_string()));
        event.insert("at".into(), Value::String(at.clone()));
        list.push(Value::Mapping(event));
        next.set("verified", Value::Sequence(list));
        if let Some(t) = &v.stale_after {
            next.set("stale_after", Value::String(instant_string(t)));
        }
        debug_assert_eq!(
            &content_hash(&next),
            seen,
            "verify changes lifecycle keys only"
        );
        // #10746: refusals and path checks first; S5: the record line, the
        // concept and `log.md` change as one journaled `Verify` operation.
        let target = confined_file(&self.root, &id.file_name())?;
        let pre = read_existing(&target)?;
        let (prepared, write_findings) =
            prepare_write_with(&self.root, &next, WriteMode::Lenient, pre.as_deref())?;
        findings.extend(write_findings);
        let line = Line {
            by: v.by.to_string(),
            at,
            hash: Some(seen.as_str().to_string()),
            invalidated_at: None,
            invalidated_by_hash: None,
            reason: None,
            op: None,
            step: None,
        };
        let outcome = match prepared.outcome {
            WriteOutcome::Unchanged => PutOutcome::Unchanged,
            WriteOutcome::Created => PutOutcome::Created,
            WriteOutcome::Updated => PutOutcome::Updated {
                content_changed: false,
                verifications_cleared: 0,
            },
        };
        let plan = PutPlan {
            id: id.clone(),
            outcome,
            findings: Vec::new(),
            pre,
            post: prepared.bytes().to_vec(),
            content: seen.clone(),
            lines: vec![line],
            log: format!("**Verified** `/{}` by `{}`", id.file_name(), v.by),
        };
        Ok((plan, findings))
    }
}

/// The `log.md` entry for a `put`.
fn log_line(id: &ConceptId, by: &Actor, outcome: &PutOutcome) -> String {
    match outcome {
        PutOutcome::Created => format!("**Created** `/{}` by `{by}`", id.file_name()),
        PutOutcome::Updated {
            content_changed: true,
            verifications_cleared,
        } => format!(
            "**Updated** `/{}` by `{by}`; content changed, {verifications_cleared} \
             verification event(s) moved to history",
            id.file_name()
        ),
        _ => format!(
            "**Updated** `/{}` by `{by}`; lifecycle keys only",
            id.file_name()
        ),
    }
}

/// Whether `by` is a human, by the test the record reader applies.
///
/// #10746: the record stores `by.to_string()` and `record::state_of` reads
/// it back with `Actor::parse`, so `Other("human:jo")` or a producer named
/// `human:jo` is a human there; the §2.4 gate must agree.
/// Test: `human_verify_refuses_an_other_actor_with_a_human_prefix`,
/// `human_verify_refuses_a_producer_named_human`.
fn is_human(by: &Actor) -> bool {
    matches!(Actor::parse(&by.to_string()), Actor::Human(_))
}
