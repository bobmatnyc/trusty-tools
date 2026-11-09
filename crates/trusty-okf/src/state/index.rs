//! The machine-local index journal and its identity check (SPEC-OKF-01
//! §4.9 Axis 2, §4.9.1, §4.9.2).
//!
//! Why: an acknowledgement and a probe describe the daemon that answered
//! them. A journal synced with the bundle would let machine B read
//! Searchable for content its own daemon never received (§4.9.1), and a
//! journal that ignores the index identity reads Searchable after the index
//! was deleted, re-created or reset (§4.9.2).
//! What: [`IndexStateDir`] is the caller's directory; a directory inside
//! the bundle root is refused with `StateDirInBundle`. [`IndexJournal`]
//! lives at `<dir>/<sha256 of the canonical bundle root>/<index-id>.jsonl`.
//! The feed (slice S7) appends [`IndexRecord`]s; each confirmation names the
//! [`IndexIdentity`] it was observed against. [`index_state`] folds a path's
//! records and checks them against the caller's [`LiveIdentity`]: a record
//! with no identity (D1 absent) never reads Searchable, and an unreadable or
//! changed identity reads Pending or WithdrawPending (fail closed).
//! Test: `c118_copied_bundle_reads_pending_on_another_machine`,
//! `c119_index_state_never_lives_in_the_bundle`,
//! `c68_reindex_demotes_and_identity_change_reads_pending`,
//! `no_state_reads_searchable_without_d1`.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::OkfError;
use crate::build::jsonl::{Appender, read_journal};
use crate::bundle::paths::io_err;
use crate::state::IndexState;

/// The caller's machine-local index state directory (§4.2, §4.9.1).
///
/// #10746: trusty-okf never derives it (B3); the caller passes a directory
/// outside the synced tree (§3 (r) A).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexStateDir(PathBuf);

impl IndexStateDir {
    /// Wrap a caller-supplied directory. It need not exist yet.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self(path.into())
    }

    /// The directory as given.
    pub fn path(&self) -> &Path {
        &self.0
    }
}

/// An index identity from D1 (#10738): equal values mean the same stored
/// contents lineage (§4.9.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexIdentity {
    /// Changes when the index ID is deleted and re-created.
    pub instance_id: String,
    /// Rises when the stored contents may have lost chunks.
    pub generation: u64,
}

/// What the caller read from the live daemon (§4.2, §4.9.2 rules 4-5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveIdentity {
    /// `search.index.status` carried an identity.
    Known(IndexIdentity),
    /// The reply carried no identity: D1 is absent.
    Unsupported,
    /// Daemon down, status error or unknown index.
    Unreadable(String),
}

/// One index-journal record, written by the feed (§4.9, §5.4).
///
/// `path` is the concept's bundle-relative file name; `bytes` is the byte
/// hash pushed. `identity` is `None` when the daemon reported none (D1
/// absent); such a record never matches a known identity (§4.9.2 rule 5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IndexRecord {
    /// The daemon confirmed a `file.put` of exactly these bytes (F4).
    PutAck {
        /// Concept file, bundle-relative.
        path: String,
        /// Byte hash pushed.
        bytes: String,
        /// Identity observed with the reply.
        identity: Option<IndexIdentity>,
    },
    /// A push failed; the path stays Pending with this error.
    PutFailed {
        /// Concept file, bundle-relative.
        path: String,
        /// Byte hash the push carried.
        bytes: String,
        /// The error.
        error: String,
    },
    /// The retrieval probe ran for these bytes.
    Probe {
        /// Concept file, bundle-relative.
        path: String,
        /// Byte hash probed.
        bytes: String,
        /// Identity observed.
        identity: Option<IndexIdentity>,
        /// Whether both probe checks passed.
        passed: bool,
    },
    /// The daemon confirmed a remove of the path.
    RemoveAck {
        /// Concept file, bundle-relative.
        path: String,
        /// Identity observed.
        identity: Option<IndexIdentity>,
    },
    /// The withdrawal probe ran.
    WithdrawProbe {
        /// Concept file, bundle-relative.
        path: String,
        /// Identity observed.
        identity: Option<IndexIdentity>,
        /// Whether the probe found nothing for the path.
        passed: bool,
    },
    /// An explicit `search.index.reindex` ran: every Searchable path drops to
    /// Acknowledged until its next probe passes (criterion 68).
    Reindex {
        /// Identity observed after the reindex.
        identity: Option<IndexIdentity>,
    },
    /// #10876: a feed pass read this identity from `search.index.status`
    /// and it differed from the newest `Observed` record. An offline read
    /// checks every acknowledgement against the newest one, so a re-create
    /// or generation bump stays visible after the pass that saw it. An
    /// older binary skips this kind as an unparseable line (F11).
    Observed {
        /// The identity the status reply carried. `None`: the reply carried
        /// none after an earlier pass saw one (D1 rolled back), so an
        /// offline read stops checking acks against the stale identity.
        // #10876: `Option` so a D1 rollback is journaled; a bare identity
        // object, as the first #10876 commit wrote, still parses as `Some`.
        identity: Option<IndexIdentity>,
    },
}

