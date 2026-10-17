//! Read-only storage probe for one sandbox index (#9993 G6).
//!
//! Why: the perf plan's storage rows (bytes per index file, the total, bytes
//! per chunk, daemon RSS) had no reader, and the release gate compares
//! bytes/chunk and RSS between baselines. The probe must never write to the
//! index or the daemon, and must never reach the live install.
//! What: [`sandbox_pid`] refuses any daemon that `scripts/sandbox_search_daemon.sh`
//! did not start; [`storage_dirs`] resolves an index's storage directories
//! under both layouts without creating either; [`snapshot`] walks one with
//! `symlink_metadata` only; [`rss_bytes`] reads a pid's resident set (Linux
//! `/proc/<pid>/status`, macOS `ps`); [`storage_rows`] turns the reads into
//! schema-1 rows, dimension `storage`, unit `bytes`. Every failure is an
//! `Err` naming its cause, never an empty or zero row.
//! Test: `snapshot_sums_exact_file_bytes_without_following_symlinks`,
//! `snapshot_refuses_a_missing_dir_a_file_and_an_empty_dir`,
//! `snapshot_refuses_an_unreadable_subdirectory`,
//! `unchanged_passes_a_read_only_probe_and_fails_on_a_write`,
//! `storage_dirs_reads_both_layouts_and_refuses_neither`,
//! `rss_reads_a_live_pid_and_refuses_unsupported_or_missing_pids`,
//! `vmrss_parser_reads_kib_and_refuses_absent_or_zero`,
//! `sandbox_pid_requires_the_pidfile_and_a_matching_argv`,
//! `storage_rows_carry_exact_bytes_and_refuse_an_empty_index`.

#![allow(dead_code)] // Each test binary uses a different subset.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use trusty_search::service::colocated_storage::COLOCATED_DIR_NAME;
use trusty_search::service::persistence::sanitize_id_for_path;

use super::perf_envelope::ResultRow;

/// The pid file `scripts/sandbox_search_daemon.sh` writes into its sandbox dir.
pub const SANDBOX_PID_FILE: &str = "sandbox.pid";
/// The data directory name the sandbox script passes as `--data-dir <dir>/data`.
pub const SANDBOX_DATA_DIR: &str = "data";

/// What kind of directory entry a snapshot row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    Other,
}

/// One entry under a storage directory, as `symlink_metadata` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Path relative to the snapshot's directory.
    pub rel: PathBuf,
    pub kind: Kind,
    pub len: u64,
    pub modified: SystemTime,
}

/// Every entry under one directory, sorted by relative path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub dir: PathBuf,
    pub entries: Vec<Entry>,
}

impl Snapshot {
    /// Regular files only; directories and symlinks carry no index bytes.
    pub fn files(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.kind == Kind::File)
    }

    /// Sum of regular-file lengths, in bytes.
    pub fn file_bytes(&self) -> u64 {
        self.files().map(|e| e.len).sum()
    }
}

/// Walk `dir` read-only: stat calls and directory reads, nothing else.
///
/// Why: the probe must leave the index byte- and mtime-identical (#9993 G6).
/// What: `symlink_metadata` on every entry, so a symlink is recorded and not
/// followed. A missing path, a non-directory, an unreadable entry, or a tree
/// holding no regular file is an `Err` naming the path.
/// Test: `snapshot_sums_exact_file_bytes_without_following_symlinks`,
/// `snapshot_refuses_a_missing_dir_a_file_and_an_empty_dir`,
/// `snapshot_refuses_an_unreadable_subdirectory`.
pub fn snapshot(dir: &Path) -> Result<Snapshot, String> {
    let meta = std::fs::symlink_metadata(dir)
        .map_err(|e| format!("storage directory {}: {e}", dir.display()))?;
    if !meta.is_dir() {
        return Err(format!(
            "storage directory {} is not a directory",
            dir.display()
        ));
    }
    let mut entries = Vec::new();
    walk(dir, Path::new(""), &mut entries)?;
    entries.sort_by(|a, b| a.rel.cmp(&b.rel));
    if !entries.iter().any(|e| e.kind == Kind::File) {
        return Err(format!(
            "storage directory {} holds no files; the index is empty",
            dir.display()
        ));
    }
    Ok(Snapshot {
        dir: dir.to_path_buf(),
        entries,
    })
}

