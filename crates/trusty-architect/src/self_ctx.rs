//! The Architect's own context meter: the port of the prototype's `self-ctx.py`.
//!
//! Why: the Architect's tmux pane carries no `ctx N%` field, so its context is
//! read from the newest interactive Claude Code transcript instead. The same
//! directory fills with short-lived `claude -p` transcripts (entrypoint
//! `sdk-cli`), which must never be the one measured (#9171 A2).
//! What: [`newest_transcript`] picks the newest `*.jsonl` (by mtime, among the
//! `limit` newest) whose newest line carries `"entrypoint": "cli"`;
//! [`tail_last_assistant_usage`] reads a transcript from the tail, growing the
//! window x8 until a line matches; [`self_ctx_tokens`] sums `input_tokens`,
//! `cache_read_input_tokens` and `cache_creation_input_tokens` of the last
//! assistant usage. Directories and the home path are passed in; nothing here
//! reads the environment.
//! Test: `tests/self_ctx_ported.rs`, `self_ctx_matches_python_goldens`.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

use crate::error::SelfCtxError;
use crate::pystr::{splitlines, strip};

/// First tail read for usage; grows if not found in it.
pub const USAGE_TAIL_START: u64 = 65_536;
/// The entrypoint is on nearly every line; a small tail suffices.
pub const ENTRYPOINT_TAIL_START: u64 = 4_096;
/// Only this many newest-by-mtime files are considered.
pub const CANDIDATE_LIMIT: usize = 20;

/// `<home>/.trusty-tools/trusty-mpm/claude-config/projects`, the prototype's
/// `PROJECTS_DIR` with `~` given explicitly.
pub fn projects_dir(home: &Path) -> PathBuf {
    home.join(".trusty-tools/trusty-mpm/claude-config/projects")
}

/// Claude Code's transcript directory for project `root`: its absolute path
/// with every `/` and `.` replaced by `-` (`/a/.b` becomes `-a--b`).
pub fn transcript_dir_for(projects_dir: &Path, root: &str) -> PathBuf {
    projects_dir.join(root.replace(['/', '.'], "-"))
}

/// The prototype's `DEFAULT_DIR`: the transcript directory of `repo_root`
/// under `home`'s projects directory.
pub fn default_dir(home: &Path, repo_root: &str) -> PathBuf {
    transcript_dir_for(&projects_dir(home), repo_root)
}

/// Python truthiness of a JSON value.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Reads `path` from the tail, newest line first, until `want` returns a
/// truthy value; the window starts at `start` bytes and grows x8 up to the
/// whole file. Malformed lines, and lines that are not JSON objects, are
/// skipped (the prototype raises `AttributeError` on the latter).
fn scan_tail(
    path: &Path,
    start: u64,
    want: impl Fn(&serde_json::Map<String, Value>) -> Option<Value>,
) -> std::io::Result<Option<Value>> {
    let size = std::fs::metadata(path)?.len();
    if size == 0 {
        return Ok(None);
    }
    let mut window = start.min(size);
    loop {
        let mut f = File::open(path)?;
        if window < size {
            // `window < size <= i64::MAX` for any real file.
            f.seek(SeekFrom::End(-i64::try_from(window).unwrap_or(i64::MAX)))?;
        }
        let mut data = Vec::new();
        f.read_to_end(&mut data)?;
        let text = String::from_utf8_lossy(&data);
        let mut lines = splitlines(&text);
        if window < size && !lines.is_empty() {
            lines.remove(0); // the first line of a tail read may be partial
        }
        for line in lines
            .iter()
            .rev()
            .map(|l| strip(l))
            .filter(|l| !l.is_empty())
        {
            let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if let Some(v) = want(&obj).filter(truthy) {
                return Ok(Some(v));
            }
        }
        if window >= size {
            return Ok(None);
        }
        window = window.saturating_mul(8).min(size);
    }
}

