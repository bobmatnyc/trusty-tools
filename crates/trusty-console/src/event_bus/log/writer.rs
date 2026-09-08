//! [`DurableLog`]: the handle [`super::bus::EventBus`] holds, and the writer
//! task behind it.
//!
//! Why: DOC-73 §4.3's log is written "as each frame is accepted" but ingest
//! must never block on it (§4.1's non-blocking invariant, and the "all I/O
//! off the ingest hot path" design constraint this issue states directly).
//! Splitting the handle ([`DurableLog`], cheap to hold and clone the pieces
//! of) from the writer (one task, one open file, all the actual I/O) is what
//! makes `EventBus::ingest` a synchronous, non-blocking `try_send` rather
//! than an `await` on disk.
//! What: [`DurableLog::open`] hardens and prepares the directory, applies
//! retention, recovers the seq high-water mark, spawns the writer task, and
//! returns a [`DurableLog`] plus the [`RecoveredState`] the caller (`EventBus
//! ::with_log`) needs to resume numbering. [`DurableLog::enqueue`] is a
//! non-blocking `try_send` into a [`WRITE_CHANNEL_CAPACITY`]-bounded channel —
//! `false` means the writer's queue was full and this event will never be
//! written (a genuine, permanent gap; [`super::replay`] detects it from the
//! resulting seq discontinuity, nothing else needs to track it). The writer
//! task rotates to a new day file at each UTC day boundary (never `event.at`,
//! which a producer controls and could set to any value), re-applies
//! retention on every rotation, and re-derives `earliest_retained_seq` from
//! whatever survives so [`super::replay::replay_since`] always compares
//! against the truth.
//! Test: `super::tests::open_recovers_next_seq_from_an_existing_log`,
//! `super::tests::backpressure_drops_are_counted_and_do_not_block_enqueue`,
//! `super::tests::events_written_are_readable_back_in_order`,
//! `super::tests::rotation_opens_a_new_file_and_keeps_seq_continuity`,
//! `super::tests::rotation_fsyncs_the_outgoing_file_before_opening_the_next_one`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{NaiveDate, Utc};
use tokio::sync::mpsc;
use trusty_common::control_bus::HarnessEvent;

use super::config::{LogConfig, WRITE_CHANNEL_CAPACITY, day_file_name};
use super::error::LogError;
use super::format::encode_line;
use super::recovery::{earliest_seq, recover_next_seq};
use super::replay::{ReplayItem, replay_since};
use super::retention::enforce_retention;

/// What [`DurableLog::open`] recovered from disk before accepting a single
/// new event.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RecoveredState {
    /// The seq [`super::bus::EventBus`] should assign to the next accepted
    /// event.
    pub next_seq: u64,
}

/// A handle to the durable NDJSON event log. Cheap to hold inside
/// [`super::bus::EventBus`]: the actual file and its I/O live in the writer
/// task this was opened with.
pub(crate) struct DurableLog {
    dir: PathBuf,
    tx: mpsc::Sender<HarnessEvent>,
    // Read today only by `written()`, `#[cfg(test)]`-gated below; a later
    // slice's metrics route (#6850) is the production reader.
    #[allow(dead_code)]
    written: Arc<AtomicU64>,
    // 0 is the "nothing retained yet" sentinel — seq is 1-based, so 0 is never
    // a real value, and `AtomicU64` avoids a mutex for a single counter two
    // tasks only ever read or replace wholesale.
    earliest_retained_seq: Arc<AtomicU64>,
}

impl DurableLog {
    /// Open (or create) the durable log at `config.dir`, recover its seq
    /// high-water mark, apply retention, and spawn the writer task.
    ///
    /// # Errors
    ///
    /// [`LogError::PrepareDir`] if the directory cannot be hardened to
    /// `0700`; [`LogError::Io`] if an existing file cannot be listed, read,
    /// or an expired one deleted. A caller that gets `Err` here should still
    /// start the bus without durability (`EventBus::new`) rather than fail
    /// console startup — DOC-73 §4.1's non-blocking invariant covers the log
    /// exactly as it covers the ingest socket.
    pub(crate) async fn open(config: LogConfig) -> Result<(Self, RecoveredState), LogError> {
        Self::open_with_capacity(config, WRITE_CHANNEL_CAPACITY).await
    }

