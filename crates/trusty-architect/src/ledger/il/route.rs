//! Which writer applies a checked IL block: the in-process Rust writer or
//! `ilio.py apply` (G0b gap ruling).
//!
//! Why: Bob's criterion is that ALL ledger updates happen automatically, and
//! the Rust writer covers only `next`, `close` and `act`. The Architect
//! ruled the simplest route: a block carrying any other applied field goes,
//! whole, to the existing `ilio.py apply`, so nothing is applied twice.
//! What: [`route`] answers a replay first (the batch log, or the
//! [`APPLIED_FILE`] sidecar an ilio apply leaves), then runs [`super::check`]
//! and names the fields that need ilio ([`ilio_fields`]). The three ilio
//! rules the checker gained here ([`badid`], [`remind_problem`],
//! [`pct_unchanged`]) run on every block, whichever path applies it.
//! [`hold`] and the [`PENDING_DIR`] markers keep one ilio run per packet.
//! Test: `route_sends_uncovered_blocks_to_ilio_and_runs_the_new_checks`,
//! `il_pending_marker_reads_unknown_until_applied`.

use std::io::Write as _;

use super::{IlError, Row, check, get, packet_id};
use crate::ledger::model::State;
use crate::ledger::store::StorePaths;
use crate::ledger::writer::seen;
use crate::secret::SecretScan;

/// The sidecar, in the store dir, holding one packet id per line that
/// `ilio.py apply` applied; `ilio` writes no batch `request_id`.
pub const APPLIED_FILE: &str = "il-applied.txt";

/// Fields only `ilio.py apply` writes, whatever the task.
const ILIO_ALWAYS: [&str; 2] = ["new", "remind"];
/// Fields ilio writes as a task `pulse` when the block's task resolves.
const ILIO_PULSE: [&str; 5] = ["pct", "est", "remaining", "decided", "delta"];

/// The writer one block goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// The packet was applied before; write nothing.
    Replay(String),
    /// Every applied field is one the Rust writer covers.
    Rust,
    /// `ilio.py apply` applies the whole block; `fields` named why.
    Ilio {
        /// The packet id to record once ilio succeeds.
        packet: String,
        /// The present fields the Rust writer does not cover.
        fields: Vec<String>,
    },
}

/// The route for `text` against the store in `paths`. Every check runs
/// before a route is returned.
///
/// # Errors
/// [`IlError::Write`] when the store or the sidecar cannot be read;
/// [`IlError::Reject`] when the block breaks a rule.
pub fn route(paths: &StorePaths, text: &str, scan: SecretScan<'_>) -> Result<Route, IlError> {
    let raw = std::fs::read_to_string(&paths.state)
        .map_err(|e| IlError::Write(crate::error::LedgerError::io(&paths.state)(e)))?;
    let st = State::from_json(&raw).map_err(IlError::Write)?;
    let packet = packet_id(text).unwrap_or_default();
    if !packet.is_empty() && ilio_applied(paths, &packet)? {
        return Ok(Route::Replay(packet));
    }
    // G0b critic: a committed batch whose acts are missing; `apply` appends
    // them without re-checking the moved state.
    if !packet.is_empty() && seen(paths, &packet).map_err(IlError::Write)? {
        return Ok(Route::Rust);
    }
    let rows = check(text, &st, scan).map_err(IlError::Reject)?;
    let fields = ilio_fields(&rows, &st);
    Ok(if fields.is_empty() {
        Route::Rust
    } else {
        Route::Ilio { packet, fields }
    })
}

/// The present fields ilio would apply and the Rust writer would not: `new`
/// and `remind` always; the pulse fields when the task resolves.
pub fn ilio_fields(rows: &[Row], st: &State) -> Vec<String> {
    let tid = task_id(st, get(rows, "task")).is_some();
    let has = |f: &str| rows.iter().any(|(k, _)| k == f);
    let pulse = ILIO_PULSE.iter().filter(|f| tid && has(f));
    ILIO_ALWAYS
        .iter()
        .filter(|f| has(f))
        .chain(pulse)
        .map(|f| (*f).to_owned())
        .collect()
}

pub(crate) fn ilio_applied(paths: &StorePaths, packet: &str) -> Result<bool, IlError> {
    let path = paths.dir.join(APPLIED_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(text.lines().any(|l| l.trim() == packet)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(IlError::Write(crate::error::LedgerError::io(&path)(e))),
    }
}

/// Records that `ilio.py apply` applied `packet`, so a replay is a no-op.
///
/// # Errors
/// The sidecar could not be appended.
pub fn mark_applied(paths: &StorePaths, packet: &str) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(paths.dir.join(APPLIED_FILE))?
        .write_all(format!("{packet}\n").as_bytes())
}

/// The lock file, in the store dir, one delivery holds from route to marker.
pub const APPLY_LOCK: &str = "il-apply.lock";
/// The dir, in the store dir, holding one file per packet ilio is running.
pub const PENDING_DIR: &str = "il-pending";

/// Holds [`APPLY_LOCK`] until the file drops, so two deliveries of one
/// packet never both apply it (G0b critic). `wait` bounds the wait.
///
/// # Errors
/// [`IlError::Write`]: the lock is held past `wait`, or cannot be opened.
pub fn hold(paths: &StorePaths, wait: std::time::Duration) -> Result<std::fs::File, IlError> {
    crate::ledger::store::lock(&paths.dir.join(APPLY_LOCK), Some(wait)).map_err(IlError::Write)
}

fn pending_path(paths: &StorePaths, packet: &str) -> std::path::PathBuf {
    paths.dir.join(PENDING_DIR).join(packet)
}

