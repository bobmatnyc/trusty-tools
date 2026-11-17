//! `Bundle::adopt`: operator adoption of reviewed legacy bytes (SPEC-OKF-01
//! §3 (q) C, §4.5 "Adopt", r7.1).
//!
//! Why: a ledger-owned concept written before r4 carries no written hash, so
//! `rebuild` must preserve it as possibly hand-edited. The equality proof
//! covers most of them; for the rest a person may review the bytes and adopt
//! them. Adoption binds proof to the reviewed bytes, as `verify` binds an
//! event to `seen`, so a file edited after the review is never adopted.
//! What: refuses the whole call, with no file changed, unless `by` is a
//! `human:` actor (`AdoptRefused { reason: "not_human" }`) and the call
//! carries a [`HumanConfirmation`] (`not_confirmed`). Then, per pair: a
//! missing concept is `not_found`; a concept no ledger names is
//! `not_ledger_owned`; disk bytes other than the reviewed hash are
//! `byte_hash_mismatch`; a concept whose bytes already have a proven record
//! is `already_proven`. Every other pair gains one proven revision line with
//! `origin: source` (op kind `adopt`); all lines are appended in one synced
//! write. Adoption never touches a verification event (§2.10 "Events with no
//! hash" rule 2). Slice S6 of #10746.
//! Test: `c142_legacy_hashless_events_never_gain_a_hash`,
//! `c142_adopt_refusals_change_nothing`.

use chrono::{DateTime, FixedOffset};

use crate::OkfError;
use crate::build::ledger::owned_entities;
use crate::bundle::Bundle;
use crate::bundle::paths::{append_line, read_rel};
use crate::bundle::put::HumanConfirmation;
use crate::bundle::record::instant_string;
use crate::format::concept::parse_concept;
use crate::format::fields::Actor;
use crate::format::hash::{bytes_hash, content_hash};
use crate::format::id::ConceptId;
use crate::state::ByteHash;
use crate::state::revisions::{self, ORIGIN_SOURCE, REVISIONS_REL, RevisionLine, run_id};

/// What `adopt` did (SPEC-OKF-01 §4.2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdoptReport {
    /// Concepts whose reviewed bytes are now proven engine-written.
    pub adopted: Vec<ConceptId>,
    /// Concepts left unchanged, with the §4.5 reason.
    pub refused: Vec<(ConceptId, String)>,
}

impl Bundle {
    /// Record reviewed legacy bytes as engine-written (SPEC-OKF-01 §4.3).
    ///
    /// Why/What: see the module doc. #10746: the §4.3 sketch takes
    /// `confirmed: bool`; this crate takes `Option<HumanConfirmation>`, the
    /// one-call token `put_confirmed` and `verify_confirmed` already use
    /// (§2.4), so `None` is `not_confirmed`.
    /// Test: `c142_legacy_hashless_events_never_gain_a_hash`,
    /// `c142_adopt_refusals_change_nothing`.
    pub fn adopt(
        &mut self,
        reviewed: &[(ConceptId, ByteHash)],
        by: Actor,
        confirmation: Option<HumanConfirmation>,
        now: DateTime<FixedOffset>,
    ) -> Result<AdoptReport, OkfError> {
        if !matches!(Actor::parse(&by.to_string()), Actor::Human(_)) {
            return Err(OkfError::AdoptRefused {
                reason: "not_human".into(),
            });
        }
        if confirmation.is_none() {
            return Err(OkfError::AdoptRefused {
                reason: "not_confirmed".into(),
            });
        }
        // #10746 S6: one writer at a time (§4.4).
        let _lock = self.write_lock()?;
        // #10746: adopt appends the revision journal; a pending migrate that
        // holds it refuses the call (§4.11 rule 2).
        self.check_recovery(&[])?;
        let owned = owned_entities(self.root())?;
        let lines = revisions::read(self.root())?;
        let stamp = instant_string(&now);
        let run = run_id("adopt", &[&stamp, &by.to_string()]);
        let mut report = AdoptReport::default();
        let mut out = Vec::new();
        for (id, seen) in reviewed {
            let refuse = |r: &mut AdoptReport, why: &str| r.refused.push((id.clone(), why.into()));
            let Some(bytes) = read_rel(self.root(), &id.file_name())? else {
                refuse(&mut report, "not_found");
                continue;
            };
            let disk = bytes_hash(&bytes);
            if !owned.contains(id.as_str()) {
                refuse(&mut report, "not_ledger_owned");
            } else if disk != seen.as_str() {
                refuse(&mut report, "byte_hash_mismatch");
            } else if revisions::record_of(&lines, id.as_str(), &disk)
                .is_some_and(RevisionLine::is_proven)
            {
                refuse(&mut report, "already_proven");
            } else {
                let text = String::from_utf8_lossy(&bytes);
                let (c, _) = parse_concept(id.clone(), &text)?;
                let line = RevisionLine {
                    id: id.as_str().to_string(),
                    content: content_hash(&c).as_str().to_string(),
                    bytes: disk,
                    op_kind: "adopt".to_string(),
                    now: stamp.clone(),
                    run: run.clone(),
                    op: None,
                    step: None,
                    proven: Some(true),
                    origin: Some(ORIGIN_SOURCE.to_string()),
                };
                out.push(line.json()?);
                report.adopted.push(id.clone());
            }
        }
        if !out.is_empty() {
            append_line(self.root(), REVISIONS_REL, &out.join("\n"))?;
        }
        Ok(report)
    }
}
