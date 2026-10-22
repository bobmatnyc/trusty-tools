//! The change log: `<root>/changes.jsonl`, one line per changed document.
//!
//! Why: DOC-82 LDG-2.8.3 wants an audit line for every document change, with
//! a strictly increasing `seq`, rotated by month with rotated files kept, so
//! `fsck` can find a lost line or a hand edit (#10063 AC 4). The log is not
//! the source of truth (`dashboard_jsonl.py`), so it is appended after the
//! commit, under the same lock. Every check that can fail without an I/O
//! fault runs before the commit, so a torn log refuses writes instead of
//! letting each one commit unlogged (#10063 df-G01).
//! What: [`prepare`], before anything is written, reads the last `seq` and
//! refuses a torn or corrupt last line, then rotates a log whose first line
//! is from an earlier month to `changes-YYYY-MM.jsonl`, refusing a bad `ts`
//! and an existing target (never overwritten). [`append`], after the commit,
//! writes one canonical line per dirty document of the batch, in
//! `(collection, id)` byte order: `{by, collection, id, keys, op, seq,
//! sha256, ts, version}`, where `sha256` hashes the bytes written (the
//! `jsonl` line or the `dir` file) and is null for a delete, and `keys` are
//! the top-level keys whose value changed; only its durable append can fail.
//! [`read_all`] reads every log (rotated first) for `fsck`, accepting the
//! prototype's `coll` key too. [`append_durable`] is the fsynced append the
//! prune log shares.
//! Test: `changes_seq_strictly_increases_across_rotation`,
//! `change_line_hashes_the_written_bytes`, `rotation_never_overwrites_a_rotated_file`,
//! `change_log_preconditions_refuse_the_batch`,
//! `change_log_failure_is_unlogged_after_commit`, `fsck_reports_torn_change_line`.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::super::canonical::compact;
use super::super::error::DashError;
use super::stage::{Slot, Staged};
use super::{Format, Store, dir, jsonl};
use crate::ledger::store::sync_dir;
use crate::sha256::sha256_hex;

/// The current log's file name under the store root.
pub const CHANGES: &str = "changes.jsonl";

/// How much of a log's end is read to find its last line.
const TAIL: u64 = 64 * 1024;

/// Checks, under the lock and before anything is written, that the log
/// can take a batch committed at `now`, and rotates it when its month has
/// passed; returns the newest `seq` (0 when there is none).
///
/// # Errors
/// [`DashError::Corrupt`] for a torn or `seq`-less last line, a bad
/// first-line `ts` or an existing rotation target; [`DashError::Io`] when a
/// log cannot be read or renamed. The batch is refused on any of these.
pub(super) fn prepare(root: &Path, now: &str) -> Result<u64, DashError> {
    // #10063 df-G01: these failures are deterministic, so they refuse the
    // batch; after the commit they made every later write `Unlogged`.
    let seq = last_seq(root)?;
    rotate(root, &root.join(CHANGES), now)?;
    Ok(seq)
}

/// Appends the change lines of `staged` (committed at `now`), numbered
/// after `seq`, the value [`prepare`] returned.
pub(super) fn append(
    store: &Store,
    staged: &Staged,
    now: &str,
    mut seq: u64,
) -> Result<(), DashError> {
    let mut dirty: Vec<&Slot> = staged.slots.iter().filter(|s| s.dirty).collect();
    if dirty.is_empty() {
        return Ok(());
    }
    dirty.sort_by(|a, b| (&a.coll, &a.id).cmp(&(&b.coll, &b.id)));
    let path = store.root.join(CHANGES);
    let mut text = String::new();
    for slot in dirty {
        seq += 1;
        text.push_str(&compact(&line(store, slot, seq, now)));
        text.push('\n');
    }
    append_durable(&path, &text)
}

/// One change line.
fn line(store: &Store, s: &Slot, seq: u64, now: &str) -> Value {
    let (keys, op, sha, version) = if s.exists {
        let bytes = match store.format {
            Format::Dir => dir::render(s.version, &s.data),
            Format::Jsonl => jsonl::render_line(&s.id, s.version, &s.data),
        };
        let old = s.old.as_ref().map(|o| &o.1);
        let keys = changed_keys(old, &s.data);
        (
            keys,
            s.op,
            Value::from(sha256_hex(bytes.as_bytes())),
            s.version,
        )
    } else {
        let version = s.old.as_ref().map_or(0, |o| o.0);
        (Vec::new(), "delete", Value::Null, version)
    };
    json!({
        "by": store.actor,
        "collection": s.coll,
        "id": s.id,
        "keys": keys,
        "op": op,
        "seq": seq,
        "sha256": sha,
        "ts": now,
        "version": version,
    })
}

