//! `Bundle::revision_state` and `Bundle::state_summary` (SPEC-OKF-01 §4.3,
//! §4.9).
//!
//! Why: saved, indexed and verified are three independent axes of the
//! current revision (§1.1); no surface may report one on behalf of another.
//! What: [`Bundle::revision_state`] reads one concept's file once and
//! composes the three axes plus the OKF trust tier; [`Bundle::state_summary`]
//! counts each axis over every concept, reading each journal once, and
//! computes Synced from the checked index states at each read (§4.6, §4.9.2
//! rule 6). Both are read-only.
//! Test: `c60_saved_and_pending_after_a_failed_push`,
//! `c64_out_of_band_edit_reads_saved_external_and_unverified`,
//! `c118_copied_bundle_reads_pending_on_another_machine`,
//! `c117_planted_verification_hash_never_verifies`.

use crate::OkfError;
use crate::bundle::Bundle;
use crate::bundle::paths::{confined_file, io_err};
use crate::bundle::record::{self, VerificationState};
use crate::format::concept::{Concept, parse_concept};
use crate::format::fields::{Field, Status};
use crate::format::hash::{ContentHash, bytes_hash, content_hash};
use crate::format::id::ConceptId;
use crate::format::validate::{Finding, FindingKind, Severity};
use crate::format::{TrustTier, trust_tier};
use crate::state::index::{IndexJournal, IndexRecord, IndexStateDir, LiveIdentity, index_state};
use crate::state::revisions::{self, RevisionLine};
use crate::state::{ByteHash, IndexState, SaveState};

/// The three axes of one concept's current revision (SPEC-OKF-01 §4.2).
///
/// #10746: `findings` is added to the §4.2 sketch, because criterion 64
/// needs the §2.10 out-of-band warning beside the state.
#[derive(Debug, Clone, PartialEq)]
pub struct RevisionState {
    /// The concept.
    pub id: ConceptId,
    /// §2.10 content hash of the current file.
    pub revision: ContentHash,
    /// Byte hash of the current file; keys the index state.
    pub bytes: ByteHash,
    /// Axis 1.
    pub save: SaveState,
    /// Axis 2, checked against the live identity.
    pub index: IndexState,
    /// Axis 3, from the verification record only.
    pub verification: VerificationState,
    /// The advisory OKF trust tier from raw frontmatter, never the state.
    pub okf_tier: TrustTier,
    /// Warnings about this read (`verification_mismatch`).
    pub findings: Vec<Finding>,
}

/// Per-axis counts over every concept (§4.9 "Reporting rule").
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StateSummary {
    /// Concepts read.
    pub concepts: usize,
    /// Axis 1: Saved.
    pub saved: usize,
    /// Axis 1: SavedExternal.
    pub saved_external: usize,
    /// Axis 2: NotIndexed.
    pub not_indexed: usize,
    /// Axis 2: Pending.
    pub pending: usize,
    /// Axis 2: Acknowledged.
    pub acknowledged: usize,
    /// Axis 2: Searchable.
    pub searchable: usize,
    /// Axis 2: WithdrawPending.
    pub withdraw_pending: usize,
    /// Axis 2: Withdrawn.
    pub withdrawn: usize,
    /// Axis 3: Verified.
    pub verified: usize,
    /// Axis 3: Unverified.
    pub unverified: usize,
}

impl StateSummary {
    /// Bundle-level Synced (§4.6): every live concept Searchable and every
    /// withdrawn path Withdrawn. Computed, never stored (§4.9.2 rule 6).
    pub fn synced(&self) -> bool {
        self.concepts > 0
            && self.not_indexed + self.pending + self.acknowledged + self.withdraw_pending == 0
    }
}

/// Whether the concept should be out of the index: deprecated (3i A) or
/// tombstoned (§4.8).
pub(crate) fn withdraw_intended(c: &Concept) -> bool {
    matches!(c.frontmatter().status, Field::Valid(Status::Deprecated))
        || c.get("source_status").and_then(|v| v.as_str()) == Some("deleted")
}

/// #10746 S8: a tombstone trusty-search will not detect (§4.8): it never
/// reads Withdrawn, and every feed pass removes it again (F9).
/// Test: `c141_overgrown_tombstone_lands_undetectable_and_never_withdrawn`.
pub(crate) fn undetectable_tombstone(c: &Concept, text: &str) -> bool {
    c.get("source_status").and_then(|v| v.as_str()) == Some("deleted")
        && !crate::format::tombstone::is_detectable_tombstone(text)
}