/// Records that `ilio.py apply` is about to run on `packet`.
///
/// # Errors
/// The marker could not be written.
pub fn mark_pending(paths: &StorePaths, packet: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(paths.dir.join(PENDING_DIR))?;
    std::fs::write(pending_path(paths, packet), b"")
}

/// Drops the pending marker; a missing one is not an error.
///
/// # Errors
/// The marker exists and could not be removed.
pub fn clear_pending(paths: &StorePaths, packet: &str) -> std::io::Result<()> {
    match std::fs::remove_file(pending_path(paths, packet)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// True when an earlier ilio run on `packet` left a pending marker and no
/// applied record: its outcome is unknown, so it must not run again.
///
/// # Errors
/// [`IlError::Write`] when the sidecar cannot be read.
pub fn pending_unknown(paths: &StorePaths, packet: &str) -> Result<bool, IlError> {
    Ok(pending_path(paths, packet).exists() && !ilio_applied(paths, packet)?)
}

/// `[a-z][a-z0-9]{1,7}-`: the length of a project-code prefix, if any.
fn code_prefix(tok: &str) -> Option<usize> {
    let (code, _) = tok.split_once('-')?;
    let b = code.as_bytes();
    let ok = (2..=8).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    ok.then_some(code.len() + 1)
}

/// ilio `ID.fullmatch`: `[code-]<G|D|A|T|R><2 base-34 digits>[@dddd]`.
fn id_full(tok: &str) -> bool {
    let rest = code_prefix(tok).map_or(tok, |n| &tok[n..]);
    let (rest, stamp) = rest
        .split_once('@')
        .map_or((rest, None), |(r, s)| (r, Some(s)));
    let b = rest.as_bytes();
    let digit =
        |c: &u8| c.is_ascii_digit() || (c.is_ascii_lowercase() && !matches!(c, b'l' | b'o'));
    b.len() == 3
        && matches!(b[0], b'G' | b'D' | b'A' | b'T' | b'R')
        && b[1..].iter().all(digit)
        && stamp.is_none_or(|s| s.len() == 4 && s.bytes().all(|c| c.is_ascii_digit()))
}

/// ilio `LOOKALIKE.fullmatch`: an id shape holding an `o` or an `l`.
fn lookalike(tok: &str) -> bool {
    let body = |s: &str| {
        let b = s.as_bytes();
        !b.is_empty()
            && matches!(b[0], b'G' | b'D' | b'A' | b'T' | b'R')
            && b[1..]
                .iter()
                .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase())
            && b[1..].iter().any(|c| matches!(c, b'o' | b'l'))
    };
    body(tok) || code_prefix(tok).is_some_and(|n| body(&tok[n..]))
}

/// ilio BADID: a `task`/`open`/`close` token shaped like an id but holding
/// a non-base-34 `o` or `l`. The offending token, if any.
pub(crate) fn badid(v: &str) -> Option<&str> {
    v.split([' ', ','])
        .filter(|t| !t.is_empty())
        .find(|t| lookalike(t) && !id_full(t))
}

/// ilio `task_id`: the full id of the task the token names, stamp dropped.
pub(crate) fn task_id<'a>(st: &'a State, tok: &str) -> Option<&'a str> {
    let tok = tok.split([' ', ',']).next()?;
    let tok = tok.split_once('@').map_or(tok, |(id, _)| id);
    let (code, bare) = code_prefix(tok).map_or((None, tok), |n| (Some(&tok[..n]), &tok[n..]));
    if !bare.starts_with('T') || !id_full(bare) {
        return None;
    }
    st.tasks
        .iter()
        .find(|t| t.id.ends_with(&format!("-{bare}")) && code.is_none_or(|c| t.id.starts_with(c)))
        .map(|t| t.id.as_str())
}

/// ilio `remind_problem`, form and task part: `None` when the value reads
/// `<task> at=<ts> [every=<n>] "note"` naming a task in the ledger. The `at`
/// and `every` values are parsed by `il_remind` on the ilio path.
pub(crate) fn remind_problem(v: &str, st: &State) -> Option<String> {
    let form = || Some("form <task> at=<ts> [every=14d] \"note\"".to_owned());
    let Some((task, rest)) = v.split_once(char::is_whitespace) else {
        return form();
    };
    let Some(rest) = rest.trim_start().strip_prefix("at=") else {
        return form();
    };
    let (at, mut rest) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    rest = rest.trim_start();
    if let Some(e) = rest.strip_prefix("every=") {
        let (every, r) = e.split_once(char::is_whitespace).unwrap_or((e, ""));
        if every.is_empty() {
            return form();
        }
        rest = r.trim_start();
    }
    let note = rest.strip_prefix('"').and_then(|r| r.strip_suffix('"'));
    if at.is_empty() || !note.is_some_and(|n| !n.is_empty() && !n.contains('"')) {
        return form();
    }
    task_id(st, task)
        .is_none()
        .then(|| format!("{task} is not a live task in the ledger"))
}

/// ilio's apply-time rule, run at check time: `pct` equal to the task's last
/// `pct` while `done:` claims progress and `remaining:` is empty.
pub(crate) fn pct_unchanged(rows: &[Row], st: &State) -> Option<String> {
    let tid = task_id(st, get(rows, "task"))?;
    let pct: u64 = get(rows, "pct").parse().ok()?;
    let last = st.tasks.iter().find(|t| t.id == tid)?.extra.get("pct")?;
    let done = get(rows, "done").trim().to_lowercase();
    let claims = !matches!(done.as_str(), "" | "none" | "nothing" | "-");
    (last.as_u64() == Some(pct) && claims && get(rows, "remaining").trim().is_empty()).then(|| {
        format!("pct {pct} is unchanged for {tid} but done: claims progress; move pct or give remaining:")
    })
}
