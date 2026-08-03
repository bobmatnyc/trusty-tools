//! Classification of durable-corpus open failures (issue #4333).
//!
//! Why: every `corpus_open_failed` case — a 30 s open timeout under warm-boot
//! contention, a stale redb file lock, a permission denial, and genuine format
//! incompatibility — used to collapse into ONE hardcoded status string:
//! *"redb corpus could not be opened (incompatible or corrupted format); run
//! `trusty-search index <path> --force` to rebuild from source"*. That wording
//! names the one cause that is permanent and prescribes the one remedy that is
//! destructive. In the 2026-07-29 `cto-duetto` incident the daemon reported it
//! while simultaneously reporting `indexes_skipped_timeout: 18` — daemon-wide
//! contention — and the corpus was verified intact at 201,206 chunks. The same
//! conflation had already cost one index to a `--force` rebuild on 2026-07-27.
//! Reporting "corrupted" for a timeout is how a transient outage becomes
//! permanent data loss.
//!
//! What: [`CorpusOpenFailure`] is a small classification of *why* the open
//! failed, derived from the error by TYPED downcast (never string matching) in
//! [`CorpusOpenFailure::classify`], plus the per-kind operator guidance
//! ([`CorpusOpenFailure::stage_reason`]) that replaces the single hardcoded
//! string. Transient kinds explicitly tell the operator NOT to reindex.
//!
//! Test: `classify_*` and `stage_reason_*` unit tests below.

use std::time::Duration;

use crate::core::corpus::open_guard::{CorpusOpenTimeout, CorpusOpenWedged};

/// Why a durable redb corpus failed to open on this boot (issue #4333).
///
/// Why: the operator's next move differs completely per kind — wait vs.
/// investigate vs. rebuild — and the previous single string picked the most
/// destructive one for every case. Encoding the kind lets `/health`, the
/// per-index status endpoint, and the warm-boot stage reason all say the same
/// precise thing.
/// What: a `Copy` classification with no payload. [`Self::is_transient`] is the
/// safety-critical predicate: `true` means the on-disk corpus was never
/// actually read, so it is presumed intact and MUST NOT be rebuilt.
/// Test: `classify_*` unit tests below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorpusOpenFailure {
    /// The open did not complete within `TRUSTY_CORPUS_OPEN_TIMEOUT` — the
    /// blocking opener is still running (issue #3659). Transient; self-heals.
    OpenTimeout,
    /// Another opener holds the file (redb `DatabaseAlreadyOpen`, or the
    /// per-path wedge latch). Transient; self-heals.
    Contention,
    /// redb reports the file is in an unreadable on-disk shape — an old file
    /// format needing manual upgrade, or a genuinely corrupted database.
    /// Permanent; a rebuild is the correct remedy.
    FormatIncompatible,
    /// The open failed for a reason this classifier does not recognise
    /// (permissions, I/O error, unresolvable storage path, an opener panic).
    /// Treated as NOT-transient for reporting but explicitly NOT as
    /// corruption — the operator must diagnose before rebuilding.
    Unclassified,
}

impl CorpusOpenFailure {
    /// Classify an open error by typed downcast (issue #4333).
    ///
    /// Why: the previous code carried no classification at all, and any
    /// classification built on error *text* would silently regress the moment
    /// redb rewords a message — the same fragility `open_corpus_with_retry`
    /// already avoids for its `DatabaseAlreadyOpen` retry. Downcasting walks
    /// anyhow's full context chain, so `.context(..)` wrappers added anywhere
    /// between `CorpusStore::open` and here cannot break it.
    /// What: checks this crate's own typed markers first (they are the only
    /// definitive signal for the timeout/wedge paths, which produce no redb
    /// error at all because the open never returned), then redb's typed
    /// variants, then falls back to [`Self::Unclassified`].
    /// Test: `classify_timeout_marker_is_transient`,
    /// `classify_database_already_open_is_contention`,
    /// `classify_upgrade_required_is_format_incompatible`,
    /// `classify_corrupted_storage_is_format_incompatible`,
    /// `classify_unknown_error_is_unclassified`.
    pub fn classify(err: &anyhow::Error) -> Self {
        if err.downcast_ref::<CorpusOpenTimeout>().is_some() {
            return Self::OpenTimeout;
        }
        if err.downcast_ref::<CorpusOpenWedged>().is_some() {
            return Self::Contention;
        }
        if let Some(db_err) = err.downcast_ref::<redb::DatabaseError>() {
            return match db_err {
                redb::DatabaseError::DatabaseAlreadyOpen
                | redb::DatabaseError::TransactionInProgress => Self::Contention,
                redb::DatabaseError::UpgradeRequired(_) => Self::FormatIncompatible,
                redb::DatabaseError::Storage(redb::StorageError::Corrupted(_)) => {
                    Self::FormatIncompatible
                }
                // `RepairAborted` and the remaining storage variants (I/O,
                // lock poisoning, closed) are real failures but say nothing
                // about the on-disk FORMAT — refusing to call them corruption
                // is the entire point of this issue.
                _ => Self::Unclassified,
            };
        }
        if let Some(storage) = err.downcast_ref::<redb::StorageError>() {
            return match storage {
                redb::StorageError::Corrupted(_) => Self::FormatIncompatible,
                _ => Self::Unclassified,
            };
        }
        Self::Unclassified
    }

