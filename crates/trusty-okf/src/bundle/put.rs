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
//! human-reviewed concept from a non-`human:` actor
//! (`stale_after_extension`). On a content change it applies §2.10: a
//! malformed `verified`/`generated` moves to its `legacy_` key with its raw
//! bytes, every frontmatter event moves to the record, `verified` leaves the
//! frontmatter, and `generated` becomes `{by, at: now}`. Then the shared
//! strict write runs, the record is appended and `log.md` gains one entry.
//! [`Bundle::verify`] binds a new event to the hash the verifier saw.
//! Residual risk until slice S5's operation journal: a crash between the
//! file rename and the record or `log.md` append leaves those behind.
//! Test: `a1_put_never_replaces_an_existing_file_with_a_new_concept`,
//! `a1_put_refuses_a_stale_write`, `a2_strict_check_runs_on_rendered_values`,
//! `a3_put_preserves_the_target_file_mode`,
//! `c31_body_change_moves_verification_to_the_record`,
//! `c32_description_change_clears_verification`,
//! `c33_status_or_stale_after_change_keeps_verification`,
//! `c35_verify_with_stale_hash_is_conflict_and_writes_nothing`,
//! `c125_malformed_owned_values_move_to_legacy_keys`,
//! `c146_stale_after_extension_needs_a_human_or_a_verify`.

use chrono::{DateTime, FixedOffset};
use serde_yaml::{Mapping, Value};

use crate::OkfError;
use crate::bundle::Bundle;
use crate::bundle::log_md::append_entry;
use crate::bundle::paths::{confined_file, fold_conflict};
use crate::bundle::record::{self, Line, instant_string};
use crate::format::concept::{Concept, parse_concept};
use crate::format::fields::{Actor, Field, Status, Verification};
use crate::format::hash::{ContentHash, content_hash};
use crate::format::id::{ConceptId, ConceptRef};
use crate::format::validate::{Finding, FindingKind, Severity};
use crate::format::write::{
    WriteMode, WriteOutcome, check_not_stale, foreign_refusal, read_existing, write_concept,
};

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
    /// criterion 125 needs the `legacy_value_moved` warnings. A `human:`
    /// actor is taken as the caller's explicit confirmation (§2.4); the §4.3
    /// signature carries no separate flag.
    /// Test: see the module doc.
    pub fn put(
        &mut self,
        c: Concept,
        by: Actor,
        mode: WriteMode,
        now: DateTime<FixedOffset>,
    ) -> Result<(PutOutcome, Vec<Finding>), OkfError> {
        let (outcome, findings, id) = self.put_inner(c, &by, mode, &now, false)?;
        if outcome != PutOutcome::Unchanged {
            append_entry(&self.root, &now, &log_line(&id, &by, &outcome))?;
        }
        Ok((outcome, findings))
    }

    /// `put` without the `log.md` entry; `move_concept` logs once for the
    /// whole move. `link_rewrite` marks a `move_concept` link rewrite, which
    /// is not a caller edit, so a deprecated concept still takes it (§4.8
    /// refuses caller edits only). Returns the outcome, the findings and the
    /// target ID.
    pub(crate) fn put_inner(
        &mut self,
        c: Concept,
        by: &Actor,
        mode: WriteMode,
        now: &DateTime<FixedOffset>,
        link_rewrite: bool,
    ) -> Result<(PutOutcome, Vec<Finding>, ConceptId), OkfError> {
        let id = match &c.id {
            ConceptRef::Id(id) => id.clone(),
            ConceptRef::Foreign(p) => return Err(foreign_refusal(p)),
        };
        let target = confined_file(&self.root, &id.file_name())?;
        if fold_conflict(&self.root, &id.file_name())?.is_some() {
            return Err(conflict(&id, "fold_collision"));
        }
        let existing = read_existing(&target)?;
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
            if content_changed && deprecated && !link_rewrite {
                return Err(conflict(&id, "deprecated"));
            }
            if extends_stale_after(o, &next)
                && !matches!(by, Actor::Human(_))
                && matches!(
                    record::state_for(self, &id, o)?,
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
                let folded = record::fold(record::read_lines(self, &id)?);
                let new_hash = content_hash(&next);
                lines =
                    record::invalidation_lines(&folded, &events, &instant_string(now), &new_hash);
                cleared = events.len();
            }
        }
        if content_changed {
            next.set("generated", generated_value(by, now));
        }
        let (written, write_findings) = write_concept(&self.root, &next, mode)?;
        findings.extend(write_findings);
        record::append(self, &id, &lines)?;
        let outcome = match written {
            WriteOutcome::Unchanged => PutOutcome::Unchanged,
            WriteOutcome::Created => PutOutcome::Created,
            WriteOutcome::Updated => PutOutcome::Updated {
                content_changed,
                verifications_cleared: cleared,
            },
        };
        Ok((outcome, findings, id))
    }

    /// Record a verification of the revision the verifier saw (§2.10).
    ///
    /// Why: a person must not verify a revision that changed after they read
    /// it, and only a record event bound to the current hash counts.
    /// What: `seen` must equal the current content hash, else
    /// `Conflict { reason: "stale_verify_hash" }` and nothing is written. A
    /// malformed `verified` first moves to `legacy_verified` (criterion 125;
    /// `Conflict` when that key exists). The event `{by, at}` is appended to
    /// `verified[]`; a `v.stale_after` sets `stale_after` in the same write
    /// and is not projected into the event (§2.4, r7.1). The record gains
    /// the event bound to `seen`, and `log.md` one entry. The write runs
    /// `Lenient`: it changes only lifecycle keys, so the content hash is
    /// unchanged. #10746: returns findings beside the outcome, as `put`.
    /// Test: `c35_verify_with_stale_hash_is_conflict_and_writes_nothing`,
    /// `c125_malformed_owned_values_move_to_legacy_keys`,
    /// `c146_stale_after_extension_needs_a_human_or_a_verify`.
    pub fn verify(
        &mut self,
        id: &ConceptId,
        v: Verification,
        seen: &ContentHash,
    ) -> Result<(PutOutcome, Vec<Finding>), OkfError> {
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
        let (written, write_findings) = write_concept(&self.root, &next, WriteMode::Lenient)?;
        findings.extend(write_findings);
        let line = Line {
            by: v.by.to_string(),
            at,
            hash: Some(seen.as_str().to_string()),
            invalidated_at: None,
            invalidated_by_hash: None,
            reason: None,
        };
        record::append(self, id, &[line])?;
        append_entry(
            &self.root,
            &v.at,
            &format!("**Verified** `/{}` by `{}`", id.file_name(), v.by),
        )?;
        let outcome = match written {
            WriteOutcome::Unchanged => PutOutcome::Unchanged,
            WriteOutcome::Created => PutOutcome::Created,
            WriteOutcome::Updated => PutOutcome::Updated {
                content_changed: false,
                verifications_cleared: 0,
            },
        };
        Ok((outcome, findings))
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
