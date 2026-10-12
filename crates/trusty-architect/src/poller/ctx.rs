//! A PM pane's context percent from its own transcript.
//!
//! Why: after an auto-compact, in a narrow pane, or at `ctx>200k`, the status
//! line carries no `ctx N%`. Several PMs share one cwd, so the cwd cannot pick
//! the transcript (the 2026-10-05 "ctx 45%" x5 false alerts); Claude Code's
//! live session registry maps a tmux pane to its own session id (#9171 A3a).
//! What: [`pane_session_id`] reads `<sessions_dir>/*.json` and returns the
//! `sessionId` of the newest live entry whose `tmux` ends in `.<pane>`.
//! [`TranscriptCtx`] finds `<transcript_base>/*/<sessionId>.jsonl` (newest by
//! mtime), reads its last assistant usage, and caches the token count per
//! transcript keyed by `(mtime, size)`.
//! Test: `test_same_cwd_panes_get_their_own_ctx`,
//! `test_transcript_ctx_is_cached_until_the_file_changes`,
//! `transcript_ctx_matches_python_goldens`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

use super::config::Config;
use crate::classify::PaneInfo;
use crate::error::SelfCtxError;
use crate::self_ctx::{USAGE_TAIL_START, tail_last_assistant_usage, usage_tokens};

/// True when `pid` names a live process (`os.kill(pid, 0)`; EPERM is alive).
fn pid_alive(pid: i64) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks that the process exists.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Python `int(d["pid"])` for the value shapes the registry holds.
fn pid_of(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f.trunc() as i64)),
        Value::String(s) => super::pyfmt::py_int(s),
        Value::Bool(b) => Some(i64::from(*b)),
        _ => None,
    }
}

/// `(d.get("updatedAt") or 0)` as a number; `None` for a value Python could
/// not compare with a number.
fn updated_at(d: &serde_json::Map<String, Value>) -> Option<f64> {
    match d.get("updatedAt") {
        None | Some(Value::Null) => Some(0.0),
        Some(Value::Bool(b)) => Some(f64::from(u8::from(*b))),
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) if s.is_empty() => Some(0.0),
        Some(_) => None,
    }
}

/// `sessionId` of the live Claude process in tmux pane `pane` (`%N`), or `None`.
///
/// Newest `updatedAt` wins, a later directory entry winning a tie; entries
/// with a dead pid, a missing pid, or unreadable JSON are ignored.
pub fn pane_session_id(sessions_dir: &Path, pane: &str) -> Option<String> {
    let entries = std::fs::read_dir(sessions_dir).ok()?;
    let suffix = format!(".{pane}");
    let mut best: Option<(f64, Option<String>)> = None;
    for e in entries.filter_map(Result::ok) {
        if !e.file_name().to_string_lossy().ends_with(".json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(e.path()) else {
            continue;
        };
        let Ok(Value::Object(d)) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if !d
            .get("tmux")
            .and_then(Value::as_str)
            .is_some_and(|t| t.ends_with(&suffix))
        {
            continue;
        }
        let Some(pid) = d.get("pid").and_then(pid_of) else {
            continue;
        };
        let Some(updated) = updated_at(&d) else {
            continue;
        };
        if !pid_alive(pid) {
            continue;
        }
        if best.as_ref().is_none_or(|(b, _)| updated >= *b) {
            let sid = d
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_owned);
            best = Some((updated, sid));
        }
    }
    best.and_then(|(_, sid)| sid).filter(|s| !s.is_empty())
}

/// Cache key of one transcript: modification time and size.
type FileKey = (SystemTime, u64);

/// Per-transcript token counts across cycles (the prototype's
/// `_transcript_cache`), so a tick re-reads only a changed file.
#[derive(Debug, Default)]
pub struct TranscriptCtx {
    cache: HashMap<PathBuf, (FileKey, Option<i64>)>,
    reads: usize,
}

impl TranscriptCtx {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many transcripts were parsed (cache misses) so far.
    pub fn reads(&self) -> usize {
        self.reads
    }

    /// The newest `<base>/*/<sid>.jsonl` by mtime, skipping hidden folders.
    fn newest(base: &Path, sid: &str) -> Result<Option<(PathBuf, FileKey)>, SelfCtxError> {
        let Ok(entries) = std::fs::read_dir(base) else {
            return Ok(None);
        };
        let mut best: Option<(PathBuf, FileKey)> = None;
        for e in entries.filter_map(Result::ok) {
            if e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let path = e.path().join(format!("{sid}.jsonl"));
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let modified = meta.modified().map_err(|source| SelfCtxError::Io {
                path: path.clone(),
                source,
            })?;
            if best.as_ref().is_none_or(|(_, (m, _))| modified > *m) {
                best = Some((path, (modified, meta.len())));
            }
        }
        Ok(best)
    }

    /// Context percent of the Claude session in tmux pane `pane`, or `None`
    /// when its own transcript cannot be resolved (never another session's).
    ///
    /// # Errors
    /// [`SelfCtxError`] when the transcript cannot be read or its usage is not
    /// numeric; the prototype logs these and reads `None`.
    pub fn pct(&mut self, cfg: &Config, pane: &str) -> Result<Option<u64>, SelfCtxError> {
        let window = cfg.pane_ctx_window;
        let Some(sid) = pane_session_id(&cfg.sessions_dir, pane) else {
            return Ok(None);
        };
        if window <= 0 {
            return Ok(None);
        }
        let Some((path, key)) = Self::newest(&cfg.transcript_base, &sid)? else {
            return Ok(None);
        };
        let tokens = match self.cache.get(&path) {
            Some((k, tokens)) if *k == key => *tokens,
            _ => {
                self.reads += 1;
                let tokens = match tail_last_assistant_usage(&path, USAGE_TAIL_START)? {
                    Some(usage) => Some(usage_tokens(&usage)?),
                    None => None,
                };
                self.cache.insert(path, (key, tokens));
                tokens
            }
        };
        // Python's int(100 * tokens / window) truncates toward zero.
        Ok(tokens
            .and_then(|t| u64::try_from((100.0 * t as f64 / window as f64).trunc() as i64).ok()))
    }

    /// The prototype's fallback in `cycle`: when the status line had no
    /// `ctx N%`, take it from the pane's own transcript. On an error the ctx
    /// stays `None` and the error is returned for the caller to log.
    ///
    /// # Errors
    /// As [`TranscriptCtx::pct`].
    pub fn fill_ctx(
        &mut self,
        cfg: &Config,
        info: &mut PaneInfo,
        pane: &str,
    ) -> Result<(), SelfCtxError> {
        if info.ctx.is_none() {
            info.ctx = self.pct(cfg, pane)?;
        }
        Ok(())
    }
}
