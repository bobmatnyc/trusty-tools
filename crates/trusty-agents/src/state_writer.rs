//! Concurrency-safe state file writer (#198).
//!
//! Why: Multiple `trusty-agents` binary instances may run simultaneously — a durable
//! API server on `:7654`, a Tauri GUI, and a `cargo run` source build can all
//! share `~/.trusty-agents/`. Direct `fs::write` / append-mode `OpenOptions` calls
//! on the same path race: one process can truncate or partially-write a file
//! while another is reading it, corrupting the on-disk JSON / NDJSON. We
//! already use `fs4` advisory locks for the code memory store
//! (`memory::code_store`); this module extends the same pattern to all
//! state files (sessions, skill index, interaction log, perf, processes).
//! What: Three helpers — `atomic_write` (lock + tmp + rename, suitable for
//! whole files like `skill_index.json`), `atomic_update` (lock + read + decide
//! + write, for a state file whose new value DEPENDS on its old one), and
//! `atomic_append_line` (lock + append, suitable for NDJSON logs). All three
//! use a sibling `<path>.lock` file as the advisory-lock target so the data
//! file itself is never opened just to take a lock; this avoids the "open
//! exclusively-locked-by-other-process file" failure mode on Windows and keeps
//! the lock semantics symmetric across readers/writers.
//! Test: `atomic_write_is_crash_safe`, `concurrent_writes_no_corruption`,
//! `atomic_append_line_multiple_writers`,
//! `atomic_update_serializes_read_modify_write`.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use fs4::FileExt;

/// Resolve `<path>.lock` — the sibling lock file paired with a state file.
///
/// Why: Locking the data file itself would force every reader to open it for
/// write (to acquire the lock), which is awkward for read-only consumers and
/// platform-fragile. A sibling `.lock` file is purely an advisory rendezvous
/// point; readers and writers both `open + lock` the same path.
/// What: Appends `.lock` to the OS-string form of `path`. For
/// `~/.trusty-agents/state/runs.jsonl` returns `~/.trusty-agents/state/runs.jsonl.lock`.
fn lock_path_for(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".lock");
    PathBuf::from(s)
}

/// Open (or create) the sibling lock file for `path`.
///
/// Why: `fs4`'s lock methods need an open `File`. Centralizing the open-with-
/// create logic here keeps the caller code (atomic_write, atomic_append_line)
/// focused on the actual write semantics.
/// What: Ensures the parent dir exists, then opens `<path>.lock` with
/// `create + write` (no truncation). The file is ordinarily empty — we never
/// write to it.
fn open_lock_file(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating parent dir {}", parent.display()))?;
    }
    let lock_path = lock_path_for(path);
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("opening lock file {}", lock_path.display()))
}

/// Write `contents` to `path` atomically under an advisory exclusive lock.
///
/// Why: `fs::write` on a state file is unsafe across multiple processes — a
/// reader can observe a partially-written buffer and a second writer can
/// truncate the first writer's bytes. The lock + tmp + rename combo gives
/// us:
///   1. Mutual exclusion across processes (advisory lock)
///   2. All-or-nothing semantics for readers (rename is atomic on the same
///      filesystem; readers either see the old file or the new one, never a
///      half-written buffer)
///   3. Crash safety (a crash mid-write leaves `<path>.tmp` orphaned but the
///      target file untouched).
/// What: Acquires an exclusive lock on `<path>.lock`, writes `contents` to
/// `<path>.tmp`, fsyncs (best-effort), then `fs::rename`s `<path>.tmp` to
/// `<path>`. Lock is released via explicit `unlock` (with drop as backstop).
/// Test: `atomic_write_is_crash_safe`, `concurrent_writes_no_corruption`.
pub fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    let lock_file = open_lock_file(path)?;
    FileExt::lock(&lock_file).map_err(|e| anyhow!("acquiring exclusive write lock: {e}"))?;

    // Inner block so we always unlock even on early return.
    let result = write_tmp_and_rename(path, contents);

    let _ = FileExt::unlock(&lock_file);
    result
}

