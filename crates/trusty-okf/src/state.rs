//! Revision journal, machine-local index state and the three-state read
//! model (SPEC-OKF-01 §4.9, §4.9.1, §4.9.2).
//!
//! Why: §1.1 tracks saved, indexed/searchable and verified independently for
//! the current revision. Save state and verification describe the files and
//! stay with the bundle; index state describes one machine's daemon and
//! never lives in the bundle.
//! What: [`revisions`] is the bundle's `_sources/revisions.jsonl`; [`index`]
//! is the caller's [`IndexStateDir`] journal and the identity check; [`read`]
//! composes them into [`RevisionState`] and [`StateSummary`]. Slice S5 of
//! #10746. No I/O beyond bundle files and the caller's state directory; no
//! network (B8): the caller passes the live identity it read.
//! Test: `c118_copied_bundle_reads_pending_on_another_machine`,
//! `c119_index_state_never_lives_in_the_bundle`.

pub mod index;
pub mod read;
pub(crate) mod revisions;

pub use index::{IndexIdentity, IndexJournal, IndexRecord, IndexStateDir, LiveIdentity};
pub use read::{RevisionState, StateSummary};

/// `sha256:<hex>` of a concept file's bytes; keys the index state (§4.2).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ByteHash(pub(crate) String);

impl ByteHash {
    /// #10746 S6: the byte hash of `bytes`, as `adopt` takes it (§4.3).
    pub fn of(bytes: &[u8]) -> ByteHash {
        ByteHash(crate::format::hash::bytes_hash(bytes))
    }

    /// The `sha256:<hex>` string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Axis 1 (§4.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveState {
    /// The file holds bytes trusty-okf recorded in the revision journal.
    Saved,
    /// The file differs from every recorded write, or no record exists.
    SavedExternal,
}

/// Axis 2 (§4.9), after the §4.9.2 identity check.
///
/// #10746: `NotIndexed` is kept from §4.2 but no read produces it: §4.9.1
/// rule 3 and criterion 118 read a concept with no journal record as
/// Pending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexState {
    /// No push attempted.
    NotIndexed,
    /// A push or re-push is due; carries the last error, if any.
    Pending {
        /// The newest push error for these bytes, or the identity reason.
        last_error: Option<String>,
    },
    /// The daemon confirmed a put of these bytes under the live identity.
    Acknowledged,
    /// Acknowledged, and the retrieval probe passed.
    Searchable,
    /// Should be out of the index; withdrawal not confirmed.
    WithdrawPending,
    /// Remove confirmed and the withdrawal probe found nothing.
    Withdrawn,
}