fn walk(base: &Path, rel: &Path, out: &mut Vec<Entry>) -> Result<(), String> {
    let here = base.join(rel);
    let listing = std::fs::read_dir(&here)
        .map_err(|e| format!("read directory {}: {e}", here.display()))?;
    for item in listing {
        let item = item.map_err(|e| format!("read an entry of {}: {e}", here.display()))?;
        let child = rel.join(item.file_name());
        let path = base.join(&child);
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| format!("stat {}: {e}", path.display()))?;
        let modified = meta
            .modified()
            .map_err(|e| format!("mtime of {}: {e}", path.display()))?;
        let file_type = meta.file_type();
        let kind = if file_type.is_symlink() {
            Kind::Symlink
        } else if file_type.is_dir() {
            Kind::Dir
        } else if file_type.is_file() {
            Kind::File
        } else {
            Kind::Other
        };
        out.push(Entry {
            rel: child.clone(),
            kind,
            len: meta.len(),
            modified,
        });
        if kind == Kind::Dir {
            walk(base, &child, out)?;
        }
    }
    Ok(())
}

/// `Ok` when `before` and `after` list the same entries, lengths and mtimes.
///
/// What: the `Err` names the directory and the first entry that differs, or
/// the entry present on one side only.
/// Test: `unchanged_passes_a_read_only_probe_and_fails_on_a_write`.
pub fn unchanged(before: &[Snapshot], after: &[Snapshot]) -> Result<(), String> {
    if before.len() != after.len() {
        return Err(format!(
            "{} storage directories before, {} after",
            before.len(),
            after.len()
        ));
    }
    for (b, a) in before.iter().zip(after) {
        if b == a {
            continue;
        }
        let first = b
            .entries
            .iter()
            .zip(&a.entries)
            .find(|(x, y)| x != y)
            .map(|(x, y)| format!("{x:?} became {y:?}"))
            .unwrap_or_else(|| {
                format!(
                    "{} entries before, {} after",
                    b.entries.len(),
                    a.entries.len()
                )
            });
        return Err(format!("{} changed during the probe: {first}", b.dir.display()));
    }
    Ok(())
}

/// One storage directory of an index and the layout it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageDir {
    /// `data_dir` (`<data_dir>/indexes/<id>/`) or `colocated`
    /// (`<root_path>/.trusty-search/`).
    pub layout: &'static str,
    pub path: PathBuf,
}

/// The storage directories of `index_id`, resolved without creating any.
///
/// Why: since #8499 a new index stores under the daemon's data dir, while a
/// pre-#8499 index lives colocated under its root; the daemon's own
/// `disk_bytes` sums both (`status.rs::index_storage_dirs`), so the probe
/// does too. Both paths are joined by hand: the crate's path helpers call
/// `create_dir_all`.
/// What: the legacy dir counts when it is a directory; the colocated one
/// only when it holds `index.redb`, the daemon's own rule. A stat failure
/// other than not-found is an `Err`, and so is finding neither.
/// Test: `storage_dirs_reads_both_layouts_and_refuses_neither`.
pub fn storage_dirs(
    data_dir: &Path,
    index_id: &str,
    root_path: &Path,
) -> Result<Vec<StorageDir>, String> {
    let legacy = data_dir.join("indexes").join(sanitize_id_for_path(index_id));
    let colocated = root_path.join(COLOCATED_DIR_NAME);
    let mut dirs = Vec::new();
    if stat_kind(&legacy)? == Some(Kind::Dir) {
        dirs.push(StorageDir {
            layout: "data_dir",
            path: legacy.clone(),
        });
    }
    if stat_kind(&colocated.join("index.redb"))? == Some(Kind::File) {
        dirs.push(StorageDir {
            layout: "colocated",
            path: colocated.clone(),
        });
    }
    if dirs.is_empty() {
        return Err(format!(
            "index {index_id}: no storage directory at {} or {}",
            legacy.display(),
            colocated.join("index.redb").display()
        ));
    }
    Ok(dirs)
}

/// `None` when `path` does not exist; any other stat failure is an `Err`.
fn stat_kind(path: &Path) -> Result<Option<Kind>, String> {
    match std::fs::metadata(path) {
        Ok(m) if m.is_dir() => Ok(Some(Kind::Dir)),
        Ok(m) if m.is_file() => Ok(Some(Kind::File)),
        Ok(_) => Ok(Some(Kind::Other)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("stat {}: {e}", path.display())),
    }
}

/// A snapshot of every directory in `dirs`, in order.
pub fn snapshots(dirs: &[StorageDir]) -> Result<Vec<Snapshot>, String> {
    dirs.iter().map(|d| snapshot(&d.path)).collect()
}

