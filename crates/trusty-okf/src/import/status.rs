//! `import status`: per-store, per-axis counts and interrupted runs
//! (SPEC-OKF-01 §2.14 rule 10, §4.10).
//!
//! Why: progress comes from the ledger (DOC-55 §6.3), not from a second
//! durable state, and §4.9 keeps the three axes apart in every surface.
//! What: [`import_status`] reads the registry, each store's ledger, each
//! ledger concept's save and verification axes, and, when the caller passes
//! an index state directory and a live identity, its index axis. It lists
//! every logged attempt with whether it finished. Reads only. Slice S8 of
//! #10746.
//! Test: `c83_killed_run_reruns_to_the_same_bundle_and_reads_interrupted`.

use serde::Serialize;

use super::runlog::all_lines;
use crate::OkfError;
use crate::build::ledger::{ItemStatus, Ledger};
use crate::build::registry::SourceRegistry;
use crate::bundle::Bundle;
use crate::bundle::paths::read_rel;
use crate::bundle::record::VerificationState;
use crate::format::hash::bytes_hash;
use crate::format::id::ConceptId;
use crate::state::revisions::{self, save_state};
use crate::state::{IndexState, IndexStateDir, LiveIdentity, SaveState};

/// One import attempt (§2.14 rule 10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunStatus {
    /// The attempt ID.
    pub attempt: String,
    /// The store ID.
    pub store: String,
    /// The `now` of its start line.
    pub started: String,
    /// A start line with no finish line.
    pub interrupted: bool,
}

/// One store's counts, one axis at a time (§4.9 "Reporting rule").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct StoreStatus {
    /// The store ID.
    pub store: String,
    /// The store kind.
    pub kind: String,
    /// Ledger items in the ingested state.
    pub live: usize,
    /// Ledger items tombstoned.
    pub tombstoned: usize,
    /// Concepts on disk whose bytes trusty-okf recorded.
    pub saved: usize,
    /// Concepts on disk whose bytes changed outside trusty-okf.
    pub saved_external: usize,
    /// Ledger concepts absent from disk.
    pub absent: usize,
    /// Concepts whose current revision is Verified.
    pub verified: usize,
    /// Concepts whose current revision is Unverified.
    pub unverified: usize,
    /// Index axis counts, when the caller passed a state directory:
    /// `[pending, acknowledged, searchable, withdraw_pending, withdrawn]`.
    pub index: Option<[usize; 5]>,
}

/// Per-store counts and every logged attempt.
///
/// What: `store` narrows the stores; `index` is the caller's state
/// directory, index ID and live identity for the index axis. Errors: an
/// unreadable registry, ledger, journal or concept.
pub fn import_status(
    bundle: &Bundle,
    store: Option<&str>,
    index: Option<(&IndexStateDir, &str, &LiveIdentity)>,
) -> Result<(Vec<StoreStatus>, Vec<RunStatus>), OkfError> {
    let root = bundle.root();
    let lines = revisions::read(root)?;
    let mut stores = Vec::new();
    for spec in SourceRegistry::load(root)?.sources {
        if store.is_some_and(|s| s != spec.id) {
            continue;
        }
        let ledger = Ledger::load(root, &spec.id)?;
        let mut st = StoreStatus {
            store: spec.id.clone(),
            kind: spec.kind().to_string(),
            index: index.map(|_| [0; 5]),
            ..StoreStatus::default()
        };
        for r in ledger.records() {
            match r.status {
                ItemStatus::Ingested => st.live += 1,
                ItemStatus::Tombstoned => st.tombstoned += 1,
            }
            let Ok(id) = ConceptId::parse(&r.entity) else {
                continue;
            };
            let Some(bytes) = read_rel(root, &id.file_name())? else {
                st.absent += 1;
                continue;
            };
            match save_state(&lines, id.as_str(), &bytes_hash(&bytes)) {
                SaveState::Saved => st.saved += 1,
                SaveState::SavedExternal => st.saved_external += 1,
            }
            match bundle.verification_state(&id)? {
                VerificationState::Verified { .. } => st.verified += 1,
                VerificationState::Unverified { .. } => st.unverified += 1,
            }
            if let (Some((dir, index_id, live)), Some(counts)) = (index, st.index.as_mut()) {
                let slot = match bundle.revision_state(&id, dir, index_id, live)?.index {
                    IndexState::NotIndexed | IndexState::Pending { .. } => 0,
                    IndexState::Acknowledged => 1,
                    IndexState::Searchable => 2,
                    IndexState::WithdrawPending => 3,
                    IndexState::Withdrawn => 4,
                };
                counts[slot] += 1;
            }
        }
        stores.push(st);
    }
    let mut runs: Vec<RunStatus> = Vec::new();
    for l in all_lines(bundle)? {
        if store.is_some_and(|s| s != l.store) {
            continue;
        }
        match l.rec.as_str() {
            "start" => runs.push(RunStatus {
                attempt: l.attempt,
                store: l.store,
                started: l.now,
                interrupted: true,
            }),
            "finish" => {
                if let Some(r) = runs.iter_mut().find(|r| r.attempt == l.attempt) {
                    r.interrupted = false;
                }
            }
            _ => {}
        }
    }
    Ok((stores, runs))
}