/// Top-level keys whose value differs between `old` and `new`, sorted.
fn changed_keys(old: Option<&Map<String, Value>>, new: &Map<String, Value>) -> Vec<String> {
    let empty = Map::new();
    let old = old.unwrap_or(&empty);
    let mut keys: Vec<String> = old
        .keys()
        .chain(new.keys())
        .filter(|k| match (old.get(*k), new.get(*k)) {
            (Some(a), Some(b)) => compact(a) != compact(b),
            _ => true,
        })
        .cloned()
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

/// Every log under `root`: rotated ones by name (oldest month first), then
/// the current one, when present.
pub(super) fn logs(root: &Path) -> Result<Vec<PathBuf>, DashError> {
    let mut rotated: Vec<String> = super::list_names(root)?
        .into_iter()
        .filter(|n| {
            n.strip_suffix(".jsonl")
                .is_some_and(|s| s.starts_with("changes-") && jsonl::is_reserved(s))
        })
        .collect();
    rotated.sort();
    let mut out: Vec<PathBuf> = rotated.into_iter().map(|n| root.join(n)).collect();
    let current = root.join(CHANGES);
    if current.exists() {
        out.push(current);
    }
    Ok(out)
}

/// The `seq` of the newest line in any log; 0 when there is none.
fn last_seq(root: &Path) -> Result<u64, DashError> {
    for path in logs(root)?.iter().rev() {
        if let Some(text) = last_line(path)? {
            let seq = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.get("seq").and_then(Value::as_u64));
            return seq.ok_or_else(|| DashError::corrupt(path, "last line has no integer seq"));
        }
    }
    Ok(0)
}

/// The last line of `path` without its LF; `None` for an empty or absent
/// file. A file not ending in LF is torn and refused.
fn last_line(path: &Path) -> Result<Option<String>, DashError> {
    let mut f = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(DashError::io(path)(e)),
    };
    let len = f.metadata().map_err(DashError::io(path))?.len();
    let mut from = len.saturating_sub(TAIL);
    loop {
        let mut buf = Vec::new();
        f.seek(SeekFrom::Start(from))
            .and_then(|_| f.read_to_end(&mut buf))
            .map_err(DashError::io(path))?;
        let Some((last, body)) = buf.split_last() else {
            return Ok(None);
        };
        if *last != b'\n' {
            return Err(DashError::corrupt(
                path,
                "torn last line (no newline): fix it before the next write",
            ));
        }
        let start = match body.iter().rposition(|b| *b == b'\n') {
            Some(i) => i + 1,
            None if from == 0 => 0,
            None => {
                from = 0;
                continue;
            }
        };
        return String::from_utf8(body[start..].to_vec())
            .map(Some)
            .map_err(|e| DashError::corrupt(path, format!("last line is not UTF-8: {e}")));
    }
}