/// The journals read once for a whole-bundle pass.
pub(crate) struct Loaded {
    revisions: Vec<RevisionLine>,
    pub(crate) records: Vec<IndexRecord>,
}

impl Bundle {
    pub(crate) fn load_state(
        &self,
        dir: &IndexStateDir,
        index_id: &str,
    ) -> Result<Loaded, OkfError> {
        let journal = IndexJournal::open(dir, self.root(), index_id)?;
        Ok(Loaded {
            revisions: revisions::read(self.root())?,
            records: journal.read()?,
        })
    }

    pub(crate) fn state_with(
        &self,
        id: &ConceptId,
        loaded: &Loaded,
        live: &LiveIdentity,
    ) -> Result<RevisionState, OkfError> {
        let path = confined_file(self.root(), &id.file_name())?;
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(OkfError::NotFound(id.clone()));
            }
            Err(e) => return Err(io_err(&path, e)),
        };
        let text = String::from_utf8_lossy(&bytes);
        let (c, _) = parse_concept(id.clone(), &text)?;
        let disk = bytes_hash(&bytes);
        let revision = content_hash(&c);
        let events = record::counted_events(self, id, &c)?;
        let mut findings = Vec::new();
        if record::has_mismatch(&events, &revision) {
            let mut f = Finding::new(
                Severity::Warning,
                FindingKind::VerificationMismatch,
                id.file_name(),
                "content changed since its recorded verification".to_string(),
                "SPEC-OKF-01 §2.10",
            );
            f.concept = Some(id.clone());
            findings.push(f);
        }
        Ok(RevisionState {
            id: id.clone(),
            save: revisions::save_state(&loaded.revisions, id.as_str(), &disk),
            index: if undetectable_tombstone(&c, &text) {
                IndexState::WithdrawPending
            } else {
                index_state(
                    &loaded.records,
                    &id.file_name(),
                    &disk,
                    withdraw_intended(&c),
                    live,
                )
            },
            verification: record::state_of(&events, &revision),
            okf_tier: trust_tier(c.frontmatter()),
            revision,
            bytes: ByteHash(disk),
            findings,
        })
    }

    /// The three states of `id`'s current revision (SPEC-OKF-01 §4.3).
    ///
    /// Why: see the module doc. What: read-only; detects SavedExternal from
    /// the revision journal, checks every index record against `live`
    /// (§4.9.2), and reads verification from the record only. `state` inside
    /// the bundle is `StateDirInBundle`.
    /// Test: `c60_saved_and_pending_after_a_failed_push`,
    /// `c64_out_of_band_edit_reads_saved_external_and_unverified`,
    /// `c117_planted_verification_hash_never_verifies`.
    pub fn revision_state(
        &self,
        id: &ConceptId,
        state: &IndexStateDir,
        index_id: &str,
        live: &LiveIdentity,
    ) -> Result<RevisionState, OkfError> {
        let loaded = self.load_state(state, index_id)?;
        self.state_with(id, &loaded, live)
    }

    /// Per-axis counts over every concept on disk (SPEC-OKF-01 §4.3).
    ///
    /// What: one [`revision_state`](Bundle::revision_state) per concept with
    /// each journal read once; a concept whose file cannot be read or parsed
    /// is an error, never skipped.
    /// Test: `c118_copied_bundle_reads_pending_on_another_machine`,
    /// `c63_acknowledgement_alone_is_not_searchable`.
    pub fn state_summary(
        &self,
        state: &IndexStateDir,
        index_id: &str,
        live: &LiveIdentity,
    ) -> Result<StateSummary, OkfError> {
        let loaded = self.load_state(state, index_id)?;
        let mut s = StateSummary::default();
        for id in self.concept_ids()? {
            let r = self.state_with(&id, &loaded, live)?;
            s.concepts += 1;
            match r.save {
                SaveState::Saved => s.saved += 1,
                SaveState::SavedExternal => s.saved_external += 1,
            }
            match r.index {
                IndexState::NotIndexed => s.not_indexed += 1,
                IndexState::Pending { .. } => s.pending += 1,
                IndexState::Acknowledged => s.acknowledged += 1,
                IndexState::Searchable => s.searchable += 1,
                IndexState::WithdrawPending => s.withdraw_pending += 1,
                IndexState::Withdrawn => s.withdrawn += 1,
            }
            match r.verification {
                VerificationState::Verified { .. } => s.verified += 1,
                VerificationState::Unverified { .. } => s.unverified += 1,
            }
        }
        Ok(s)
    }
}
