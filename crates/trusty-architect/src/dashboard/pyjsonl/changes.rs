//! `dashboard_jsonl.py`'s change log: `<root>/changes.jsonl`, appended, rotated monthly (#12029 P2b).
//!
//! Why: #10225: one torn or oversized last line must not block every later
//! append, so the reader scans back to the newest valid entry, however long
//! the lines after it are.
//! What: [`append_changes`] continues `seq` from the newest valid entry,
//! terminates a torn tail, rotates a log whose first valid entry is from an
//! earlier month (`os.replace`, which overwrites), appends one canonical line
//! per entry and syncs it; it returns the warning Python writes to stderr.
//! [`scan`] reads every line for [`read_changes`] and `fsck`.
//! Every path is `root` joined with a fixed name: nothing else is written.
//! Known gap: a line is decoded as UTF-8 with a UTF-8 BOM stripped (what
//! `json.loads(bytes)` does for those); the UTF-16/32 detection and the
//! `surrogatepass` decoding of `json.loads(bytes)` are not reproduced, so such
//! a line counts as invalid.
//! Test: `dashboard_jsonl_recordings_replay_clean_against_the_port` (`append`,
//! `read`), `append_writes_only_under_the_root`.

use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};

use super::{JsonlError, canonical, join};
use crate::pyjson_load::{PyInt, PyValue, loads};
use crate::pyval::{get, int_text, py_str};
use crate::spec_drift::py::os_error;

/// The current log's file name (`CHANGES`).
pub(crate) const CHANGES: &str = "changes.jsonl";
const TAIL_CHUNK: u64 = 8192;

fn os(path: &str) -> impl Fn(std::io::Error) -> JsonlError + '_ {
    move |e| JsonlError::Os(os_error(path.as_ref(), &e).msg)
}

/// `_change_entry(raw)`: the entry when the line is a JSON object with an
/// integer (not `bool`) `seq`.
pub(crate) fn change_entry(raw: &[u8]) -> Option<PyValue> {
    let raw = raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(raw);
    let entry = loads(std::str::from_utf8(raw).ok()?).ok()?;
    matches!(get(&entry, "seq"), Some(PyValue::Int(_))).then_some(entry)
}

/// `bytes.strip()` is empty: only ASCII whitespace.
fn blank(raw: &[u8]) -> bool {
    raw.iter().all(|b| b" \t\n\r\x0b\x0c".contains(b))
}

/// `_read_tail(path)`: `(newest valid entry, torn, invalid lines after it)`.
fn read_tail(path: &str) -> Result<(Option<PyValue>, bool, usize), JsonlError> {
    let mut f = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((None, false, 0)),
        Err(e) => return Err(os(path)(e)),
    };
    let mut pos = f.seek(SeekFrom::End(0)).map_err(os(path))?;
    if pos == 0 {
        return Ok((None, false, 0));
    }
    let mut last = [0u8; 1];
    f.seek(SeekFrom::Start(pos - 1)).map_err(os(path))?;
    f.read_exact(&mut last).map_err(os(path))?;
    let torn = last[0] != b'\n';
    let (mut carry, mut skipped) = (Vec::new(), 0);
    while pos > 0 {
        let step = TAIL_CHUNK.min(pos);
        pos -= step;
        f.seek(SeekFrom::Start(pos)).map_err(os(path))?;
        let mut chunk = vec![0u8; usize::try_from(step).unwrap_or(0)];
        f.read_exact(&mut chunk).map_err(os(path))?;
        chunk.extend_from_slice(&carry);
        let mut parts: Vec<&[u8]> = chunk.split(|b| *b == b'\n').collect();
        let head = parts.remove(0);
        carry = head.to_vec();
        if pos == 0 {
            parts.insert(0, head);
        }
        for raw in parts.into_iter().rev() {
            if blank(raw) {
                continue;
            }
            if let Some(entry) = change_entry(raw) {
                return Ok((Some(entry), torn, skipped));
            }
            skipped += 1;
        }
    }
    Ok((None, torn, skipped))
}

/// `for raw in f`: binary lines, each with its `\n`.
fn lines(blob: &[u8]) -> impl Iterator<Item = &[u8]> {
    blob.split_inclusive(|b| *b == b'\n')
}

fn read_opt(path: &str) -> Result<Option<Vec<u8>>, JsonlError> {
    match fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(os(path)(e)),
    }
}

/// `_read_first_entry(path)`.
fn read_first_entry(path: &str) -> Result<Option<PyValue>, JsonlError> {
    let Some(blob) = read_opt(path)? else {
        return Ok(None);
    };
    Ok(lines(&blob)
        .filter(|raw| !blank(raw))
        .find_map(change_entry))
}

fn seq_of(entry: Option<&PyValue>) -> PyInt {
    match entry.and_then(|e| get(e, "seq")) {
        Some(PyValue::Int(i)) => i.clone(),
        _ => PyInt::Small(0),
    }
}

/// `seq + 1`, past `i64` as decimal text.
pub(crate) fn bump(seq: &PyInt) -> PyInt {
    match seq {
        PyInt::Small(n) => n.checked_add(1).map_or_else(
            || PyInt::Big(format!("{}", i128::from(*n) + 1)),
            PyInt::Small,
        ),
        PyInt::Big(t) => PyInt::Big(match t.strip_prefix('-') {
            Some(mag) => format!("-{}", step_digits(mag, false)),
            None => step_digits(t, true),
        }),
    }
}

