//! The receiver's durable owner of a relayed delivery (#5182, ADR-0034 §2).
//!
//! Why: `RelayOutcome::Acked` is the sole state that lets `trusty-console`
//! delete a spool entry, and the spool entry is the only copy — GitHub never
//! re-sends a delivery it has seen acknowledged. So an ack sent before the
//! receiver's own copy is on disk moves the silent-loss defect ADR-0034 exists
//! to remove one hop further along, this time with the sender actively deleting
//! the evidence. This module exists so "the work is durably owned" is a
//! filesystem fact the ack can be conditioned on, not a hope about what the
//! process will do next.
//!
//! What: [`Inbox`] writes one JSON file per delivery, fsync'ing the bytes and
//! then the directory entry, and refuses to clobber. Redelivery of an id
//! already on disk is [`Ownership::already_owned`] — still an ack, because the
//! relay is at-least-once by construction (see [`super::RelayParams::attempts`])
//! and re-acking a delivery we already hold is correct, while refusing it would
//! wedge the sender's spool forever.
//!
//! The write sequence is deliberately the same one `trusty-console`'s spool
//! uses: temp file with `create_new` → `sync_all` → `hard_link` into place →
//! unlink the temp → `sync_all` the directory. `hard_link` rather than `rename`
//! because rename silently replaces the destination while link fails with
//! `EEXIST`, which is the atomic already-present answer this needs.
//!
//! Test: `tests.rs` — `inbox_*` cases.

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use super::RelayDelivery;

/// Mode the inbox directory is held at: owner-only, like every other socket and
/// spool directory on this path.
pub const INBOX_DIR_MODE: u32 = 0o700;

/// Mode each stored delivery is written at.
///
/// A delivery holds the raw webhook body and its original headers, so it is not
/// a file another local user has any business reading.
pub const INBOX_FILE_MODE: u32 = 0o600;