/// One machine's index journal for one bundle root and one index ID.
#[derive(Debug, Clone)]
pub struct IndexJournal {
    path: PathBuf,
}

/// `p` with its longest existing prefix canonicalised (links resolved) and
/// the missing rest applied lexically (`.` skipped, `..` pops), so a state
/// directory that does not exist yet is still placed exactly.
fn resolve(p: &Path) -> Result<PathBuf, OkfError> {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::path::absolute(p).map_err(|e| io_err(p, e))?
    };
    let mut out = PathBuf::new();
    let mut existing = true;
    for comp in abs.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => {
                out.push(other);
                if existing {
                    match out.canonicalize() {
                        Ok(c) => out = c,
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => existing = false,
                        Err(e) => return Err(io_err(p, e)),
                    }
                }
            }
        }
    }
    Ok(out)
}

impl IndexJournal {
    /// The journal for `bundle_root` and `index_id` under `dir`.
    ///
    /// What: refuses a `dir` that resolves inside the canonical bundle root
    /// with `StateDirInBundle` (§4.9.1), and an `index_id` that is not one
    /// normal path segment with `PathEscape`. Creates nothing.
    /// Test: `c119_index_state_never_lives_in_the_bundle`.
    pub fn open(dir: &IndexStateDir, bundle_root: &Path, index_id: &str) -> Result<Self, OkfError> {
        let root = crate::bundle::paths::canonical_root(bundle_root)?;
        let resolved = resolve(dir.path())?;
        if resolved.starts_with(&root) {
            return Err(OkfError::StateDirInBundle(dir.path().to_path_buf()));
        }
        let mut comps = Path::new(index_id).components();
        let one = matches!(comps.next(), Some(Component::Normal(_))) && comps.next().is_none();
        if !one || index_id.contains(['/', '\\']) {
            return Err(OkfError::PathEscape(PathBuf::from(index_id)));
        }
        let digest = Sha256::digest(root.as_os_str().as_encoded_bytes());
        let key: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        Ok(Self {
            path: resolved.join(key).join(format!("{index_id}.jsonl")),
        })
    }

    /// The journal file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Durably append records (the feed's write, slice S7).
    ///
    /// Test: `c118_copied_bundle_reads_pending_on_another_machine`.
    pub fn append(&self, records: &[IndexRecord]) -> Result<(), OkfError> {
        let load = read_journal::<serde_json::Value>(&self.path)?;
        let mut app = Appender::new(self.path.clone(), load.needs_newline);
        for r in records {
            app.append(r)?;
        }
        Ok(())
    }

    /// Every parseable record, oldest first; a torn line is skipped (F11).
    pub fn read(&self) -> Result<Vec<IndexRecord>, OkfError> {
        Ok(read_journal::<IndexRecord>(&self.path)?.records)
    }
}

/// A path's folded journal state.
///
/// #10746 S7: crate-visible so the backlog (`state::backlog`) decides what
/// to push from the same fold the read model uses.
#[derive(Debug, Default, Clone)]
pub(crate) struct Folded {
    /// Newest acknowledged put: byte hash and identity.
    pub(crate) acked: Option<(String, Option<IndexIdentity>)>,
    /// A probe passed for `acked`, with no reindex since.
    pub(crate) searchable: bool,
    error: Option<(String, String)>,
    /// Newest confirmed remove (when it is newer than every put ack).
    pub(crate) removed: Option<Option<IndexIdentity>>,
    /// A withdrawal probe passed for `removed`.
    pub(crate) withdrawn: bool,
}

/// Every path any record names, sorted (§5.4 "Journal coverage").
pub(crate) fn journaled_paths(records: &[IndexRecord]) -> std::collections::BTreeSet<String> {
    records
        .iter()
        .filter_map(|r| match r {
            IndexRecord::PutAck { path, .. }
            | IndexRecord::PutFailed { path, .. }
            | IndexRecord::Probe { path, .. }
            | IndexRecord::RemoveAck { path, .. }
            | IndexRecord::WithdrawProbe { path, .. } => Some(path.clone()),
            // #10876: an identity observation names no path.
            IndexRecord::Reindex { .. } | IndexRecord::Observed { .. } => None,
        })
        .collect()
}

