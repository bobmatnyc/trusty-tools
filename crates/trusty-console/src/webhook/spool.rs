//! The durable spool: a delivery is on disk and fsync'd before console
//! acknowledges GitHub.
//!
//! Why: both existing webhook handlers return `202` and *then* do the work, so
//! any failure after the ack loses the delivery permanently — GitHub does not
//! retry an acknowledged delivery (ADR-0034 Context, "The fail-open shape,
//! already shipped on this exact path"). Ordering the durable write before the
//! ack is what makes a lost delivery either recoverable from GitHub (never
//! acked) or visible in a health signal (acked and spooled). That ordering only
//! holds if the write is genuinely durable, hence the fsync of both the file
//! and its directory.
//!
//! What: one JSON file per delivery under
//! `resolve_data_dir("trusty-console")/webhook-spool/`. [`Spool::persist_new`]
//! writes a temp file, `fsync`s it, links it into place — refusing to clobber an
//! entry already there — then `fsync`s the directory so the new name survives a
//! crash. [`Spool::persist_update`] is the same sequence committing with
//! `rename`, for [`Spool::record_attempt`]'s deliberate overwrite.
//! [`Spool::remove_acked`] is the only deletion path and is reachable only from
//! an explicit target acknowledgement.
//!
//! 🔴 [`Spool::list_pending`] answers a missing directory with an error, not an
//! empty listing, for any spool that was successfully opened. An unreadable
//! spool reported as "nothing pending" makes the health scan green while every
//! delivery 500s.
//!
//! 🔴 Every fallible step here returns an error to the caller. Nothing in this
//! module logs-and-continues, because a swallowed spool failure is exactly the
//! defect the spool exists to remove.
//!
//! Test: `webhook/tests.rs` — `spool_*` cases cover the durable round trip,
//! attempt bumping, ack-only deletion, and both write-failure arms.