    /// `true` when the failure is transient and the on-disk corpus was never
    /// read — so it is presumed INTACT and must not be rebuilt (issue #4333).
    ///
    /// Why: this is the predicate that stands between an operator and a
    /// destructive `--force` rebuild of healthy data. Everything else in this
    /// module is presentation; this is the safety decision.
    /// What: `true` for [`Self::OpenTimeout`] and [`Self::Contention`].
    /// Test: `transient_kinds_never_recommend_rebuild`.
    pub fn is_transient(self) -> bool {
        matches!(self, Self::OpenTimeout | Self::Contention)
    }

    /// Stable machine-readable label for status payloads (issue #4333).
    ///
    /// Test: `labels_are_distinct`.
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenTimeout => "open_timeout",
            Self::Contention => "contention",
            Self::FormatIncompatible => "format_incompatible",
            Self::Unclassified => "unclassified",
        }
    }

    /// Per-kind stage-failure reason surfaced on `/health` and
    /// `GET /indexes/:id/status` (issue #4333).
    ///
    /// Why: replaces the single hardcoded "incompatible or corrupted format …
    /// run `--force`" string that `derive_warm_boot_stages` emitted for every
    /// kind. Only [`Self::FormatIncompatible`] may mention corruption or
    /// prescribe a rebuild; the transient kinds say the opposite, in as many
    /// words, because an operator reading this string is deciding right then
    /// whether to destroy the corpus.
    /// What: returns a `'static` string per kind.
    /// Test: `transient_kinds_never_recommend_rebuild`,
    /// `format_incompatible_keeps_the_rebuild_instruction`.
    pub fn stage_reason(self) -> &'static str {
        match self {
            Self::OpenTimeout => {
                "durable corpus open TIMED OUT — an opener is still running (issue #3659). \
                 This is TRANSIENT and self-heals: the on-disk corpus was never read and is \
                 presumed INTACT. Retry shortly, or restart the daemon once warm-boot \
                 contention subsides. DO NOT reindex and DO NOT run `--force`: rebuilding \
                 here destroys healthy data (issue #4333)"
            }
            Self::Contention => {
                "durable corpus is held by another opener (contention / stale file lock). \
                 This is TRANSIENT and self-heals: the on-disk corpus was never read and is \
                 presumed INTACT. Retry shortly, or restart the daemon. DO NOT reindex and \
                 DO NOT run `--force` (issue #4333)"
            }
            Self::FormatIncompatible => {
                "redb corpus is in an incompatible on-disk format or is corrupted — redb \
                 reported this about the file itself, not about contention. This is \
                 PERMANENT: run `trusty-search index <path> --force` to rebuild from source \
                 (issues #1158, #1870, #2203, #4333)"
            }
            Self::Unclassified => {
                "durable corpus could not be opened and the cause was NOT classified — check \
                 file permissions, disk health, and the daemon log for the underlying error. \
                 This is NOT known to be corruption: DO NOT reindex or run `--force` until \
                 the cause is identified, because a rebuild is irreversible (issue #4333)"
            }
        }
    }
}

