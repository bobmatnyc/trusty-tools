//! An exclusive, crash-releasing claim on one inbox entry (#5192, ADR-0034 §5).
//!
//! Why: the drain has to answer two questions that look alike and are not. "Is
//! another drainer already working this entry?" must be answered without
//! double-processing it, and "did the drainer that was working this entry
//! die?" must be answered without stranding it. A claim marker written to disk
//! answers the first and gets the second wrong forever — a crash leaves the
//! marker behind and the delivery is never picked up again, which is the silent
//! loss ADR-0034 exists to remove, arriving one hop later.
//!
//! What: `flock(LOCK_EX | LOCK_NB)` on the entry file itself. The kernel
//! releases it when the holding fd closes, which includes the process being
//! SIGKILLed — so "crashed mid-processing" and "finished" are the same state to
//! everyone else, and that state is *claimable*. A busy entry answers
//! `EWOULDBLOCK` immediately rather than blocking, so one slow review never
//! stalls the rest of the drain.
//!
//! 🔴 The `nlink` check after the lock is not defensive padding. Two drainers
//! can both `open` the same path; the first locks, processes, unlinks and
//! releases; the second then acquires the lock on an inode with no name and
//! would process a delivery that has already been handled. `st_nlink == 0` is
//! how a held fd learns its file is gone, and it is what makes the claim a
//! once-only claim rather than a mutual-exclusion window.
//!
//! Test: `tests.rs` — `claim_*`.

use std::fs::File;
use std::io::Read as _;
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use super::RelayDelivery;