/// The `entrypoint` of the newest line that has one (`cli` interactive,
/// `sdk-cli` for `claude -p`).
fn entrypoint(path: &Path) -> std::io::Result<Option<Value>> {
    scan_tail(path, ENTRYPOINT_TAIL_START, |o| {
        o.get("entrypoint").cloned()
    })
}

/// The newest `*.jsonl` in `directory` whose entrypoint is `cli`, examining
/// only the `limit` newest by mtime. `None` for a missing or empty directory.
///
/// Test: `test_newest_transcript_skips_a_newer_sdk_cli_file_for_an_older_cli_one`,
/// `test_newest_transcript_bounds_the_files_it_examines`.
pub fn newest_transcript(directory: &Path, limit: usize) -> Option<PathBuf> {
    let entries = std::fs::read_dir(directory).ok()?;
    let mut paths: Vec<(SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.ends_with(".jsonl"))
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    // Newest first; a stable sort keeps directory order for equal mtimes.
    paths.sort_by_key(|p| std::cmp::Reverse(p.0));
    paths
        .into_iter()
        .take(limit)
        .map(|(_, p)| p)
        .find(|p| matches!(entrypoint(p), Ok(Some(Value::String(ref s))) if s == "cli"))
}

/// The last assistant `message.usage` value in `path`, read from the tail.
///
/// # Errors
/// [`SelfCtxError::Io`] when `path` cannot be read.
pub fn tail_last_assistant_usage(path: &Path, start: u64) -> Result<Option<Value>, SelfCtxError> {
    let want = |o: &serde_json::Map<String, Value>| {
        if o.get("type").and_then(Value::as_str) != Some("assistant") {
            return None;
        }
        match o.get("message") {
            Some(Value::Object(m)) => m.get("usage").cloned(),
            _ => None,
        }
    };
    scan_tail(path, start, want).map_err(|source| SelfCtxError::Io {
        path: path.to_owned(),
        source,
    })
}

/// One usage field as Python's `(usage.get(k) or 0)` reads it.
fn usage_field(usage: &serde_json::Map<String, Value>, key: &str) -> Result<i64, SelfCtxError> {
    match usage.get(key) {
        Some(v) if truthy(v) => match v {
            Value::Bool(true) => Ok(1),
            Value::Number(n) => n
                .as_i64()
                .ok_or_else(|| SelfCtxError::BadUsage(key.to_owned())),
            _ => Err(SelfCtxError::BadUsage(key.to_owned())),
        },
        _ => Ok(0),
    }
}

/// input + cache_read + cache_creation tokens from the newest interactive
/// transcript's last assistant usage; `Ok(None)` when there is nothing to
/// measure (no directory, no `cli` transcript, no assistant usage line).
///
/// # Errors
/// [`SelfCtxError::Io`] when the chosen transcript cannot be read, and
/// [`SelfCtxError::BadUsage`] when a usage field is not an integer (the
/// prototype raises `TypeError`/`AttributeError` in both cases).
pub fn self_ctx_tokens(directory: &Path) -> Result<Option<i64>, SelfCtxError> {
    let Some(path) = newest_transcript(directory, CANDIDATE_LIMIT) else {
        return Ok(None);
    };
    let Some(usage) = tail_last_assistant_usage(&path, USAGE_TAIL_START)? else {
        return Ok(None);
    };
    usage_tokens(&usage).map(Some)
}

/// input + cache_read + cache_creation tokens of one usage object.
///
/// # Errors
/// [`SelfCtxError::BadUsage`] when `usage` is not an object or a field is not
/// an integer. Also used by the pane transcript reader (#9171 A3a).
pub(crate) fn usage_tokens(usage: &Value) -> Result<i64, SelfCtxError> {
    let Value::Object(usage) = usage else {
        return Err(SelfCtxError::BadUsage("usage".to_owned()));
    };
    let keys = [
        "input_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
    ];
    keys.iter().try_fold(
        0i64,
        |sum, k| Ok(sum.saturating_add(usage_field(usage, k)?)),
    )
}