/// A process's resident set and where it was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RssReading {
    pub bytes: u64,
    pub source: &'static str,
}

/// RSS of `pid` on this host's OS.
pub fn rss_bytes(pid: u32) -> Result<RssReading, String> {
    rss_bytes_on(std::env::consts::OS, pid)
}

/// RSS of `pid`, read the way `os` supports.
///
/// Why: a missing pid or an unsupported OS must not become a `0` row (#9993 G6).
/// What: Linux reads `VmRSS` from `/proc/<pid>/status`; macOS runs
/// `/bin/ps -o rss= -p <pid>`. Both report KiB, returned as bytes. Any other
/// OS, a pid of 0 or 1, a missing process, or a reading of 0 is an `Err`.
/// Test: `rss_reads_a_live_pid_and_refuses_unsupported_or_missing_pids`.
pub fn rss_bytes_on(os: &str, pid: u32) -> Result<RssReading, String> {
    if pid <= 1 {
        return Err(format!("refusing to read RSS of pid {pid}"));
    }
    let (kib, source) = match os {
        "linux" => {
            let path = format!("/proc/{pid}/status");
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("no process {pid}: read {path}: {e}"))?;
            (parse_vmrss_kib(&text)?, "/proc/<pid>/status VmRSS")
        }
        "macos" => (ps_rss_kib(pid)?, "ps -o rss="),
        other => return Err(format!("reading process RSS is unsupported on {other}")),
    };
    if kib == 0 {
        return Err(format!("pid {pid} reported an RSS of 0 kB; not recorded"));
    }
    Ok(RssReading {
        bytes: kib * 1024,
        source,
    })
}

/// The `VmRSS:` value of a `/proc/<pid>/status` text, in KiB.
///
/// Test: `vmrss_parser_reads_kib_and_refuses_absent_or_zero`.
pub fn parse_vmrss_kib(status: &str) -> Result<u64, String> {
    let line = status
        .lines()
        .find(|l| l.starts_with("VmRSS:"))
        .ok_or("process status has no VmRSS line (kernel thread or zombie)")?;
    let mut parts = line["VmRSS:".len()..].split_whitespace();
    let value = parts.next().ok_or("VmRSS line has no value")?;
    if parts.next() != Some("kB") {
        return Err(format!("VmRSS line is not in kB: {line}"));
    }
    let kib: u64 = value
        .parse()
        .map_err(|e| format!("VmRSS value {value}: {e}"))?;
    if kib == 0 {
        return Err("VmRSS is 0 kB".into());
    }
    Ok(kib)
}

fn ps_rss_kib(pid: u32) -> Result<u64, String> {
    let out = ps(pid, "rss=")?;
    out.parse()
        .map_err(|e| format!("ps rss for pid {pid}: {out:?}: {e}"))
}

/// One `ps` column for `pid`; a non-zero exit or empty output is an `Err`.
fn ps(pid: u32, column: &str) -> Result<String, String> {
    let output = Command::new("/bin/ps")
        .args(["-o", column, "-p", &pid.to_string()])
        .output()
        .map_err(|e| format!("run /bin/ps for pid {pid}: {e}"))?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() || text.is_empty() {
        return Err(format!("no process {pid} (ps exit {})", output.status));
    }
    Ok(text)
}

/// The command line of `pid`, its arguments joined by single spaces.
fn process_argv(pid: u32) -> Result<String, String> {
    match std::env::consts::OS {
        "linux" => {
            let path = format!("/proc/{pid}/cmdline");
            let raw =
                std::fs::read(&path).map_err(|e| format!("no process {pid}: read {path}: {e}"))?;
            let args: Vec<String> = raw
                .split(|b| *b == 0)
                .filter(|a| !a.is_empty())
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect();
            Ok(args.join(" "))
        }
        "macos" => ps(pid, "command="),
        other => Err(format!("reading a process command line is unsupported on {other}")),
    }
}

/// Whether `argv` holds `--data-dir <data_dir>` followed by a space or the end.
///
/// What: the same token rule `sandbox_search_daemon.sh`'s `owned_pid` uses;
/// `data_dir` is tried as given and canonicalized.
pub fn argv_names_data_dir(argv: &str, data_dir: &Path) -> bool {
    let mut forms = vec![data_dir.to_path_buf()];
    if let Ok(canonical) = data_dir.canonicalize() {
        forms.push(canonical);
    }
    forms.iter().any(|dir| {
        let token = format!("--data-dir {}", dir.display());
        argv.match_indices(&token).any(|(at, _)| {
            let rest = &argv[at + token.len()..];
            rest.is_empty() || rest.starts_with(' ')
        })
    })
}

