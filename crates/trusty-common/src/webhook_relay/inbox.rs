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

/// Distinguishes two temp files written in the same nanosecond by this process.
///
/// See [`Inbox::write_temp`] for why a collision here refuses a valid delivery.
static TEMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The temp name one `write_temp` attempt uses, given the clock reading it took.
///
/// Why: `stamp_nanos` is a parameter rather than read in here so a test can hold
/// the clock still. The counter only earns its place when two names are drawn
/// inside a single tick, so a test that lets the clock run proves nothing on a
/// platform fine-grained enough to separate the two by stamp alone — and the
/// platform that gates merges is not the one where this defect showed up.
/// What: `<final path>.json.<pid>.<stamp>.<seq>.tmp`, with `seq` from
/// [`TEMP_SEQ`].
/// Test: `two_temp_names_within_one_clock_tick_differ`.
fn temp_path_for(final_path: &Path, stamp_nanos: u128) -> PathBuf {
    let seq = TEMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    final_path.with_extension(format!(
        "json.{}.{stamp_nanos}.{seq}.tmp",
        std::process::id()
    ))
}

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

    /// A different delivery already occupies this delivery's path.
    ///
    /// Acking here would discard the incoming delivery and license the sender to
    /// delete its only copy of it, on nothing more than a filename match.
    /// Refusing keeps the sender's copy and surfaces the collision.
    #[error("delivery {delivery_id} collides with the entry already at {path}: {detail}")]
    KeyCollision {
        /// Path that is already occupied.
        path: PathBuf,
        /// The delivery that could not be stored.
        delivery_id: String,
        /// What the held copy turned out to be.
        detail: String,
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
    /// Deliberately narrower than [`crate::uds::prepare_socket_dir`]: it chmods
    /// an existing directory without checking ownership or refusing a symlink.
    /// The inbox lives under the user's own data directory, so reaching it
    /// already requires having compromised that directory. Point this somewhere
    /// less private and those checks become load-bearing.
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
    /// What: `<sanitised id>-<sha256 of the raw id, first 16 hex>.json`. The id
    /// arrives from an attacker-influenced header (the HMAC covers the body, not
    /// the headers), so it is reduced to `[A-Za-z0-9_-]` and truncated before it
    /// reaches `Path::join`; the digest of the RAW id restores the uniqueness
    /// sanitising throws away.
    ///
    /// The digest narrows collisions, it does not eliminate them, and the ack
    /// does not rest on it: [`Inbox::take_ownership`] reads the held copy back
    /// and refuses when it is a different delivery. A truncated SHA-256 rather
    /// than a non-cryptographic hash because a signature-capable sender could
    /// otherwise choose two ids that land on one path (`sha2` is already a
    /// dependency — `webhook-relay` implies `webhook-hmac`).
    /// Test: `inbox_entry_path_sanitises_a_hostile_delivery_id`,
    /// `inbox_entry_path_separates_ids_that_sanitise_alike`.
    pub fn entry_path(&self, delivery_id: &str) -> PathBuf {
        self.root.join(format!(
            "{}-{}.json",
            sanitise_delivery_id(delivery_id),
            id_digest(delivery_id)
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
    ///
    /// An `EEXIST` on the link means something is already at this path. That is
    /// an ack ONLY once the held copy has been read back and confirmed to be the
    /// same delivery — see [`InboxError::KeyCollision`]. Acking on the bare
    /// `EEXIST` would discard the incoming delivery and license the sender to
    /// delete its only copy of it, on nothing more than a filename match.
    ///
    /// # Errors
    ///
    /// Any [`InboxError`]. Each one means the delivery is not durably held.
    ///
    /// Test: `inbox_persist_is_durable_before_it_returns`,
    /// `inbox_redelivery_of_a_held_id_is_already_owned`,
    /// `inbox_refuses_a_different_delivery_on_the_same_path`,
    /// `inbox_persist_fails_when_the_root_is_not_a_directory`.
    pub fn take_ownership(&self, delivery: &RelayDelivery) -> Result<Ownership, InboxError> {
        let final_path = self.entry_path(&delivery.delivery_id);
        let tmp_path = self.write_temp(delivery, &final_path)?;

        let already_owned = match std::fs::hard_link(&tmp_path, &final_path) {
            Ok(()) => false,
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                // #5182 review: a filename match is not a delivery match. Two
                // distinct ids can land here — console's own fallback id
                // `no-delivery-header-<ms>` is not unique within a millisecond,
                // and a digest is not a proof of injectivity.
                if let Err(e) = self.confirm_same_delivery(&final_path, delivery) {
                    let _ = std::fs::remove_file(&tmp_path);
                    return Err(e);
                }
                true
            }
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

    /// Confirm the copy already at `path` is the same delivery as `incoming`.
    ///
    /// Why: [`Inbox::take_ownership`] treats `EEXIST` as "already held", and
    /// that answer licenses the sender to delete its only copy. If the held file
    /// is a DIFFERENT delivery that merely shares a path, acking discards the
    /// incoming one permanently. Refusing instead leaves the sender's copy alive
    /// and turns a silent loss into a visible, retried failure.
    /// What: reads the held entry and compares the delivery id and the body. A
    /// file that cannot be read or decoded is treated as a mismatch — an
    /// unreadable held copy is not evidence that the work is owned.
    /// Test: `inbox_refuses_a_different_delivery_on_the_same_path`,
    /// `inbox_refuses_when_the_held_copy_is_unreadable`.
    fn confirm_same_delivery(
        &self,
        path: &Path,
        incoming: &RelayDelivery,
    ) -> Result<(), InboxError> {
        let collision = |detail: &str| InboxError::KeyCollision {
            path: path.to_path_buf(),
            delivery_id: incoming.delivery_id.clone(),
            detail: detail.to_string(),
        };
        let bytes =
            std::fs::read(path).map_err(|e| collision(&format!("held copy unreadable: {e}")))?;
        let held: RelayDelivery = serde_json::from_slice(&bytes)
            .map_err(|e| collision(&format!("held copy undecodable: {e}")))?;
        if held.delivery_id != incoming.delivery_id {
            return Err(collision(&format!(
                "path is held by delivery {:?}",
                held.delivery_id
            )));
        }
        if held.body_b64 != incoming.body_b64 {
            return Err(collision(
                "same delivery id, different body — the sender re-used an id",
            ));
        }
        Ok(())
    }

    /// Encode `delivery` into a fresh, fsync'd temp file beside `final_path`.
    ///
    /// The temp name carries the process id, a nanosecond stamp and a
    /// process-wide counter so two concurrent writers for the same delivery
    /// cannot share one, and uses `create_new` so a leftover from a crashed run
    /// is never appended to.
    ///
    /// 🔴 The counter is not belt-and-braces. Two deliveries handled in the same
    /// nanosecond collide on the pid+stamp name alone, `create_new` fails with
    /// `EEXIST`, and the second one is REFUSED — a correct delivery reported as
    /// undurable purely because a sibling was concurrent. That went unseen while
    /// connections were served inline and appeared the moment they were spawned.
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
        let tmp_path = temp_path_for(final_path, stamp);

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

/// How many deliveries sit at `root`, without opening (or creating) an inbox.
///
/// Why: `trusty-console` has to meter a directory that belongs to another
/// service, and it must not create or chmod it as a side effect of asking. A
/// held delivery is work that arrived and is not finished, so this count is what
/// stands between an operator and an undrained backlog reported as healthy —
/// see [`crate::webhook_relay::inbox_root_for`] and #5192.
/// What: counts `*.json` entries. An absent directory is `0`, not an error:
/// nothing has ever been delivered to that service.
/// Test: `held_count_reports_zero_for_an_absent_inbox`,
/// `held_count_counts_stored_deliveries`.
pub fn held_count(root: &Path) -> Result<usize, InboxError> {
    let dir = match std::fs::read_dir(root) {
        Ok(dir) => dir,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(source) => {
            return Err(InboxError::Read {
                path: root.to_path_buf(),
                source,
            });
        }
    };
    Ok(dir
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .count())
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

/// First 8 bytes of SHA-256 over the raw delivery id, as hex.
///
/// Why: [`Inbox::entry_path`] needs a digest that is identical across processes
/// and releases, because the path is the deduplication key — `DefaultHasher` is
/// explicitly not stable across releases and would silently start writing a
/// second copy of a delivery already held. SHA-256 rather than a
/// non-cryptographic hash so a signature-capable sender cannot choose two ids
/// that land on one path; truncation keeps the filename short, and the
/// read-back in [`Inbox::confirm_same_delivery`] is what actually decides
/// whether a shared path is the same delivery.
fn id_digest(delivery_id: &str) -> String {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(delivery_id.as_bytes());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod temp_path_tests {
    use super::*;

    #[test]
    fn two_temp_names_within_one_clock_tick_differ() {
        // Two handlers for one delivery id, both reading the same clock value.
        // Without the counter they build the same name, `create_new` fails
        // EEXIST, and the second delivery is refused as undurable with nothing
        // wrong with it. `serve_concurrent_deliveries_of_one_id_produce_one_
        // stored_copy` catches that too, but only when the clock actually
        // collides — reliably on macOS, unverified on the Linux that gates
        // merges. Freezing the stamp is what makes the proof platform-neutral.
        let final_path = Path::new("/inbox/delivery-1.json");
        let frozen_stamp = 1_700_000_000_000_000_000u128;

        let first = temp_path_for(final_path, frozen_stamp);
        let second = temp_path_for(final_path, frozen_stamp);

        assert_ne!(
            first, second,
            "two writers in one tick must not share a temp name"
        );
    }
}
