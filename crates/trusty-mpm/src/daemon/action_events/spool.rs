//! On-disk spool for action events the bus did not take at shutdown (#10412).
//!
//! Why: the push buffer lives in memory, so a daemon stopping while
//! trusty-events is down lost every buffered event (4096 on the 2026-10-29
//! restart). A drain alone cannot help when the bus is absent.
//! What: [`persist`] writes one complete NDJSON file per stop (a locked temp
//! file, then a rename). [`take_all`] takes each file under an exclusive
//! file lock, reads it, and deletes it. A lock is the kernel's, released
//! when its holder exits or dies, so liveness needs no pid and pid reuse
//! cannot fool it: a locked file has a live taker and is left alone; an
//! unlocked one, including one a dead taker read but never deleted, is
//! replayed. A temp file is swept once it is past [`TMP_GRACE`] and unlocked.
//! Test: `spool_tests.rs`.

use std::fs::{File, TryLockError};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use trusty_common::control_bus::HarnessEvent;

/// The spool's directory name under the daemon's data dir.
pub const SPOOL_DIR_NAME: &str = "action-events-spool";

/// How old an unlocked temp file must be before [`take_all`] removes it.
/// The lock already marks a live writer; the age covers the instant between
/// its create and its lock.
pub const TMP_GRACE: Duration = Duration::from_secs(60);

/// Suffix of a complete spool file.
const SPOOL_SUFFIX: &str = ".ndjson";

/// Suffix of a spool file still being written.
const TMP_SUFFIX: &str = ".tmp";

/// Tells apart two files one process writes in the same nanosecond.
static FILE_SEQ: AtomicU64 = AtomicU64::new(0);

/// The spool directory under `data_dir`.
pub fn spool_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(SPOOL_DIR_NAME)
}

/// What [`take_all`] read back.
#[derive(Debug, Default)]
pub struct Replay {
    /// Events in file order, files oldest first.
    pub events: Vec<HarnessEvent>,
    /// Lines that did not parse (a torn tail), counted, never replayed.
    pub skipped: usize,
    /// Files that could not be taken or read; logged, left on disk.
    pub failed_files: usize,
    /// Files another live process holds; left for it.
    pub held: usize,
    /// Abandoned temp files removed.
    pub swept_tmp: usize,
}

/// Write `events` to a new file in `dir`; returns how many were written.
///
/// What: no file for an empty slice. The directory is created owner-only.
/// The file is written under a hidden temp name, locked for the whole
/// write, then renamed, so [`take_all`] never sees half of it. #10412: no
/// fsync; the spool must outlive a process stop, which the page cache
/// does, and an fsync under load overran the 200 ms signal budget.
///
/// # Errors
///
/// Any I/O or serialization error; the temp file is removed best-effort.
pub fn persist(dir: &Path, events: &[HarnessEvent]) -> std::io::Result<usize> {
    if events.is_empty() {
        return Ok(0);
    }
    create_private_dir(dir)?;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = FILE_SEQ.fetch_add(1, Ordering::Relaxed);
    let name = format!("{nanos:024}-{}-{seq}{SPOOL_SUFFIX}", std::process::id());
    let tmp = dir.join(format!(".{name}{TMP_SUFFIX}"));
    let written = write_file(&tmp, &dir.join(&name), events);
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.map(|()| events.len())
}

/// `dir` and its missing parents, mode 0700 on unix.
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

/// [`persist`]'s body: an owner-only `tmp`, locked, written, renamed to
/// `done`; the lock goes when the file closes after the rename.
fn write_file(tmp: &Path, done: &Path, events: &[HarnessEvent]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let file = options.open(tmp)?;
    lock_now(&file)?;
    let mut out = std::io::BufWriter::new(&file);
    for event in events {
        serde_json::to_writer(&mut out, event)?;
        out.write_all(b"\n")?;
    }
    out.flush()?;
    drop(out);
    std::fs::rename(tmp, done)
}

