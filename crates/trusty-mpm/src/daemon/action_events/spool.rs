//! On-disk spool for action events the bus did not take at shutdown (#10412).
//!
//! Why: the push buffer lives in memory, so a daemon stopping while
//! trusty-events is down lost every buffered event (4096 on the 2026-10-29
//! restart). A drain alone cannot help when the bus is absent.
//! What: [`persist`] writes one complete NDJSON file per shutdown (temp file,
//! then rename). [`take_all`] claims each file by renaming it to a hidden
//! name only one process can win, reads it, and deletes it, so a file is
//! replayed at most once even with two takers.
//! Test: `spool_tests.rs`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use trusty_common::control_bus::HarnessEvent;

/// The spool's directory name under the daemon's data dir.
pub const SPOOL_DIR_NAME: &str = "action-events-spool";

/// Suffix of a complete, unclaimed spool file.
const SPOOL_SUFFIX: &str = ".ndjson";

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
    /// Files that could not be claimed or read; logged, left on disk.
    pub failed_files: usize,
}

/// Write `events` to a new file in `dir`; returns how many were written.
///
/// What: no file for an empty slice. The file is written under a hidden
/// temp name, then renamed, so [`take_all`] never sees half of it. #10412:
/// no fsync; the spool must outlive a process stop, which the page cache
/// does, and an fsync under load overran the 200 ms signal budget.
///
/// # Errors
///
/// Any I/O or serialization error; the temp file is removed best-effort.
pub fn persist(dir: &Path, events: &[HarnessEvent]) -> std::io::Result<usize> {
    if events.is_empty() {
        return Ok(0);
    }
    std::fs::create_dir_all(dir)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = FILE_SEQ.fetch_add(1, Ordering::Relaxed);
    let name = format!("{nanos:024}-{}-{seq}{SPOOL_SUFFIX}", std::process::id());
    let tmp = dir.join(format!(".{name}.tmp"));
    let written = write_file(&tmp, events).and_then(|()| std::fs::rename(&tmp, dir.join(&name)));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.map(|()| events.len())
}

/// [`persist`]'s body: owner-only file, one JSON line per event.
fn write_file(path: &Path, events: &[HarnessEvent]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut out = std::io::BufWriter::new(options.open(path)?);
    for event in events {
        serde_json::to_writer(&mut out, event)?;
        out.write_all(b"\n")?;
    }
    out.flush()
}

/// Claim, read and delete every complete spool file in `dir`.
///
/// What: a missing `dir` is an empty replay. Files go oldest first (names
/// start with a zero-padded timestamp). A claim lost to another process is
/// skipped; any other failure is counted in [`Replay::failed_files`] and
/// logged. A claimed file whose delete fails stays under its hidden name,
/// which no later scan reads, so its events are never replayed twice.
///
/// # Errors
///
/// When `dir` exists but cannot be listed.
pub fn take_all(dir: &Path) -> std::io::Result<Replay> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Replay::default()),
        Err(e) => return Err(e),
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.') && name.ends_with(SPOOL_SUFFIX))
        .collect();
    names.sort();
    let mut replay = Replay::default();
    for name in names {
        let claim = dir.join(format!(".{name}.claim-{}", std::process::id()));
        match std::fs::rename(dir.join(&name), &claim) {
            Ok(()) => {}
            // Another process claimed it first.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                tracing::warn!(error = %e, file = %name, "action events: spool file not claimed; left for the next start");
                replay.failed_files += 1;
                continue;
            }
        }
        let text = match std::fs::read_to_string(&claim) {
            Ok(text) => text,
            Err(e) => {
                tracing::warn!(error = %e, file = %claim.display(), "action events: claimed spool file unreadable; its events are lost");
                replay.failed_files += 1;
                continue;
            }
        };
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            match serde_json::from_str(line) {
                Ok(event) => replay.events.push(event),
                Err(_) => replay.skipped += 1,
            }
        }
        if let Err(e) = std::fs::remove_file(&claim) {
            tracing::warn!(error = %e, file = %claim.display(), "action events: replayed spool file not removed; it is never read again");
        }
    }
    Ok(replay)
}