/// The tmp-write + rename half of [`atomic_write`], with NO locking.
///
/// Why: [`atomic_update`] needs the same publish step while already holding the
/// lock, and taking the lock twice would deadlock. Splitting it out is what
/// lets both entry points share one definition of "publish these bytes".
/// What: writes `contents` to `<path>.tmp`, fsyncs best-effort, renames onto
/// `path`. The caller MUST hold the advisory lock.
fn write_tmp_and_rename(path: &Path, contents: &[u8]) -> Result<()> {
    // Ensure the parent directory exists for the data file too (it's the
    // same dir as the lock, but be explicit).
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating parent dir {}", parent.display()))?;
    }
    let tmp = {
        let mut s = path.as_os_str().to_owned();
        s.push(".tmp");
        PathBuf::from(s)
    };
    {
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)
            .with_context(|| format!("opening tmp file {}", tmp.display()))?;
        f.write_all(contents)
            .with_context(|| format!("writing tmp file {}", tmp.display()))?;
        // Best-effort fsync so the rename can't expose an empty file on
        // crash; ignore platform errors.
        let _ = f.sync_all();
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Read `path`, decide the new contents from the old, and write — all under a
/// SINGLE held exclusive lock (#4683).
///
/// Why: [`atomic_write`] makes each individual write atomic, but a caller whose
/// new value depends on the current one (a counter, a high-water mark, a
/// monotonic timestamp) has to read first — and a read outside the lock is a
/// lost update waiting to happen. Two processes can both read the old value,
/// both decide they may write, and the later write clobbers the earlier one's
/// effect. Holding the lock across read + decide + write closes that window,
/// which is the only way a "never goes backwards" invariant can actually hold
/// across the GUI / `--api` sidecar / REPL processes that share
/// `~/.trusty-agents/`.
/// What: acquires the exclusive lock on `<path>.lock`, reads `path` (`None`
/// when it does not exist), hands the bytes to `update`, and publishes the
/// returned bytes via tmp + rename. `update` returning `Ok(None)` means "leave
/// the file alone" and the whole call is a no-op; the return value says whether
/// a write happened. An `Err` from `update` propagates and writes nothing.
///
/// `update` runs with the lock HELD, so it must be short and must not itself
/// call back into this module for the same path.
/// Test: `atomic_update_serializes_read_modify_write`,
/// `atomic_update_skips_the_write_when_the_closure_declines`.
pub fn atomic_update<F>(path: &Path, update: F) -> Result<bool>
where
    F: FnOnce(Option<&[u8]>) -> Result<Option<Vec<u8>>>,
{
    let lock_file = open_lock_file(path)?;
    FileExt::lock(&lock_file).map_err(|e| anyhow!("acquiring exclusive update lock: {e}"))?;

    let result = (|| -> Result<bool> {
        let existing = match std::fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(e).with_context(|| format!("reading {} for update", path.display()));
            }
        };
        match update(existing.as_deref())? {
            Some(contents) => write_tmp_and_rename(path, &contents).map(|()| true),
            None => Ok(false),
        }
    })();

    let _ = FileExt::unlock(&lock_file);
    result
}