/// Everything that can stop a claim attempt from producing an answer.
///
/// Deliberately small: "someone else holds it", "it is already gone" and "its
/// contents are not a delivery" are all valid ANSWERS ([`ClaimOutcome`]), not
/// errors. Only a failure that leaves the drain unable to say anything about
/// the entry is an error.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ClaimError {
    /// The entry could not be opened, for a reason other than being absent.
    #[error("open inbox entry {path}: {source}")]
    Open {
        /// Entry that could not be opened.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// `flock` failed for a reason other than the lock being held.
    #[error("lock inbox entry {path}: {source}")]
    Lock {
        /// Entry that could not be locked.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// The held fd could not be stat'd, so "still linked" is unknowable.
    ///
    /// Treated as an error rather than assumed either way: proceeding would
    /// risk a double-process, and skipping would risk stranding the entry
    /// silently. The drain reports it and the entry stays claimable.
    #[error("stat inbox entry {path}: {source}")]
    Stat {
        /// Entry that could not be stat'd.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// The claimed entry could not be read back through the held fd.
    #[error("read inbox entry {path}: {source}")]
    Read {
        /// Entry that could not be read.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },
}

/// What one [`Claim::try_acquire`] found.
///
/// Why: three of these four are ordinary, expected states under concurrency and
/// after a crash, and the drain counts them differently. Collapsing them into
/// `Option<Claim>` is what makes a report say `skipped: 3` with no way to tell
/// a busy entry from a poisoned one.
/// Test: `claim_is_exclusive_between_two_holders`,
/// `claim_of_a_vanished_entry_reports_vanished`,
/// `claim_of_an_undecodable_entry_reports_undecodable`.
#[derive(Debug)]
pub enum ClaimOutcome {
    /// The entry is ours until the returned [`Claim`] drops.
    Claimed(Claim),
    /// Another drainer holds it. Not a failure — try again next pass.
    InFlight,
    /// It was drained (or never existed) before we got the lock.
    Vanished,
    /// It is on disk but is not a decodable delivery, so no processor can ever
    /// accept it. The drain quarantines these rather than retrying forever.
    Undecodable {
        /// Entry that could not be decoded.
        path: PathBuf,
        /// What went wrong, verbatim.
        reason: String,
    },
}

/// Exclusive ownership of one inbox entry, for as long as this value lives.
///
/// Why: the drain's whole safety argument is "the entry is removed only after
/// the pipeline accepted it, and until then exactly one drainer may touch it".
/// This type is the second half of that sentence, and its `Drop` is what makes
/// a panic or a SIGKILL indistinguishable from a clean release.
/// What: the decoded delivery plus the `flock`-holding fd. Not `Clone`, not
/// `Send`-hostile — a `File` is `Send`, so a claim may be held across an await.
/// Test: `claim_is_exclusive_between_two_holders`,
/// `claim_is_released_when_the_holder_is_dropped`.
#[derive(Debug)]
pub struct Claim {
    path: PathBuf,
    delivery: RelayDelivery,
    /// Holds the advisory lock. Closing this fd — including via process death —
    /// is what releases it.
    _held: File,
}

impl Claim {
    /// Try to take exclusive ownership of the entry at `path`.
    ///
    /// Never blocks: a contended entry answers [`ClaimOutcome::InFlight`] at
    /// once, so a 37-second review in one drainer does not stall another's
    /// whole pass.
    ///
    /// # Errors
    ///
    /// [`ClaimError`] when the entry can neither be claimed nor classified. The
    /// entry is left on disk and stays claimable in every such case.
    ///
    /// Test: `claim_is_exclusive_between_two_holders`,
    /// `claim_of_a_vanished_entry_reports_vanished`,
    /// `claim_of_an_undecodable_entry_reports_undecodable`,
    /// `claim_of_a_removed_entry_reports_vanished`. The `nlink` branch itself
    /// is proven through [`entry_is_still_linked`].
    pub fn try_acquire(path: &Path) -> Result<ClaimOutcome, ClaimError> {
        let mut file = match File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ClaimOutcome::Vanished);
            }
            Err(source) => {
                return Err(ClaimError::Open {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };

        // SAFETY: `file` owns the descriptor for the whole call and outlives it
        // inside the returned `Claim`; `flock` mutates only kernel lock state.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            let source = std::io::Error::last_os_error();
            return match source.raw_os_error() {
                Some(libc::EWOULDBLOCK) => Ok(ClaimOutcome::InFlight),
                _ => Err(ClaimError::Lock {
                    path: path.to_path_buf(),
                    source,
                }),
            };
        }

        // 🔴 See the module docs: the winner of a contended lock may be holding
        // an inode the previous winner already unlinked.
        if !entry_is_still_linked(&file).map_err(|source| ClaimError::Stat {
            path: path.to_path_buf(),
            source,
        })? {
            return Ok(ClaimOutcome::Vanished);
        }
        let meta = file.metadata().map_err(|source| ClaimError::Stat {
            path: path.to_path_buf(),
            source,
        })?;

        // Read through the held fd, not the path: by now the path could name a
        // different inode, and the one we locked is the one we own.
        let mut bytes = Vec::with_capacity(meta.len() as usize);
        file.read_to_end(&mut bytes)
            .map_err(|source| ClaimError::Read {
                path: path.to_path_buf(),
                source,
            })?;

        match serde_json::from_slice::<RelayDelivery>(&bytes) {
            Ok(delivery) => Ok(ClaimOutcome::Claimed(Claim {
                path: path.to_path_buf(),
                delivery,
                _held: file,
            })),
            Err(e) => Ok(ClaimOutcome::Undecodable {
                path: path.to_path_buf(),
                reason: format!("{e}"),
            }),
        }
    }

    /// Entry this claim covers.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The delivery held at that entry.
    pub fn delivery(&self) -> &RelayDelivery {
        &self.delivery
    }
}

/// Does the file behind this fd still have a name?
///
/// Why: the TOCTOU check the whole once-only claim rests on. Two drainers can
/// both `open` one path; the first locks, processes, unlinks and releases, and
/// the second then wins a lock on an inode with no name. `st_nlink == 0` is how
/// the holder of an fd learns that happened.
///
/// It is a free function rather than three inline lines because the branch is
/// otherwise unreachable from a test — the unlink has to land inside
/// [`Claim::try_acquire`]'s own open-to-stat window, which no caller can
/// schedule. Calling the predicate directly against a deliberately unlinked fd
/// proves the same condition without pretending to reproduce the race.
///
/// # Errors
///
/// When the fd cannot be stat'd.
///
/// Test: `entry_is_still_linked_is_false_for_an_unlinked_fd`,
/// `entry_is_still_linked_is_true_for_a_live_entry`.
pub fn entry_is_still_linked(file: &File) -> std::io::Result<bool> {
    Ok(file.metadata()?.nlink() > 0)
}