    /// [`DurableLog::open`] with the writer channel's bound taken explicitly,
    /// so a test can prove backpressure behavior against a capacity of 1
    /// instead of enqueuing thousands of events.
    ///
    /// Test: `super::tests::backpressure_drops_are_counted_and_do_not_block_enqueue`.
    pub(crate) async fn open_with_capacity(
        config: LogConfig,
        channel_capacity: usize,
    ) -> Result<(Self, RecoveredState), LogError> {
        trusty_common::uds::prepare_socket_dir(&config.dir).map_err(|source| {
            LogError::PrepareDir {
                path: config.dir.clone(),
                source,
            }
        })?;

        let today = Utc::now().date_naive();
        let survivors = enforce_retention(&config.dir, today, config.retain_days).await?;
        let next_seq = recover_next_seq(&survivors).await?;
        let earliest = earliest_seq(&survivors).await?;

        let (tx, rx) = mpsc::channel(channel_capacity.max(1));
        let written = Arc::new(AtomicU64::new(0));
        let earliest_retained_seq = Arc::new(AtomicU64::new(earliest.unwrap_or(0)));

        tokio::spawn(run_writer(
            config.clone(),
            rx,
            Arc::clone(&written),
            Arc::clone(&earliest_retained_seq),
        ));

        Ok((
            Self {
                dir: config.dir,
                tx,
                written,
                earliest_retained_seq,
            },
            RecoveredState { next_seq },
        ))
    }

    /// Hand `event` to the writer task. `true` means it was accepted into the
    /// bounded queue (it WILL be written, though not necessarily yet — see
    /// the module docs' persisted-flag contract); `false` means the queue was
    /// full and this event will never reach the log.
    pub(crate) fn enqueue(&self, event: HarnessEvent) -> bool {
        self.tx.try_send(event).is_ok()
    }

    /// How many events the writer has durably written (write + flush
    /// returned) since this log was opened. Test-only introspection for
    /// waiting on the writer to catch up without a fixed sleep.
    #[cfg(test)]
    pub(crate) fn written(&self) -> u64 {
        self.written.load(Ordering::Relaxed)
    }

    /// Replay everything persisted after `since_seq`, in order — see
    /// [`super::replay::replay_since`].
    ///
    /// # Errors
    ///
    /// [`LogError::Io`] if a day file cannot be listed or read.
    pub(crate) async fn replay_since(&self, since_seq: u64) -> Result<Vec<ReplayItem>, LogError> {
        replay_since(&self.dir, since_seq, self.earliest_retained_seq()).await
    }

    fn earliest_retained_seq(&self) -> Option<u64> {
        match self.earliest_retained_seq.load(Ordering::Relaxed) {
            0 => None,
            n => Some(n),
        }
    }
}

/// The writer task body: owns the one open file, rotates at a UTC day
/// boundary, and exits (after a best-effort final sync) once every
/// [`DurableLog`] clone's sender has dropped and the channel drains.
async fn run_writer(
    config: LogConfig,
    mut rx: mpsc::Receiver<HarnessEvent>,
    written: Arc<AtomicU64>,
    earliest_retained_seq: Arc<AtomicU64>,
) {
    let mut open_day: Option<NaiveDate> = None;
    let mut file: Option<tokio::fs::File> = None;
    let mut current_path: Option<PathBuf> = None;

    while let Some(event) = rx.recv().await {
        let today = Utc::now().date_naive();
        if open_day != Some(today) {
            match rotate(&config, today, &earliest_retained_seq, file.as_mut()).await {
                Ok((f, path)) => {
                    file = Some(f);
                    current_path = Some(path);
                    open_day = Some(today);
                }
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "event log: could not rotate to today's file; this event \
                         will not be persisted"
                    );
                    continue;
                }
            }
        }

        let (Some(f), Some(path)) = (file.as_mut(), current_path.as_deref()) else {
            continue;
        };
        if let Err(e) = write_line(f, path, &event).await {
            tracing::error!(
                error = %e,
                seq = event.seq,
                "event log: write failed; this event will not be persisted"
            );
            continue;
        }
        written.fetch_add(1, Ordering::Relaxed);
    }

    if let Some(f) = file.as_mut() {
        let _ = tokio::io::AsyncWriteExt::flush(f).await;
        let _ = f.sync_data().await;
    }
}