/// Append `line` (plus a trailing newline) to `path` under an advisory
/// exclusive lock.
///
/// Why: NDJSON logs (`runs.jsonl`, `interactions.jsonl`) tolerate concurrent
/// appenders at line granularity — but only when each writer's `write_all`
/// is atomic with respect to others. Without a lock, two simultaneous
/// `OpenOptions::append` writes can interleave bytes within a single line
/// and produce unparseable JSONL. Holding the advisory lock for the duration
/// of the write serializes the appends without forcing the caller to batch.
/// What: Acquires exclusive lock on `<path>.lock`, opens `path` in
/// create+append mode, writes `line` + `\n`, releases the lock. The caller
/// supplies a single line WITHOUT the trailing newline.
/// Test: `atomic_append_line_multiple_writers`.
pub fn atomic_append_line(path: &Path, line: &str) -> Result<()> {
    let lock_file = open_lock_file(path)?;
    FileExt::lock(&lock_file).map_err(|e| anyhow!("acquiring exclusive append lock: {e}"))?;

    let result = (|| -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating parent dir {}", parent.display()))?;
        }
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("opening append target {}", path.display()))?;
        f.write_all(line.as_bytes())
            .with_context(|| format!("appending to {}", path.display()))?;
        f.write_all(b"\n")
            .with_context(|| format!("appending newline to {}", path.display()))?;
        Ok(())
    })();

    let _ = FileExt::unlock(&lock_file);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;

    /// Why: A successful `atomic_write` must leave the target file with the
    /// final contents and clean up the tmp file. This is the happy-path
    /// baseline — without it the more complex concurrency tests are
    /// meaningless.
    /// What: Writes a JSON blob, asserts the file exists with the expected
    /// content, asserts the `.tmp` sibling is gone.
    #[test]
    fn atomic_write_is_crash_safe() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("state.json");
        atomic_write(&path, br#"{"hello":"world"}"#).unwrap();
        let read = std::fs::read_to_string(&path).unwrap();
        assert_eq!(read, r#"{"hello":"world"}"#);
        // tmp file cleaned up by the rename.
        let tmp_path = {
            let mut s = path.as_os_str().to_owned();
            s.push(".tmp");
            PathBuf::from(s)
        };
        assert!(!tmp_path.exists(), "tmp file should have been renamed away");
    }

    /// Why: Locks make sure simultaneous writers don't truncate each other's
    /// output mid-write. Without the lock, the resulting file might be a
    /// partial JSON document with a smaller-than-expected payload. We verify
    /// the final file is parseable and matches *one* of the writers exactly.
    /// What: Spawns 10 OS threads each calling `atomic_write` with a distinct
    /// JSON blob. After the join, the file must be exactly one of the 10
    /// payloads (no interleaving, no partial writes).
    #[test]
    fn concurrent_writes_no_corruption() {
        let tmp = TempDir::new().unwrap();
        let path = Arc::new(tmp.path().join("state.json"));
        let mut handles = Vec::new();
        let mut expected: Vec<String> = Vec::new();
        for i in 0..10 {
            let payload = format!(r#"{{"writer":{i},"data":"{}"}}"#, "x".repeat(200));
            expected.push(payload.clone());
            let p = Arc::clone(&path);
            handles.push(std::thread::spawn(move || {
                atomic_write(&p, payload.as_bytes()).unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let final_text = std::fs::read_to_string(&*path).unwrap();
        // The final file must equal one of the writers' payloads exactly.
        assert!(
            expected.iter().any(|e| e == &final_text),
            "final file content does not match any single writer's payload — corruption detected: {final_text:?}"
        );
        // And it must be valid JSON.
        let _: serde_json::Value =
            serde_json::from_str(&final_text).expect("final file must be valid JSON");
    }

    /// Why: NDJSON appenders can interleave bytes within one line if the
    /// underlying `write_all` isn't serialized. We assert that with the lock,
    /// 200 concurrent appends (2 threads × 100 lines) yield exactly 200
    /// parseable JSON lines — no truncated, merged, or duplicated rows.
    /// What: Two threads append 100 distinct JSON lines each. Afterwards we
    /// count lines, parse each as JSON, and verify the multiset matches.
    #[test]
    fn atomic_append_line_multiple_writers() {
        let tmp = TempDir::new().unwrap();
        let path = Arc::new(tmp.path().join("log.jsonl"));
        let mut handles = Vec::new();
        for w in 0..2 {
            let p = Arc::clone(&path);
            handles.push(std::thread::spawn(move || {
                for i in 0..100 {
                    let line = format!(r#"{{"writer":{w},"i":{i}}}"#);
                    atomic_append_line(&p, &line).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let text = std::fs::read_to_string(&*path).unwrap();
        let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 200, "expected 200 lines, got {}", lines.len());
        // Every line must parse as JSON (proves no byte-level interleaving).
        for line in &lines {
            let v: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("unparseable line {line:?}: {e}"));
            assert!(v.get("writer").is_some());
            assert!(v.get("i").is_some());
        }
        // All 200 (writer, i) pairs must be present exactly once.
        let mut seen: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();
        for line in &lines {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            let w = v["writer"].as_u64().unwrap();
            let i = v["i"].as_u64().unwrap();
            assert!(seen.insert((w, i)), "duplicate row ({w},{i})");
        }
        assert_eq!(seen.len(), 200);
    }

    /// Why: `atomic_update` exists to make a read-modify-write sequence safe
    /// across concurrent writers (#4683). A lost update — two writers both
    /// reading the same old value and both incrementing it — is exactly what
    /// `atomic_write` plus a separate read cannot prevent. Counting to 400
    /// from 8 threads is the cheapest way to detect one.
    /// What: 8 threads × 50 increments of a counter stored in the file. With
    /// the read held under the lock the final value must be exactly 400; a
    /// lost update shows up as any smaller number.
    #[test]
    fn atomic_update_serializes_read_modify_write() {
        let tmp = TempDir::new().unwrap();
        let path = Arc::new(tmp.path().join("counter.txt"));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let p = Arc::clone(&path);
            handles.push(std::thread::spawn(move || {
                for _ in 0..50 {
                    atomic_update(&p, |existing| {
                        let current: u64 = match existing {
                            Some(bytes) => std::str::from_utf8(bytes)?.trim().parse()?,
                            None => 0,
                        };
                        Ok(Some((current + 1).to_string().into_bytes()))
                    })
                    .unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let final_value: u64 = std::fs::read_to_string(&*path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            final_value, 400,
            "lost update: increments were not serialized under the lock"
        );
    }

    /// Why: "leave the file alone" has to be a first-class outcome — a
    /// monotonic guard that rejects an out-of-order value must not rewrite the
    /// file with what is already there, and must report that it did nothing.
    /// What: an update returning `Ok(None)` leaves the existing bytes untouched
    /// and returns `Ok(false)`; on a missing file it creates nothing.
    #[test]
    fn atomic_update_skips_the_write_when_the_closure_declines() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("state.json");

        assert!(!atomic_update(&path, |_| Ok(None)).unwrap());
        assert!(!path.exists(), "a declined update created the file anyway");

        assert!(
            atomic_update(&path, |existing| {
                assert!(existing.is_none());
                Ok(Some(b"first".to_vec()))
            })
            .unwrap()
        );
        assert!(
            !atomic_update(&path, |existing| {
                assert_eq!(existing, Some(&b"first"[..]));
                Ok(None)
            })
            .unwrap()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");
    }

    /// Why: An exclusive lock must NOT prevent the same process from later
    /// re-acquiring the lock (drop releases it), otherwise repeat writes in
    /// one binary deadlock.
    /// What: Calls atomic_write twice in succession on the same path.
    #[test]
    fn atomic_write_releases_lock_for_followup_writes() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("state.json");
        atomic_write(&path, b"first").unwrap();
        atomic_write(&path, b"second").unwrap();
        atomic_write(&path, b"third").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "third");
    }
}