use std::collections::BTreeMap;
use std::fs::{File, Permissions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Mode the spool directory is held at.
///
/// A spooled delivery holds the raw webhook body, which is not public data.
/// Owner-only, matching the `0700` convention `trusty_common::uds` established
/// for the socket directory.
const SPOOL_DIR_MODE: u32 = 0o700;

/// Mode every spool entry file is written at.
const SPOOL_FILE_MODE: u32 = 0o600;

/// Bumped whenever [`SpoolEntry`]'s shape changes, so a future reader can tell
/// an entry it cannot interpret from one it can.
pub const SPOOL_SCHEMA_VERSION: u32 = 1;

/// Directory name under the console's data dir.
pub const SPOOL_DIR_NAME: &str = "webhook-spool";

/// Failures of the durable-write path. Every variant means the delivery is
/// **not** safely recorded, so every one of them must reach the HTTP caller as
/// a 5xx rather than a log line.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SpoolError {
    /// The spool directory could not be created or hardened.
    #[error("prepare spool directory {path}: {source}")]
    PrepareDir {
        /// Directory that could not be prepared.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// Serialising the entry failed.
    #[error("serialize spool entry {delivery_id}: {source}")]
    Encode {
        /// Delivery that could not be encoded.
        delivery_id: String,
        /// Underlying serde error.
        #[source]
        source: serde_json::Error,
    },

    /// Writing or fsyncing the temp file failed — disk full, permission, or a
    /// genuine fsync error.
    #[error("write spool entry to {path}: {source}")]
    Write {
        /// Temp path that could not be written.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// The atomic rename into place failed.
    #[error("commit spool entry {from} -> {to}: {source}")]
    Commit {
        /// Temp path.
        from: PathBuf,
        /// Final path.
        to: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// The directory fsync that makes the rename durable failed.
    #[error("fsync spool directory {path}: {source}")]
    SyncDir {
        /// Directory that could not be synced.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// An entry already exists at the path a fresh delivery derives.
    ///
    /// Defensive: GitHub always sends `X-GitHub-Delivery`, so two deliveries
    /// only collide when the header is absent AND they land in the same
    /// millisecond. Refusing is still the right answer — clobbering would
    /// destroy a delivery that has already been acknowledged.
    #[error("a spool entry already exists at {path}; refusing to clobber it")]
    AlreadyExists {
        /// Path that was already taken.
        path: PathBuf,
    },

    /// Listing the spool failed. Surfaced as a red health state rather than an
    /// empty (and therefore falsely healthy) listing.
    #[error("read spool directory {path}: {source}")]
    ReadDir {
        /// Directory that could not be read.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// Deleting an acknowledged entry failed.
    #[error("remove acknowledged spool entry {path}: {source}")]
    Remove {
        /// Entry that could not be removed.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },
}

/// What console proves to the target about a relayed body (ADR-0034 §3).
///
/// Re-exported from `trusty_common::webhook_relay` rather than defined here:
/// step 4's receivers live in `trusty-review` and `trusty-analyze`, which
/// cannot depend on the console, so the type has to sit where both halves read
/// it.
pub use trusty_common::webhook_relay::Provenance;

/// One spooled delivery.
///
/// Why: holds everything a target needs to act on the delivery and everything
/// an operator needs to diagnose a stuck one, so a pending entry is
/// self-describing without console being alive to explain it.
/// What: the raw body is base64 so the JSON container cannot corrupt bytes the
/// HMAC was computed over; the target decodes it and may re-verify
/// independently. `attempts` and `last_error` are the durable record of relay
/// failure that replaces the `tracing::warn!` both current handlers use.
/// Test: `spool_persists_and_reloads_an_entry_byte_exact`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpoolEntry {
    /// Schema version; see [`SPOOL_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// GitHub's `X-GitHub-Delivery` GUID, or a synthesised stand-in.
    pub delivery_id: String,
    /// Which target this delivery is bound for (`review` / `analyze`).
    pub source: String,
    /// The `X-GitHub-Event` value.
    pub event: String,
    /// The request headers, lowercased, as received.
    pub headers: BTreeMap<String, String>,
    /// The raw request body, base64-encoded — byte-exact, never re-serialised.
    pub body_b64: String,
    /// What console verified before spooling.
    pub provenance: Provenance,
    /// When console accepted the delivery.
    pub received_at_unix_ms: u64,
    /// How many relay attempts have failed. `0` on a freshly spooled entry.
    pub attempts: u32,
    /// Why the most recent attempt failed, if one has.
    pub last_error: Option<String>,
    /// When the most recent attempt ran.
    pub last_attempt_at_unix_ms: Option<u64>,
}

/// A pending entry paired with the path it lives at.
#[derive(Debug, Clone)]
pub struct PendingEntry {
    /// Absolute path of the entry file.
    pub path: PathBuf,
    /// The decoded entry.
    pub entry: SpoolEntry,
}

/// The on-disk spool rooted at one directory.
///
/// Why: `trusty_common::resolve_data_dir("trusty-console")` is already the
/// console's canonical state location (`lib.rs:476`), so the spool is a
/// subdirectory of it rather than a new location convention.
/// What: a path plus the durable write sequence. Cheap to clone.
/// Test: every `spool_*` case in `webhook/tests.rs`.
#[derive(Debug, Clone)]
pub struct Spool {
    root: PathBuf,
    /// Whether this spool's directory was successfully created at construction.
    ///
    /// 🔴 Load-bearing for the health signal, not bookkeeping. Once the
    /// directory has been created, its later absence means it was *removed* or
    /// its volume unmounted — a broken ingress, not an empty one. Without this
    /// flag `list_pending` cannot tell "never written" from "gone", and
    /// answering `ErrorKind::NotFound` with an empty listing makes
    /// `scan_health` report green while every `POST /api/webhooks/{source}`
    /// 500s. See [`Spool::list_pending`].
    opened: bool,
}

impl Spool {
    /// Bind a spool to `root` without touching the filesystem.
    ///
    /// Why: lets a caller construct the spool before deciding whether it can be
    /// created, and lets a test point one at a path that will fail on write.
    /// What: stores the path. No I/O, and the spool is NOT marked opened — an
    /// absent directory under this constructor is genuinely "never written".
    /// Test: `spool_persist_fails_when_the_root_is_not_a_directory`.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            opened: false,
        }
    }

    /// Bind a spool to `root`, creating it at `0700`.
    ///
    /// What: creates the directory and records that it existed, which is what
    /// makes a later `ENOENT` a failure rather than an empty listing.
    /// Test: `spool_open_creates_the_directory_at_0700`,
    /// `health_reports_error_when_the_spool_directory_is_gone`.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, SpoolError> {
        let mut spool = Self::at(root);
        spool.prepare_dir()?;
        spool.opened = true;
        Ok(spool)
    }

    /// The console's production spool location.
    ///
    /// Test: exercised indirectly by `WebhookIngress::from_env`.
    pub fn default_root() -> anyhow::Result<PathBuf> {
        Ok(trusty_common::resolve_data_dir("trusty-console")?.join(SPOOL_DIR_NAME))
    }

    /// Directory this spool writes into.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn prepare_dir(&self) -> Result<(), SpoolError> {
        std::fs::create_dir_all(&self.root).map_err(|source| SpoolError::PrepareDir {
            path: self.root.clone(),
            source,
        })?;
        std::fs::set_permissions(&self.root, Permissions::from_mode(SPOOL_DIR_MODE)).map_err(
            |source| SpoolError::PrepareDir {
                path: self.root.clone(),
                source,
            },
        )
    }

    /// Path an entry occupies, derived from its receipt time and delivery id.
    ///
    /// Why: the leading zero-padded millisecond timestamp makes a lexical sort
    /// an age sort, so the oldest-pending scan does not have to parse every
    /// file to find the oldest one. Public because [`Spool::record_attempt`]
    /// rewrites the same path and the tests assert on it.
    /// What: `<received_at_unix_ms:013>-<sanitised delivery id>.json`. The id is
    /// reduced to `[A-Za-z0-9_-]` and truncated so a hostile header value
    /// cannot traverse out of the spool directory or overflow `NAME_MAX`.
    /// Test: `spool_entry_path_sanitises_a_hostile_delivery_id`.
    pub fn entry_path(&self, entry: &SpoolEntry) -> PathBuf {
        self.root.join(format!(
            "{:013}-{}.json",
            entry.received_at_unix_ms,
            sanitise_delivery_id(&entry.delivery_id)
        ))
    }

    /// Write a NEW entry durably, refusing to overwrite an existing one.
    ///
    /// Why: this call returning `Ok` is the ONLY thing that licenses console to
    /// send GitHub a `202`. ADR-0034 §2: "Console returns `202` **only after**
    /// the delivery … is written and fsync'd to a spool." Refusing to clobber
    /// matters because the entry already at that path may be a delivery console
    /// has already acknowledged; overwriting it would destroy work GitHub will
    /// never re-send. (Defensive: GitHub always sends `X-GitHub-Delivery`, so
    /// two deliveries only collide when the header is absent and they land in
    /// the same millisecond.)
    ///
    /// What: encode → write temp with `create_new` → `sync_all` the file →
    /// `hard_link` into place → unlink the temp → `sync_all` the directory.
    /// `hard_link` rather than `rename` because rename silently replaces the
    /// destination while link fails with `EEXIST` — the atomic refusal this
    /// needs. The file fsync makes the bytes durable; the directory fsync makes
    /// the *name* durable, without which a crash can leave the entry
    /// unreachable even though its data reached the platter.
    ///
    /// # Errors
    ///
    /// Any [`SpoolError`]. Every one means the delivery is not recorded and the
    /// caller must return 5xx without acknowledging.
    ///
    /// Test: `spool_persists_and_reloads_an_entry_byte_exact`,
    /// `spool_persist_fails_when_the_root_is_not_a_directory`,
    /// `spool_persist_new_refuses_to_clobber_an_existing_entry`.
    pub fn persist_new(&self, entry: &SpoolEntry) -> Result<PathBuf, SpoolError> {
        let final_path = self.entry_path(entry);
        let tmp_path = self.write_temp(entry, &final_path)?;

        std::fs::hard_link(&tmp_path, &final_path).map_err(|source| {
            let _ = std::fs::remove_file(&tmp_path);
            if source.kind() == std::io::ErrorKind::AlreadyExists {
                SpoolError::AlreadyExists {
                    path: final_path.clone(),
                }
            } else {
                SpoolError::Commit {
                    from: tmp_path.clone(),
                    to: final_path.clone(),
                    source,
                }
            }
        })?;
        // The temp name is redundant once the entry is linked under its real
        // one; the inode survives until the last link goes.
        let _ = std::fs::remove_file(&tmp_path);

        sync_dir(&self.root)?;
        Ok(final_path)
    }

    /// Rewrite an entry that already exists, replacing it atomically.
    ///
    /// Why: [`Spool::record_attempt`] needs clobber semantics — updating the
    /// attempt count IS overwriting the previous version of the same delivery.
    /// Split from [`Spool::persist_new`] so the two intents cannot be confused
    /// at a call site.
    /// What: identical to `persist_new` except it commits with `rename`, which
    /// replaces the destination.
    /// Test: `spool_record_attempt_increments_durably`,
    /// `spool_persist_update_fails_when_the_final_path_is_a_directory`.
    pub fn persist_update(&self, entry: &SpoolEntry) -> Result<PathBuf, SpoolError> {
        let final_path = self.entry_path(entry);
        let tmp_path = self.write_temp(entry, &final_path)?;

        std::fs::rename(&tmp_path, &final_path).map_err(|source| {
            // Leaving the temp file behind on a failed rename would accumulate
            // junk the health scan then has to ignore; drop it here, and let
            // the rename error stand as the reported failure.
            let _ = std::fs::remove_file(&tmp_path);
            SpoolError::Commit {
                from: tmp_path.clone(),
                to: final_path.clone(),
                source,
            }
        })?;

        sync_dir(&self.root)?;
        Ok(final_path)
    }

    /// Encode `entry` into a fresh, fsync'd temp file beside `final_path`.
    ///
    /// The temp name carries the process id and a nanosecond stamp so two
    /// in-process writers targeting the same entry cannot share one, and uses
    /// `create_new` so a leftover from a crashed run is never appended to.
    fn write_temp(&self, entry: &SpoolEntry, final_path: &Path) -> Result<PathBuf, SpoolError> {
        let bytes = serde_json::to_vec_pretty(entry).map_err(|source| SpoolError::Encode {
            delivery_id: entry.delivery_id.clone(),
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
            file.set_permissions(Permissions::from_mode(SPOOL_FILE_MODE))?;
            file.write_all(&bytes)?;
            file.sync_all()
        };
        write().map_err(|source| SpoolError::Write {
            path: tmp_path.clone(),
            source,
        })?;
        Ok(tmp_path)
    }

    /// Record one failed relay attempt against a spooled entry.
    ///
    /// Why: ADR-0034 §2 — "Relay failure … leaves the spool entry `pending`
    /// with an incremented attempt count. It is never deleted on failure."
    /// The durable count is what a stuck delivery is diagnosed from; a
    /// `tracing::warn!` is explicitly forbidden as the sole record.
    /// What: bumps `attempts`, stores `reason` and the attempt time, and
    /// rewrites the entry through [`Spool::persist_update`]. Mutates `entry` in
    /// place so the caller sees the new count.
    ///
    /// The rewrite copies the whole entry, body included. That cost is bounded
    /// by [`super::BackoffPolicy`], which spaces retries exponentially and stops
    /// them entirely at `max_attempts` — without it a permanently unrelayable
    /// delivery would rewrite its own body plus two `fsync`s every sweep tick,
    /// forever.
    ///
    /// Test: `spool_record_attempt_increments_durably`,
    /// `relay_failure_leaves_a_pending_entry_with_an_incremented_attempt_count`.
    pub fn record_attempt(
        &self,
        entry: &mut SpoolEntry,
        reason: String,
        now_unix_ms: u64,
    ) -> Result<PathBuf, SpoolError> {
        entry.attempts = entry.attempts.saturating_add(1);
        entry.last_error = Some(reason);
        entry.last_attempt_at_unix_ms = Some(now_unix_ms);
        self.persist_update(entry)
    }

    /// Delete an entry the target has explicitly acknowledged.
    ///
    /// Why: the only deletion path in this module, reachable only from a
    /// `RelayOutcome::Acked`. "The connection succeeded" is deliberately not
    /// enough — ADR-0034 §2 makes the explicit ack the sole delete trigger, and
    /// treating a successful connect as a successful delivery is the same
    /// silent loss one layer down.
    /// What: `remove_file`, then fsyncs the directory so the deletion is as
    /// durable as the creation was. A missing file is not an error — a
    /// concurrent sweep may already have removed it.
    /// Test: `spool_remove_acked_deletes_the_entry`,
    /// `spool_remove_acked_tolerates_an_already_removed_entry`.
    pub fn remove_acked(&self, path: &Path) -> Result<(), SpoolError> {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(SpoolError::Remove {
                    path: path.to_path_buf(),
                    source,
                });
            }
        }
        sync_dir(&self.root)
    }

    /// Every entry currently pending, oldest first.
    ///
    /// Why: both the retry sweep and the health scan need this, and both need
    /// a *failure* to be distinguishable from an empty spool — an unreadable
    /// spool reported as "nothing pending" is the fail-quiet shape again.
    /// What: reads the directory, skips temp files and anything that is not a
    /// `.json` entry, decodes each, and sorts by receipt time. An entry that
    /// fails to decode is reported through `undecodable` rather than dropped.
    /// Test: `spool_list_pending_orders_oldest_first`,
    /// `spool_list_pending_reports_an_undecodable_entry`.
    pub fn list_pending(&self) -> Result<PendingListing, SpoolError> {
        let read = match std::fs::read_dir(&self.root) {
            Ok(read) => read,
            // 🔴 An absent directory is only "legitimately empty" for a spool
            // that was never opened. For an opened one it means the directory
            // was removed or its volume unmounted, which breaks ingress
            // completely — reporting that as an empty listing is what makes
            // `scan_health` answer green while every delivery 500s. That is the
            // fail-quiet shape one level below the one this module exists to
            // remove (ADR-0034 Consequences, "A spool that silently stops being
            // written reintroduces exactly the failure it was built to
            // prevent").
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !self.opened => {
                return Ok(PendingListing::default());
            }
            Err(source) => {
                return Err(SpoolError::ReadDir {
                    path: self.root.clone(),
                    source,
                });
            }
        };

        let mut listing = PendingListing::default();
        for dirent in read {
            let dirent = dirent.map_err(|source| SpoolError::ReadDir {
                path: self.root.clone(),
                source,
            })?;
            let path = dirent.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            match std::fs::read(&path)
                .map_err(SpoolReadFailure::Io)
                .and_then(|b| {
                    serde_json::from_slice::<SpoolEntry>(&b).map_err(SpoolReadFailure::Decode)
                }) {
                Ok(entry) => listing.pending.push(PendingEntry { path, entry }),
                Err(failure) => listing.undecodable.push((path, failure.to_string())),
            }
        }
        listing
            .pending
            .sort_by_key(|p| (p.entry.received_at_unix_ms, p.path.clone()));
        listing.undecodable.sort();
        Ok(listing)
    }
}