/// Open (create/append) `today`'s file, apply retention, and refresh
/// `earliest_retained_seq` from whatever survives. `previous` is the
/// currently-open handle being rotated away from, if any — fsync'd (flush,
/// then `sync_data`) before the new file is opened, matching the durability
/// contract this module's own doc comment states ("full fsync happens on
/// rotation and on writer-task shutdown, not per line"). A sync failure is
/// logged, not propagated: a rotation that cannot confirm the old file's
/// durability should still proceed to today's file rather than stop
/// persisting entirely (DOC-73 §4.1's non-blocking invariant).
///
/// Test: `super::tests::rotation_fsyncs_the_outgoing_file_before_opening_the_next_one`.
pub(super) async fn rotate(
    config: &LogConfig,
    today: NaiveDate,
    earliest_retained_seq: &Arc<AtomicU64>,
    previous: Option<&mut tokio::fs::File>,
) -> Result<(tokio::fs::File, PathBuf), LogError> {
    // #6848: previously only synced at writer-task shutdown, so a crash
    // between two rotations could lose an arbitrary, un-fsynced tail of a
    // whole PRIOR day's file — not just its last line.
    if let Some(f) = previous {
        let _ = tokio::io::AsyncWriteExt::flush(f).await;
        if let Err(source) = f.sync_data().await {
            tracing::error!(
                error = %source,
                "event log: fsync of the outgoing day file failed at rotation; \
                 its un-synced tail may not survive a crash"
            );
        }
    }

    let survivors = enforce_retention(&config.dir, today, config.retain_days).await?;
    let earliest = earliest_seq(&survivors).await?;
    earliest_retained_seq.store(earliest.unwrap_or(0), Ordering::Relaxed);

    let path = config.dir.join(day_file_name(today));
    let file = open_append_0600(&path).await?;
    Ok((file, path))
}

/// Open `path` for append, creating it at `0600` if absent — matching this
/// workspace's `0600`-file convention (`trusty_common::uds::SOCKET_MODE`
/// reused for the value, not the socket-specific bind path) — and
/// re-asserting `0600` if a file already existed at a wider mode, the same
/// defense-in-depth `set_permissions_0600` in `credentials/file_store.rs`
/// applies to its own private file.
#[cfg(unix)]
async fn open_append_0600(path: &std::path::Path) -> Result<tokio::fs::File, LogError> {
    use std::os::unix::fs::PermissionsExt as _;
    let file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(trusty_common::uds::SOCKET_MODE)
        .open(path)
        .await
        .map_err(|source| LogError::Io {
            op: "open",
            path: path.to_path_buf(),
            source,
        })?;
    // #6848: log instead of discarding — a silent failure here means this
    // defense-in-depth re-assertion of 0600 can fail with zero
    // observability (the parent directory's own 0700 hardening still blocks
    // cross-user access even when this fails, but an operator should still
    // be able to see it happened).
    if let Err(e) = file
        .set_permissions(std::fs::Permissions::from_mode(
            trusty_common::uds::SOCKET_MODE,
        ))
        .await
    {
        tracing::warn!(
            error = %e,
            path = %path.display(),
            "event log: could not re-assert 0600 permissions on an existing file"
        );
    }
    Ok(file)
}

#[cfg(not(unix))]
async fn open_append_0600(path: &std::path::Path) -> Result<tokio::fs::File, LogError> {
    tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .map_err(|source| LogError::Io {
            op: "open",
            path: path.to_path_buf(),
            source,
        })
}

/// Serialize `event` and append it to `path`'s open `file`, flushing so a
/// reader opening the file immediately after sees the line (full `fsync`
/// happens on rotation and shutdown only — see the module docs' durability
/// trade-off).
///
/// `path` is threaded through purely for [`LogError::Io`]'s diagnostic
/// value — #6848: an earlier version built these with an empty `PathBuf`,
/// so a write/flush failure logged as `"write the event-log file at : …"`,
/// dropping the one fact (which day file) an operator needs to act on it.
pub(super) async fn write_line(
    file: &mut tokio::fs::File,
    path: &Path,
    event: &HarnessEvent,
) -> Result<(), LogError> {
    use tokio::io::AsyncWriteExt as _;
    let line = encode_line(event);
    file.write_all(&line).await.map_err(|source| LogError::Io {
        op: "write",
        path: path.to_path_buf(),
        source,
    })?;
    file.flush().await.map_err(|source| LogError::Io {
        op: "flush",
        path: path.to_path_buf(),
        source,
    })
}