/// A decimal magnitude plus or minus one (a `Big` magnitude is never 0).
fn step_digits(mag: &str, up: bool) -> String {
    let mut d: Vec<u8> = mag.bytes().collect();
    let (from, to, carry) = if up {
        (b'9', b'0', b'1')
    } else {
        (b'0', b'9', b'0')
    };
    let mut i = d.len();
    while i > 0 && d[i - 1] == from {
        d[i - 1] = to;
        i -= 1;
    }
    match i {
        0 => d.insert(0, carry),
        _ => d[i - 1] = if up { d[i - 1] + 1 } else { d[i - 1] - 1 },
    }
    let s = String::from_utf8(d).unwrap_or_default();
    let trimmed = s.trim_start_matches('0');
    if trimmed.is_empty() {
        "0".into()
    } else {
        trimmed.into()
    }
}

/// `str(x)[:7]`: the first seven code points.
fn month(v: &str) -> String {
    v.chars().take(7).collect()
}

/// `dict(e, seq=seq, ts=ts)`: the entry's pairs with `seq` and `ts` set in place or appended.
fn stamped(e: &PyValue, seq: &PyInt, ts: &str) -> PyValue {
    let mut pairs = match e {
        PyValue::Dict(p) => p.clone(),
        _ => Vec::new(),
    };
    for (k, v) in [
        ("seq", PyValue::Int(seq.clone())),
        ("ts", PyValue::Str(ts.to_owned())),
    ] {
        match pairs.iter_mut().find(|(x, _)| x == k) {
            Some(slot) => slot.1 = v,
            None => pairs.push((k.to_owned(), v)),
        }
    }
    PyValue::Dict(pairs)
}

/// What [`append_changes`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Appended {
    /// The return value: lines written, a torn-tail terminator included.
    pub(crate) written: usize,
    /// The `dashboard-jsonl: ...` warning Python writes to stderr.
    pub(crate) warning: Option<String>,
}

/// Why: the audit log must keep taking appends after a crash tore its tail (#10225).
/// What: `append_changes(root, entries, ts)`; see the module doc. `ts` is the
/// caller's ISO UTC string; the clock is never read.
/// Test: `dashboard_jsonl_recordings_replay_clean_against_the_port` (`append`),
/// `append_writes_only_under_the_root`.
///
/// # Errors
/// [`JsonlError::Os`] when the log cannot be read, renamed or appended.
pub(crate) fn append_changes(
    root: &str,
    entries: &[PyValue],
    ts: &str,
) -> Result<Appended, JsonlError> {
    if entries.is_empty() {
        return Ok(Appended {
            written: 0,
            warning: None,
        });
    }
    let path = join(root, CHANGES);
    let (last, torn, skipped) = read_tail(&path)?;
    let mut seq = seq_of(last.as_ref());
    let warning = (torn || skipped > 0).then(|| {
        format!(
            "dashboard-jsonl: {path}: corrupt line(s) at the end of the change log \
             ({skipped} invalid, torn={}); appending after them, seq continues from {}; run fsck\n",
            if torn { "True" } else { "False" },
            int_text(&seq)
        )
    });
    let first = read_first_entry(&path)?;
    let first_ts = first
        .as_ref()
        .map(|f| month(&get(f, "ts").map_or(String::new(), py_str)));
    let rotated = first_ts
        .as_ref()
        .is_some_and(|m| !m.is_empty() && *m != month(ts));
    if let (true, Some(m)) = (rotated, &first_ts) {
        let target = join(root, &format!("changes-{m}.jsonl"));
        fs::rename(&path, &target).map_err(os(&path))?;
    }
    let mut text = String::from(if torn && !rotated { "\n" } else { "" });
    let mut written = usize::from(torn && !rotated);
    for e in entries {
        seq = bump(&seq);
        text.push_str(&canonical(&stamped(e, &seq, ts))?);
        text.push('\n');
        written += 1;
    }
    let mut f = OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)
        .map_err(os(&path))?;
    f.write_all(text.as_bytes()).map_err(os(&path))?;
    f.sync_all().map_err(os(&path))?;
    Ok(Appended { written, warning })
}

/// `_scan_changes(root)`: the valid entries in order, and one problem per invalid line.
///
/// # Errors
/// [`JsonlError::Os`] when the log exists but cannot be read.
pub(crate) fn scan(root: &str) -> Result<(Vec<PyValue>, Vec<String>), JsonlError> {
    let path = join(root, CHANGES);
    let (mut out, mut problems) = (Vec::new(), Vec::new());
    let Some(blob) = read_opt(&path)? else {
        return Ok((out, problems));
    };
    for (n, raw) in lines(&blob).enumerate() {
        if blank(raw) {
            continue;
        }
        match change_entry(raw) {
            Some(e) => out.push(e),
            None => problems.push(format!(
                "{path}:{}: invalid change line (torn, not JSON, or no integer seq)",
                n + 1
            )),
        }
    }
    Ok((out, problems))
}

/// `read_changes(root)`: every entry of `changes.jsonl` in order.
///
/// # Errors
/// [`JsonlError::Bad`] naming the first invalid line.
pub(crate) fn read_changes(root: &str) -> Result<Vec<PyValue>, JsonlError> {
    let (out, problems) = scan(root)?;
    match problems.into_iter().next() {
        Some(p) => Err(JsonlError::Bad(p)),
        None => Ok(out),
    }
}