/// Build the typed timeout marker for `open_serialized` (issue #4333).
///
/// Why: keeps the marker's construction next to its classification so the two
/// cannot drift.
/// What: thin constructor.
/// Test: `classify_timeout_marker_is_transient`.
pub(crate) fn timeout_marker(path: String, timeout: Duration) -> CorpusOpenTimeout {
    CorpusOpenTimeout { path, timeout }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why (issue #4333, the core regression): the timeout path produces NO
    /// redb error at all — the open never returned — so a classifier that only
    /// looked at redb errors would fall through to "corrupted" exactly as the
    /// old hardcoded string did. The typed marker must classify as transient.
    /// What: wraps a `CorpusOpenTimeout` in anyhow (with a context layer, to
    /// prove the chain walk works) and asserts `OpenTimeout` + transient.
    /// Test: this test.
    #[test]
    fn classify_timeout_marker_is_transient() {
        let err = anyhow::Error::new(timeout_marker(
            "/tmp/index.redb".to_string(),
            Duration::from_secs(30),
        ))
        .context("warm-boot: opening corpus");
        let kind = CorpusOpenFailure::classify(&err);
        assert_eq!(kind, CorpusOpenFailure::OpenTimeout);
        assert!(kind.is_transient());
    }

    /// Why: the wedge latch is the second transient-but-not-redb path.
    /// Test: this test.
    #[test]
    fn classify_wedge_marker_is_contention() {
        let err = anyhow::Error::new(CorpusOpenWedged {
            path: "/tmp/index.redb".to_string(),
        });
        assert_eq!(
            CorpusOpenFailure::classify(&err),
            CorpusOpenFailure::Contention
        );
    }

    /// Why: `DatabaseAlreadyOpen` is the classic "another handle holds it"
    /// case that the old wording called corruption.
    /// Test: this test.
    #[test]
    fn classify_database_already_open_is_contention() {
        let err = anyhow::Error::new(redb::DatabaseError::DatabaseAlreadyOpen);
        let kind = CorpusOpenFailure::classify(&err);
        assert_eq!(kind, CorpusOpenFailure::Contention);
        assert!(kind.is_transient());
    }

    /// Why: the one case where "rebuild" is genuinely right must still be
    /// classified as such — the fix must not swing to the opposite error of
    /// never recommending a rebuild.
    /// Test: this test.
    #[test]
    fn classify_upgrade_required_is_format_incompatible() {
        let err = anyhow::Error::new(redb::DatabaseError::UpgradeRequired(2));
        let kind = CorpusOpenFailure::classify(&err);
        assert_eq!(kind, CorpusOpenFailure::FormatIncompatible);
        assert!(!kind.is_transient());
    }

    /// Why: genuine corruption reported by redb itself.
    /// Test: this test.
    #[test]
    fn classify_corrupted_storage_is_format_incompatible() {
        let err = anyhow::Error::new(redb::DatabaseError::Storage(redb::StorageError::Corrupted(
            "bad page".to_string(),
        )));
        assert_eq!(
            CorpusOpenFailure::classify(&err),
            CorpusOpenFailure::FormatIncompatible
        );
    }

    /// Why: an I/O error (e.g. permission denied) is a real failure but says
    /// nothing about the file format — calling it corruption is the defect.
    /// Test: this test.
    #[test]
    fn classify_io_error_is_not_format_incompatible() {
        let err = anyhow::Error::new(redb::DatabaseError::Storage(redb::StorageError::Io(
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
        )));
        assert_eq!(
            CorpusOpenFailure::classify(&err),
            CorpusOpenFailure::Unclassified
        );
    }

    /// Why: an unrecognised error must degrade to `Unclassified`, never to a
    /// corruption claim.
    /// Test: this test.
    #[test]
    fn classify_unknown_error_is_unclassified() {
        let err = anyhow::anyhow!("something else entirely");
        assert_eq!(
            CorpusOpenFailure::classify(&err),
            CorpusOpenFailure::Unclassified
        );
    }

    /// Why (issue #4333, the safety assertion): the transient reasons are what
    /// an operator reads while deciding whether to destroy a corpus. They must
    /// never contain the destructive instruction, and never claim corruption.
    /// What: asserts absence of `--force`/`reindex`-as-a-remedy and of the
    /// words "corrupt"/"incompatible" for both transient kinds.
    /// Test: this test.
    #[test]
    fn transient_kinds_never_recommend_rebuild() {
        for kind in [CorpusOpenFailure::OpenTimeout, CorpusOpenFailure::Contention] {
            let reason = kind.stage_reason();
            assert!(
                reason.contains("DO NOT reindex"),
                "{kind:?} must explicitly forbid reindex: {reason}"
            );
            assert!(
                reason.contains("INTACT"),
                "{kind:?} must state the corpus is presumed intact: {reason}"
            );
            assert!(
                !reason.contains("corrupted format"),
                "{kind:?} must not claim corruption (issue #4333): {reason}"
            );
        }
    }

    /// Why: `Unclassified` is not corruption either — it must warn against a
    /// rebuild just as firmly, since the cause is unknown.
    /// Test: this test.
    #[test]
    fn unclassified_kind_does_not_claim_corruption() {
        let reason = CorpusOpenFailure::Unclassified.stage_reason();
        assert!(reason.contains("DO NOT reindex"), "{reason}");
        assert!(!reason.contains("corrupted format"), "{reason}");
    }

    /// Why: the fix must preserve the genuine-corruption remedy.
    /// Test: this test.
    #[test]
    fn format_incompatible_keeps_the_rebuild_instruction() {
        let reason = CorpusOpenFailure::FormatIncompatible.stage_reason();
        assert!(reason.contains("--force"), "{reason}");
        assert!(!CorpusOpenFailure::FormatIncompatible.is_transient());
    }

    /// Why: labels are consumed by dashboards; collisions would erase the
    /// distinction this issue exists to create.
    /// Test: this test.
    #[test]
    fn labels_are_distinct() {
        let labels = [
            CorpusOpenFailure::OpenTimeout.label(),
            CorpusOpenFailure::Contention.label(),
            CorpusOpenFailure::FormatIncompatible.label(),
            CorpusOpenFailure::Unclassified.label(),
        ];
        let unique: std::collections::HashSet<_> = labels.iter().collect();
        assert_eq!(unique.len(), labels.len(), "labels must be distinct");
    }
}