/// Result of one [`Spool::list_pending`] sweep.
///
/// `undecodable` is carried rather than discarded: a file that cannot be parsed
/// is still an unrelayed delivery, and reporting the spool as empty because its
/// contents are corrupt is the failure this design exists to prevent.
#[derive(Debug, Default, Clone)]
pub struct PendingListing {
    /// Decodable pending entries, oldest first.
    pub pending: Vec<PendingEntry>,
    /// Paths that could not be read or decoded, with the reason.
    pub undecodable: Vec<(PathBuf, String)>,
}

/// Why one entry could not be loaded. Internal; flattened to a string for
/// [`PendingListing::undecodable`].
#[derive(Debug, thiserror::Error)]
enum SpoolReadFailure {
    #[error("read: {0}")]
    Io(#[from] std::io::Error),
    #[error("decode: {0}")]
    Decode(#[from] serde_json::Error),
}

/// `fsync` a directory so a rename or unlink within it is durable.
fn sync_dir(dir: &Path) -> Result<(), SpoolError> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|source| SpoolError::SyncDir {
            path: dir.to_path_buf(),
            source,
        })
}

/// Reduce a delivery id to something safe to use as a filename component.
///
/// A `X-GitHub-Delivery` header is attacker-controlled as far as this process
/// is concerned — the HMAC covers the body, not the headers — so `../` or a
/// 4 KiB value must not reach `Path::join`.
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