/// Take `file`'s exclusive lock without waiting; held elsewhere is an error.
fn lock_now(file: &File) -> std::io::Result<()> {
    file.try_lock().map_err(|e| match e {
        TryLockError::WouldBlock => std::io::Error::from(std::io::ErrorKind::WouldBlock),
        TryLockError::Error(e) => e,
    })
}

/// [`take_all_at`] at the current time.
///
/// # Errors
///
/// When `dir` exists but cannot be listed.
pub fn take_all(dir: &Path) -> std::io::Result<Replay> {
    take_all_at(dir, SystemTime::now())
}

/// Sweep abandoned temp files, then take every complete spool file in `dir`.
///
/// What: a missing `dir` is an empty replay. Files go oldest first (names
/// start with a zero-padded timestamp). A file locked by a live process is
/// counted in [`Replay::held`] and left. A taken file is deleted; if the
/// delete fails it is emptied, so no later start replays it twice. A temp
/// file older than [`TMP_GRACE`] at `now` and unlocked is removed and logged.
///
/// # Errors
///
/// When `dir` exists but cannot be listed.
pub fn take_all_at(dir: &Path, now: SystemTime) -> std::io::Result<Replay> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Replay::default()),
        Err(e) => return Err(e),
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .collect();
    names.sort();
    let mut replay = Replay::default();
    for name in names {
        let path = dir.join(&name);
        if name.starts_with('.') && name.ends_with(TMP_SUFFIX) {
            replay.swept_tmp += usize::from(sweep_tmp(&path, now));
        } else if !name.starts_with('.') && name.ends_with(SPOOL_SUFFIX) {
            take_one(&path, &mut replay);
        }
    }
    Ok(replay)
}

/// Remove `path` when it is past [`TMP_GRACE`] at `now` and no writer holds
/// its lock; returns whether it went.
fn sweep_tmp(path: &Path, now: SystemTime) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let Ok(meta) = file.metadata() else {
        return false;
    };
    let age = meta
        .modified()
        .ok()
        .and_then(|at| now.duration_since(at).ok())
        .unwrap_or_default();
    if age < TMP_GRACE || lock_now(&file).is_err() {
        return false;
    }
    match std::fs::remove_file(path) {
        Ok(()) => {
            tracing::warn!(file = %path.display(), bytes = meta.len(), "action events: removed a spool temp file a stopped write left");
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, file = %path.display(), "action events: abandoned spool temp file not removed");
            false
        }
    }
}

/// Take one complete spool file under its lock into `replay`.
fn take_one(path: &Path, replay: &mut Replay) {
    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
    {
        Ok(file) => file,
        // Another taker deleted it first.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            tracing::warn!(error = %e, file = %path.display(), "action events: spool file not opened; left for the next start");
            replay.failed_files += 1;
            return;
        }
    };
    if lock_now(&file).is_err() {
        replay.held += 1;
        return;
    }
    // Another taker may have deleted it between our open and our lock.
    if !still_at(path, &file) {
        return;
    }
    let mut text = String::new();
    if let Err(e) = file.read_to_string(&mut text) {
        tracing::warn!(error = %e, file = %path.display(), "action events: spool file unreadable; left for the next start");
        replay.failed_files += 1;
        return;
    }
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str(line) {
            Ok(event) => replay.events.push(event),
            Err(_) => replay.skipped += 1,
        }
    }
    if let Err(e) = std::fs::remove_file(path) {
        let emptied = file.set_len(0);
        tracing::warn!(error = %e, emptied = emptied.is_ok(), file = %path.display(), "action events: replayed spool file not removed");
    }
}

/// Whether `path` still names the file `file` has open.
#[cfg(unix)]
fn still_at(path: &Path, file: &File) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(path), file.metadata()) {
        (Ok(named), Ok(open)) => named.dev() == open.dev() && named.ino() == open.ino(),
        _ => false,
    }
}

/// Whether `path` still names the file `file` has open.
#[cfg(not(unix))]
fn still_at(path: &Path, _file: &File) -> bool {
    path.exists()
}