/// The identity of the newest [`IndexRecord::Observed`] (#10876).
///
/// `None` when there is no observation or the newest one records D1 absent.
/// Test: `observed_record_written_once_per_identity_change`,
/// `d1_rollback_journals_one_absent_observation`.
pub(crate) fn last_observed_identity(records: &[IndexRecord]) -> Option<&IndexIdentity> {
    records
        .iter()
        .rev()
        .find_map(|r| match r {
            // #10876: a newest `None` (D1 rolled back) hides older identities.
            IndexRecord::Observed { identity } => Some(identity.as_ref()),
            _ => None,
        })
        .flatten()
}

/// Fold `records` for `path` in journal order.
pub(crate) fn fold(records: &[IndexRecord], path: &str) -> Folded {
    let mut f = Folded::default();
    for r in records {
        match r {
            IndexRecord::PutAck {
                path: p,
                bytes,
                identity,
            } if p == path => {
                f = Folded {
                    acked: Some((bytes.clone(), identity.clone())),
                    ..Folded::default()
                };
            }
            IndexRecord::PutFailed {
                path: p,
                bytes,
                error,
            } if p == path => {
                f.error = Some((bytes.clone(), error.clone()));
            }
            IndexRecord::Probe {
                path: p,
                bytes,
                identity,
                passed,
            } if p == path => {
                f.searchable =
                    *passed && f.acked.as_ref() == Some(&(bytes.clone(), identity.clone()));
            }
            IndexRecord::RemoveAck { path: p, identity } if p == path => {
                f = Folded {
                    removed: Some(identity.clone()),
                    ..Folded::default()
                };
            }
            IndexRecord::WithdrawProbe {
                path: p,
                identity,
                passed,
            } if p == path => {
                f.withdrawn = *passed && f.removed.as_ref() == Some(identity);
            }
            IndexRecord::Reindex { .. } => f.searchable = false,
            _ => {}
        }
    }
    f
}

/// Why a recorded identity does not hold, or `None` when it matches.
fn mismatch(recorded: &Option<IndexIdentity>, live: &LiveIdentity) -> Option<Option<String>> {
    match live {
        LiveIdentity::Unsupported => Some(Some("index_identity_unsupported".to_string())),
        LiveIdentity::Unreadable(e) => Some(Some(format!("index_identity_unreadable: {e}"))),
        LiveIdentity::Known(l) if recorded.as_ref() == Some(l) => None,
        LiveIdentity::Known(_) => Some(None),
    }
}

/// The index axis of one path (§4.9 Axis 2, §4.9.2).
///
/// What: `withdraw` is true when the concept should not be in the index
/// (deprecated or tombstoned). A live path reads Searchable only when its
/// newest acknowledgement names `bytes`, its identity equals the known live
/// identity, and a probe passed after it with no reindex since; Acknowledged
/// without that probe; Pending otherwise, with the identity reason or the
/// newest push error for these bytes. A withdrawn path reads Withdrawn only
/// with a confirmed remove and a passed withdrawal probe under the live
/// identity; WithdrawPending otherwise. No record at all reads Pending
/// (§4.9.1 rule 3, criterion 118).
/// Test: `c68_reindex_demotes_and_identity_change_reads_pending`,
/// `no_state_reads_searchable_without_d1`.
pub(crate) fn index_state(
    records: &[IndexRecord],
    path: &str,
    bytes: &str,
    withdraw: bool,
    live: &LiveIdentity,
) -> IndexState {
    let f = fold(records, path);
    if withdraw {
        let ok = f.withdrawn
            && f.removed
                .as_ref()
                .is_some_and(|i| mismatch(i, live).is_none());
        return if ok {
            IndexState::Withdrawn
        } else {
            IndexState::WithdrawPending
        };
    }
    let push_error = f
        .error
        .as_ref()
        .filter(|(b, _)| b == bytes)
        .map(|(_, e)| e.clone());
    match &f.acked {
        Some((b, identity)) if b == bytes => match mismatch(identity, live) {
            None if f.searchable => IndexState::Searchable,
            None => IndexState::Acknowledged,
            Some(reason) => IndexState::Pending {
                last_error: reason.or(push_error),
            },
        },
        _ => IndexState::Pending {
            last_error: push_error,
        },
    }
}