/// The pid of the sandbox daemon serving `socket`, or a refusal.
///
/// Why: the probe must never measure a live index (#9993 G6). G1's socket
/// client already refuses the live default socket; this also requires the
/// layout `scripts/sandbox_search_daemon.sh` creates.
/// What: `socket` must sit in `<sandbox>/data/`; `<sandbox>/sandbox.pid` must
/// hold a pid above 1 whose command line names `--data-dir <sandbox>/data`.
/// Anything else is an `Err` naming what failed.
/// Test: `sandbox_pid_requires_the_pidfile_and_a_matching_argv`.
pub fn sandbox_pid(socket: &Path) -> Result<u32, String> {
    let refuse = |why: String| format!("refusing {}: {why}", socket.display());
    let data_dir = socket
        .parent()
        .filter(|d| d.file_name() == Some(OsStr::new(SANDBOX_DATA_DIR)))
        .ok_or_else(|| refuse(format!("not in a sandbox `{SANDBOX_DATA_DIR}/` directory")))?;
    let sandbox = data_dir
        .parent()
        .ok_or_else(|| refuse("the data directory has no parent".into()))?;
    let pidfile = sandbox.join(SANDBOX_PID_FILE);
    let text = std::fs::read_to_string(&pidfile)
        .map_err(|e| refuse(format!("no sandbox pid file {}: {e}", pidfile.display())))?;
    let pid: u32 = text
        .trim()
        .parse()
        .map_err(|e| refuse(format!("{} holds {text:?}: {e}", pidfile.display())))?;
    if pid <= 1 {
        return Err(refuse(format!("{} holds pid {pid}", pidfile.display())));
    }
    let argv = process_argv(pid).map_err(refuse)?;
    if !argv_names_data_dir(&argv, data_dir) {
        return Err(refuse(format!(
            "pid {pid} does not run with --data-dir {}",
            data_dir.display()
        )));
    }
    Ok(pid)
}

fn bytes_row(metric: String, value: f64, notes: String) -> ResultRow {
    ResultRow {
        dimension: "storage".into(),
        metric,
        unit: "bytes".into(),
        n: Some(1),
        k: None,
        p50: None,
        p95: None,
        p99: None,
        max: None,
        value: Some(value),
        threshold: None,
        notes: Some(notes),
    }
}

/// Schema-1 rows for one probe: one per file, the total, bytes per chunk, RSS.
///
/// What: dimension `storage`, unit `bytes`, `n` 1 (one deterministic read).
/// File metrics read `file <layout>/<relative path>`. Zero chunks, or zero
/// total bytes, is an `Err`: an empty index is not a measurement.
/// Test: `storage_rows_carry_exact_bytes_and_refuse_an_empty_index`.
pub fn storage_rows(
    dirs: &[StorageDir],
    snaps: &[Snapshot],
    chunk_count: u64,
    rss: RssReading,
    pid: u32,
) -> Result<Vec<ResultRow>, String> {
    if dirs.len() != snaps.len() {
        return Err(format!("{} dirs but {} snapshots", dirs.len(), snaps.len()));
    }
    if chunk_count == 0 {
        return Err("the index reports 0 chunks; nothing to measure".into());
    }
    let mut rows = Vec::new();
    let (mut total, mut files) = (0u64, 0usize);
    for (dir, snap) in dirs.iter().zip(snaps) {
        for file in snap.files() {
            total += file.len;
            files += 1;
            rows.push(bytes_row(
                format!("file {}/{}", dir.layout, file.rel.display()),
                file.len as f64,
                dir.path.display().to_string(),
            ));
        }
    }
    if total == 0 {
        return Err(format!("{files} index files hold 0 bytes; the index is empty"));
    }
    rows.push(bytes_row(
        "index storage total".into(),
        total as f64,
        format!("{files} files in {} directories", dirs.len()),
    ));
    rows.push(bytes_row(
        "bytes per chunk".into(),
        total as f64 / chunk_count as f64,
        format!("total / chunk_count {chunk_count}"),
    ));
    rows.push(bytes_row(
        "daemon rss".into(),
        rss.bytes as f64,
        format!("pid {pid}, {}", rss.source),
    ));
    Ok(rows)
}

#[cfg(test)]
#[path = "storage_probe_tests.rs"]
mod tests;