/// Everything that can stop a delivery from becoming durable.
///
/// Every variant means the receiver does NOT own the work and MUST NOT
/// acknowledge — that is the whole contract of this type, and why none of them
/// is a "log and continue" condition.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum InboxError {
    /// The inbox directory could not be created or narrowed to
    /// [`INBOX_DIR_MODE`].
    #[error("prepare webhook inbox directory {path}: {source}")]
    PrepareDir {
        /// Directory that could not be prepared.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// The delivery could not be encoded.
    #[error("encode delivery {delivery_id}: {source}")]
    Encode {
        /// Delivery that could not be encoded.
        delivery_id: String,
        /// Underlying serde error.
        #[source]
        source: serde_json::Error,
    },

    /// Writing or fsync'ing the temp file failed.
    #[error("write webhook inbox entry {path}: {source}")]
    Write {
        /// Path that could not be written.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// Linking the temp file under its real name failed.
    #[error("commit webhook inbox entry {from} -> {to}: {source}")]
    Commit {
        /// Temp path the entry was staged at.
        from: PathBuf,
        /// Final path it could not be linked to.
        to: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// The directory entry could not be made durable.
    ///
    /// Distinct from [`InboxError::Write`] on purpose: the bytes reached the
    /// platter but the NAME did not, so a crash can leave the entry unreachable
    /// even though its data survived. Acking on the strength of the file write
    /// alone would lose exactly that delivery.
    #[error("fsync webhook inbox directory {path}: {source}")]
    SyncDir {
        /// Directory that could not be fsync'd.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// The inbox could not be read back.
    #[error("read webhook inbox {path}: {source}")]
    Read {
        /// Path that could not be read.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },
}

/// A receipt proving one delivery is durably held.
///
/// Why: the value a listener must hold before it is allowed to answer
/// [`super::RelayResponse::ack`]. Making it a return value rather than a
/// `()` means the ack path names the thing that licenses it.
/// What: where the delivery landed, and whether this call is what put it there.
/// Test: `inbox_persist_is_durable_before_it_returns`,
/// `inbox_redelivery_of_a_held_id_is_already_owned`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ownership {
    /// File now holding the delivery.
    pub path: PathBuf,
    /// True when the delivery was already on disk from an earlier attempt whose
    /// ack the sender did not see.
    pub already_owned: bool,
}

/// A directory of deliveries a receiver has taken responsibility for.
///
/// Why: see the module docs — this is what makes the ack checkable.
/// What: cheap to clone (one `PathBuf`), so a listener can hand a copy to each
/// blocking write without sharing a lock. All I/O is blocking and must be run
/// off an async runtime worker.
/// Test: `tests.rs` — `inbox_*` cases.
#[derive(Debug, Clone)]
pub struct Inbox {
    root: PathBuf,
}

impl Inbox {
    /// Open (creating) an inbox at `root`, held at [`INBOX_DIR_MODE`].
    ///
    /// # Errors
    ///
    /// [`InboxError::PrepareDir`] when the directory cannot be created or
    /// narrowed. Called at startup so a misconfigured data directory fails
    /// before the first delivery rather than during one.
    ///
    /// Test: `inbox_open_creates_an_owner_only_directory`.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, InboxError> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|source| InboxError::PrepareDir {
            path: root.clone(),
            source,
        })?;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(INBOX_DIR_MODE)).map_err(
            |source| InboxError::PrepareDir {
                path: root.clone(),
                source,
            },
        )?;
        Ok(Self { root })
    }

    /// Directory this inbox writes into.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path a delivery occupies, derived only from its id.
    ///
    /// Why: the path must be a pure function of the delivery id and nothing
    /// else, because that is what makes a redelivery collide with the copy
    /// already held instead of writing a second one. A receipt timestamp in the
    /// name — which the sender's spool does use, for age-sorting — would defeat
    /// deduplication entirely.
    /// What: `<sanitised id>-<fnv1a of the raw id>.json`. The id arrives from an
    /// attacker-influenced header (the HMAC covers the body, not the headers),
    /// so it is reduced to `[A-Za-z0-9_-]` and truncated before it reaches
    /// `Path::join`; the hash of the RAW id restores the uniqueness that
    /// sanitising throws away, so two hostile ids cannot alias onto one file.
    /// Test: `inbox_entry_path_sanitises_a_hostile_delivery_id`,
    /// `inbox_entry_path_separates_ids_that_sanitise_alike`.
    pub fn entry_path(&self, delivery_id: &str) -> PathBuf {
        self.root.join(format!(
            "{}-{:016x}.json",
            sanitise_delivery_id(delivery_id),
            fnv1a64(delivery_id.as_bytes())
        ))
    }

    /// Take durable ownership of `delivery`.
    ///
    /// 🔴 This returning `Ok` is the ONLY thing that licenses a receiver to
    /// answer [`super::RelayResponse::ack`]. On any `Err` the caller must
    /// refuse, so the sender keeps its spool entry and retries.
    ///
    /// What: encode → write a temp file with `create_new` → `sync_all` it →
    /// `hard_link` it into place → unlink the temp → `sync_all` the directory.
    /// An `EEXIST` on the link is not a failure: the delivery is already held,
    /// which is [`Ownership::already_owned`] and still an ack.
    ///
    /// # Errors
    ///
    /// Any [`InboxError`]. Each one means the delivery is not durably held.
    ///
    /// Test: `inbox_persist_is_durable_before_it_returns`,
    /// `inbox_redelivery_of_a_held_id_is_already_owned`,
    /// `inbox_persist_fails_when_the_root_is_not_a_directory`.
    pub fn take_ownership(&self, delivery: &RelayDelivery) -> Result<Ownership, InboxError> {
        let final_path = self.entry_path(&delivery.delivery_id);
        let tmp_path = self.write_temp(delivery, &final_path)?;

        let already_owned = match std::fs::hard_link(&tmp_path, &final_path) {
            Ok(()) => false,
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => true,
            Err(source) => {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(InboxError::Commit {
                    from: tmp_path,
                    to: final_path,
                    source,
                });
            }
        };
        // The temp name is redundant once the entry is linked under its real
        // one; the inode survives until the last link goes.
        let _ = std::fs::remove_file(&tmp_path);

        // The file fsync above made the bytes durable; this makes the NAME
        // durable. Skipping it would let a crash strand a delivery whose data
        // reached the platter under a directory entry that did not.
        sync_dir(&self.root)?;

        Ok(Ownership {
            path: final_path,
            already_owned,
        })
    }

    /// Every delivery currently held, oldest-first by receipt time.
    ///
    /// Why: what a drain step reads, and what a test asserts the ack against.
    /// What: skips any file that is not decodable rather than failing the whole
    /// listing — one corrupt entry must not hide the rest.
    ///
    /// # Errors
    ///
    /// [`InboxError::Read`] when the directory itself cannot be listed.
    ///
    /// Test: `inbox_lists_what_it_holds`.
    pub fn list(&self) -> Result<Vec<(PathBuf, RelayDelivery)>, InboxError> {
        let dir = std::fs::read_dir(&self.root).map_err(|source| InboxError::Read {
            path: self.root.clone(),
            source,
        })?;
        let mut held: Vec<(PathBuf, RelayDelivery)> = dir
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .filter_map(|p| {
                let bytes = std::fs::read(&p).ok()?;
                let delivery: RelayDelivery = serde_json::from_slice(&bytes).ok()?;
                Some((p, delivery))
            })
            .collect();
        held.sort_by_key(|(_, d)| d.received_at_unix_ms);
        Ok(held)
    }

    /// Encode `delivery` into a fresh, fsync'd temp file beside `final_path`.
    ///
    /// The temp name carries the process id and a nanosecond stamp so two
    /// concurrent writers for the same delivery cannot share one, and uses
    /// `create_new` so a leftover from a crashed run is never appended to.
    fn write_temp(
        &self,
        delivery: &RelayDelivery,
        final_path: &Path,
    ) -> Result<PathBuf, InboxError> {
        let bytes = serde_json::to_vec_pretty(delivery).map_err(|source| InboxError::Encode {
            delivery_id: delivery.delivery_id.clone(),
            source,
        })?;

        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp_path =
            final_path.with_extension(format!("json.{}.{stamp}.tmp", std::process::id()));

        let write = || -> std::io::Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp_path)?;
            file.set_permissions(std::fs::Permissions::from_mode(INBOX_FILE_MODE))?;
            file.write_all(&bytes)?;
            file.sync_all()
        };
        write().map_err(|source| InboxError::Write {
            path: tmp_path.clone(),
            source,
        })?;
        Ok(tmp_path)
    }
}

/// fsync a directory so a name created inside it survives a crash.
fn sync_dir(path: &Path) -> Result<(), InboxError> {
    std::fs::File::open(path)
        .and_then(|d| d.sync_all())
        .map_err(|source| InboxError::SyncDir {
            path: path.to_path_buf(),
            source,
        })
}

/// Reduce an attacker-influenced delivery id to a safe filename component.
///
/// The `X-GitHub-Delivery` header is outside the HMAC, so `../` or a 4 KiB
/// value must not reach `Path::join`.
fn sanitise_delivery_id(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    if cleaned.is_empty() {
        "unknown".to_string()
    } else {
        cleaned
    }
}

/// FNV-1a 64, spelled out rather than pulled from a dependency.
///
/// Why: [`Inbox::entry_path`] needs a hash that is identical across processes
/// and releases, because the path IS the deduplication key — `DefaultHasher` is
/// explicitly not stable across releases and would silently start writing a
/// second copy of a delivery already held. This is not a security hash and is
/// not used as one; the sanitised prefix plus the raw-id hash only have to make
/// two distinct ids land on two distinct files.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