/// Renames `path` to `changes-<month>.jsonl` when its first line is from a
/// month before `now`'s. An existing rotated file is never overwritten.
fn rotate(root: &Path, path: &Path, now: &str) -> Result<(), DashError> {
    let first = match File::open(path) {
        Ok(f) => {
            let mut line = String::new();
            BufReader::new(f)
                .read_line(&mut line)
                .map_err(DashError::io(path))?;
            line
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(DashError::io(path)(e)),
    };
    if first.trim().is_empty() {
        return Ok(());
    }
    let ts = serde_json::from_str::<Value>(&first)
        .ok()
        .and_then(|v| v.get("ts").and_then(Value::as_str).map(str::to_owned))
        .ok_or_else(|| DashError::corrupt(path, "first line has no string ts"))?;
    let (Some(month), Some(this)) = (ts.get(..7), now.get(..7)) else {
        return Err(DashError::corrupt(path, format!("first line ts {ts:?}")));
    };
    let stem = format!("changes-{month}");
    if month == this {
        return Ok(());
    }
    if !jsonl::is_reserved(&stem) {
        return Err(DashError::corrupt(path, format!("first line ts {ts:?}")));
    }
    let target = root.join(format!("{stem}.jsonl"));
    if target.exists() {
        return Err(DashError::corrupt(
            &target,
            "rotated log already exists; it is never overwritten",
        ));
    }
    fs::rename(path, &target).map_err(DashError::io(path))?;
    sync_dir(&target).map_err(DashError::from)
}

/// Appends `text` to `path` (created with its directory), then fsyncs it.
pub(super) fn append_durable(path: &Path, text: &str) -> Result<(), DashError> {
    append_with(path, text, OpenOptions::new())
}

/// [`append_durable`], refusing a symlink at `path`'s last component
/// (`O_NOFOLLOW`): the open fails with [`DashError::Io`] and nothing is
/// written.
pub(super) fn append_durable_nofollow(path: &Path, text: &str) -> Result<(), DashError> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut opts = OpenOptions::new();
    // #10063 df-G01: a link planted after the path was checked is not followed.
    opts.custom_flags(libc::O_NOFOLLOW);
    append_with(path, text, opts)
}

fn append_with(path: &Path, text: &str, mut opts: OpenOptions) -> Result<(), DashError> {
    let existed = path.exists();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(DashError::io(parent))?;
    }
    let mut f = opts
        .create(true)
        .append(true)
        .open(path)
        .map_err(DashError::io(path))?;
    f.write_all(text.as_bytes())
        .and_then(|()| f.sync_all())
        .map_err(DashError::io(path))?;
    if !existed {
        sync_dir(path)?;
    }
    Ok(())
}

/// A change line as `fsck` reads it.
pub(super) struct Entry {
    pub(super) seq: u64,
    pub(super) op: String,
    pub(super) version: u64,
    pub(super) sha256: Option<String>,
}

/// The last change entry per `(collection, id)`.
pub(super) type LastEntries = BTreeMap<(String, String), Entry>;

/// The last entry per `(collection, id)` across every log, and the log's own
/// problems: unreadable or torn lines, and `seq` gaps or reversals.
///
/// # Errors
/// [`DashError::Io`] when a log cannot be read.
pub(super) fn read_all(root: &Path) -> Result<(LastEntries, Vec<String>), DashError> {
    let mut last = LastEntries::new();
    let mut problems = Vec::new();
    let mut prev: Option<u64> = None;
    for path in logs(root)? {
        let blob = fs::read(&path).map_err(DashError::io(&path))?;
        let text = String::from_utf8_lossy(&blob);
        let torn = !text.is_empty() && !text.ends_with('\n');
        let lines: Vec<&str> = text.split('\n').collect();
        for (n, raw) in lines.iter().enumerate() {
            let at = format!("{}:{}", path.display(), n + 1);
            if torn && n + 1 == lines.len() {
                problems.push(format!("{at}: torn last line (no newline)"));
                continue;
            }
            if raw.is_empty() {
                continue;
            }
            let Some(v) = serde_json::from_str::<Value>(raw)
                .ok()
                .filter(Value::is_object)
            else {
                problems.push(format!("{at}: unreadable change line"));
                continue;
            };
            let Some(seq) = v.get("seq").and_then(Value::as_u64) else {
                problems.push(format!("{at}: change line has no integer seq"));
                continue;
            };
            match prev {
                Some(p) if seq <= p => {
                    problems.push(format!("{at}: seq {seq} after {p}: not increasing"));
                }
                Some(p) if seq > p + 1 => problems.push(format!(
                    "{at}: seq {seq} follows {p}: {} missing",
                    seq - p - 1
                )),
                _ => {}
            }
            prev = Some(prev.map_or(seq, |p| p.max(seq)));
            let text_of = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
            let coll = text_of("collection").or_else(|| text_of("coll"));
            let (Some(coll), Some(id)) = (coll, text_of("id")) else {
                problems.push(format!("{at}: change line names no collection and id"));
                continue;
            };
            let entry = Entry {
                seq,
                op: text_of("op").unwrap_or_default(),
                version: v.get("version").and_then(Value::as_u64).unwrap_or(0),
                sha256: text_of("sha256"),
            };
            last.insert((coll, id), entry);
        }
    }
    Ok((last, problems))
}
