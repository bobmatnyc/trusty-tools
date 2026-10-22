//! The PM ledger writer: one batch, all or nothing, under the store's lock
//! (#9758).
//!
//! Why: the daemon method, the MCP tool and the CLI's in-process fallback
//! must write a store the same way, so the single-writer rule holds whichever
//! process writes (owner ruling, #9758 amendment) and the bytes equal the
//! prototype's (LDG-1.5).
//! What: [`Writer::batch`] takes the store's `flock` (bounded wait), replays
//! a commit journal a killed writer left, answers a seen `request_id` from
//! its `log.jsonl` record, refuses secret-shaped input (logged without the
//! value), checks `if_v`, applies every op to a copy, archives what closed,
//! prunes once when the result would pass the 16 KB bar, and commits:
//! journal (fsync), `v` re-read (a change logs `tamper`), archive lines,
//! outbox lines (LDG-4.4), log lines, `state.json` replace and directory fsync, journal removed
//! (LDG-2.8.2). Only a missing file reads as empty (#9758). [`show`] reads.
//! Test: `tests/ledger_writer.rs`, `ledger_writer_matches_prototype_bytes`.
//!
//! # Spec References
//! - [`SPEC-LEDGER-02~draft`](docs/specs/DOC-82-portfolio-intent-ledger-dashboard.md#SPEC-LEDGER-02~draft)

// #9758: the journal, the outbox lines and the read-only side are split out
// to keep each file under the SLOC cap.
mod journal;
mod outbox;
mod show;

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use outbox::OutboxLine;
pub use show::{minute, render_ids, render_prune, show};

use self::journal::{
    append_notice, complete, file_len, notice, read_lossy, reconcile_archive, recover, remove,
    replay, state_v,
};
use crate::clock::Clock;
use crate::error::LedgerError;
use crate::ledger::ids::LedgerId;
use crate::ledger::limits::{SIZE_REFUSE, SIZE_WARN, SizeVerdict, check_writable, size_verdict};
use crate::ledger::model::{ArchiveLine, Archived, Extra, LogLine, State};
use crate::ledger::ops::{self, Context, Now};
use crate::ledger::rpc::{BatchEvent, BatchRequest, BatchResponse, Op};
use crate::ledger::store::{StorePaths, lock, write_atomic};
use crate::pyjson::to_python_json;

/// How long a writer waits for the store lock before it reports exit 3.
pub const LOCK_WAIT: Duration = Duration::from_secs(10);

/// A point in a commit where a fault can be injected (a kill, a lockless writer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitStep {
    /// The journal is written; nothing else is.
    Staged,
    /// The archive and log lines are appended; `state.json` is not replaced.
    Logged,
    /// `state.json` is replaced and its directory fsynced; the journal is
    /// not yet removed (#9758).
    Replaced,
}

/// Faults a commit runs through; production uses [`NoFaults`].
pub trait Faults {
    /// Called at `step`; an error stops the commit there, as a kill would.
    ///
    /// # Errors
    /// Whatever the fault injects.
    fn at(&self, step: CommitStep, paths: &StorePaths) -> io::Result<()>;
}

/// No faults.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoFaults;

impl Faults for NoFaults {
    fn at(&self, _: CommitStep, _: &StorePaths) -> io::Result<()> {
        Ok(())
    }
}

/// A secret-shape scanner: a redacted shape name (never the value) for a
/// secret-shaped text.
pub type SecretScan<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Writes batches to PM ledger stores.
pub struct Writer<'a> {
    /// The clock every timestamp comes from.
    pub clock: &'a dyn Clock,
    /// Refuses secret-shaped input (LDG-2.3.5).
    pub scan: SecretScan<'a>,
    /// Fault injection.
    pub faults: &'a dyn Faults,
    /// The lock wait bound.
    pub lock_wait: Duration,
}

/// A commit's redo record: everything the commit writes, staged first.
#[derive(Serialize, Deserialize)]
struct Journal {
    v: u64,
    archive_at: u64,
    archive: String,
    // #9758 F3: absent in a journal a PR A writer left.
    #[serde(default)]
    outbox_at: u64,
    #[serde(default)]
    outbox: String,
    log_at: u64,
    log: String,
    state: String,
}

/// The `log.jsonl` line that records a request's result (#9758 amendment).
#[derive(Serialize, Deserialize)]
struct RequestLine {
    ts: String,
    session: String,
    kind: String,
    request_id: String,
    result: BatchResponse,
}

fn bad(msg: impl Into<String>) -> LedgerError {
    LedgerError::Invalid(msg.into())
}

impl<'a> Writer<'a> {
    /// A writer with no faults and [`LOCK_WAIT`].
    pub fn new(clock: &'a dyn Clock, scan: SecretScan<'a>) -> Self {
        Self {
            clock,
            scan,
            faults: &NoFaults,
            lock_wait: LOCK_WAIT,
        }
    }

    /// Applies `req` to its store: every op or none.
    ///
    /// A sole `prune` that finds nothing writes nothing; a `dry_run` prune
    /// (alone in its batch) reports the ids and writes nothing.
    ///
    /// # Errors
    /// [`LedgerError::code`] gives the class: `if_v` or `tamper` is a
    /// conflict (exit 1); a lock timeout or an unreadable file is exit 3;
    /// anything else is invalid (exit 2) and leaves `state.json` and
    /// `archive.jsonl` as they were.
    pub fn batch(&self, req: &BatchRequest) -> Result<BatchResponse, LedgerError> {
        if req.request_id.len() > 128 || req.request_id.contains('\n') {
            return Err(bad("request_id: one line, <= 128 chars"));
        }
        req.ops.iter().try_for_each(Op::check)?;
        let dry_run = req
            .ops
            .iter()
            .any(|o| matches!(o, Op::Prune(p) if p.dry_run));
        if dry_run && req.ops.len() > 1 {
            return Err(bad("a dry-run prune must be the only op"));
        }
        let paths = req.store.paths()?;
        let _lock = lock(&paths.lock, Some(self.lock_wait))?;
        recover(&paths)?;
        if let Some(seen) = replay(&paths, &req.request_id)? {
            return Ok(seen);
        }
        let now = Now::at(self.clock.now_unix());
        let session = req.store.session.as_str();
        if let Some(refusal) = self.secret(req) {
            append_notice(&paths, &now, session, "secret_refused", &refusal)?;
            return Err(bad(refusal));
        }
        let raw = match fs::read_to_string(&paths.state) {
            Ok(raw) => raw,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(bad("state.json missing; run init"));
            }
            Err(e) => return Err(LedgerError::io(&paths.state)(e)),
        };
        let mut st = State::from_json(&raw)
            .map_err(|e| bad(format!("state.json malformed ({e}); refusing to write")))?;
        check_writable(&st)?;
        if let Some(want) = req.if_v.filter(|w| *w != st.v) {
            return Err(LedgerError::Conflict(format!(
                "if_v {want} does not match the store's v {}",
                st.v
            )));
        }
        let archived = archived_ids(&paths)?;
        let answered = answered_times(&paths)?;
        let cx = Context {
            archived: &archived,
            answered: &answered,
            now: &now,
        };
        let (mut events, mut gone) = (Vec::new(), Vec::new());
        for op in &req.ops {
            let (e, g) = ops::apply(&mut st, op, &cx)?;
            events.extend(e);
            gone.extend(g);
        }
        gone.extend(ops::collect_closed(&mut st, &events));
        let only_prune = matches!(req.ops.as_slice(), [Op::Prune(_)]);
        if dry_run || (only_prune && gone.is_empty()) {
            let archived = gone.iter().map(|a| archived_id(a).to_owned()).collect();
            return Ok(BatchResponse {
                v: st.v,
                events: Vec::new(),
                archived,
                warnings: Vec::new(),
            });
        }
        let commit = Commit {
            paths: &paths,
            now: &now,
            session,
            loaded_v: st.v,
            old: raw.len() as u64,
        };
        commit.run(self.faults, st, events, gone, &answered, &req.request_id)
    }

    /// The refusal text for a secret-shaped `request_id` or the first
    /// secret-shaped op, naming its shape only.
    ///
    /// #9758: each decoded string is scanned on its own, so a JSON escape
    /// (`\n` before a token) cannot hide it from a boundary-anchored shape,
    /// and an op that cannot be serialized for the scan is refused.
    /// Test: `ledger_writer_refuses_a_secret_after_an_escaped_newline`,
    /// `ledger_writer_refuses_a_secret_shaped_request_id`.
    fn secret(&self, req: &BatchRequest) -> Option<String> {
        const FIX: &str = "write a location, never a value";
        if let Some(shape) = (self.scan)(&req.request_id) {
            return Some(format!(
                "request_id refused: secret-shaped value ({shape}); {FIX}"
            ));
        }
        req.ops.iter().enumerate().find_map(|(i, op)| {
            let n = i + 1;
            let Ok(value) = serde_json::to_value(op) else {
                return Some(format!(
                    "op {n} refused: it could not be scanned for secrets"
                ));
            };
            let shape = scan_strings(&value, self.scan)?;
            Some(format!(
                "op {n} refused: secret-shaped value ({shape}); {FIX}"
            ))
        })
    }
}

/// One commit's fixed inputs.
struct Commit<'a> {
    paths: &'a StorePaths,
    now: &'a Now,
    session: &'a str,
    loaded_v: u64,
    old: u64,
}

impl Commit<'_> {
    fn run(
        &self,
        faults: &dyn Faults,
        mut st: State,
        events: Vec<BatchEvent>,
        mut gone: Vec<Archived>,
        answered: &BTreeMap<String, f64>,
        request_id: &str,
    ) -> Result<BatchResponse, LedgerError> {
        let now = self.now;
        st.v += 1;
        let ses = st.session.get_or_insert_with(Default::default);
        ses.written_at = Some(now.minute.clone());
        ses.written_epoch = Some(now.epoch);
        let mut text = st.to_json()?;
        if size_verdict(self.old, text.len() as u64) == SizeVerdict::Refuse {
            gone.extend(ops::prune_state(&mut st, answered, 24.0, false, now.unix));
            text = st.to_json()?;
            if size_verdict(self.old, text.len() as u64) == SizeVerdict::Refuse {
                return Err(bad(format!(
                    "state.json would be {} B (> {SIZE_REFUSE} B bar); largest: {}; close or shorten items",
                    text.len(),
                    largest(&st)
                )));
            }
        }
        let mut archive = String::new();
        for record in &gone {
            let kind = if matches!(record, Archived::Item(_)) {
                "item"
            } else {
                "task"
            };
            let (archived_at, closed_at) = (now.minute.clone(), now.minute.clone());
            let (kind, item, cv) = (kind.to_owned(), record.clone(), st.v);
            archive += &(ArchiveLine {
                archived_at,
                closed_at,
                kind,
                item,
                cv,
                extra: Extra::new(),
            }
            .to_line()?
                + "\n");
        }
        let mut log = String::new();
        for e in &events {
            let note = (!e.note.is_empty()).then(|| e.note.clone());
            let (t, id, ev, extra) = (now.minute.clone(), e.id.clone(), e.ev.clone(), Extra::new());
            log += &(LogLine::Event {
                t,
                id,
                ev,
                note,
                extra,
            }
            .to_line()?
                + "\n");
        }
        let mut warnings = Vec::new();
        if text.len() as u64 > SIZE_WARN && SIZE_WARN >= self.old {
            let warning = format!(
                "state.json {} B > {SIZE_WARN} B bar; largest: {}",
                text.len(),
                largest(&st)
            );
            log += &(notice(now, self.session, "size_warning", &warning)? + "\n");
            warnings.push(warning);
        }
        let rev = outbox::Rev {
            code: st.code.as_str(),
            v: st.v,
            now,
        };
        let outbox = outbox::lines(self.paths, &rev, &events, &gone)?;
        let archived = gone.iter().map(|a| archived_id(a).to_owned()).collect();
        let result = BatchResponse {
            v: st.v,
            events,
            archived,
            warnings,
        };
        if !request_id.is_empty() {
            let line = RequestLine {
                ts: now.minute.clone(),
                session: self.session.to_owned(),
                kind: "request".into(),
                request_id: request_id.to_owned(),
                result: result.clone(),
            };
            log += &(serde_json::to_string(&line)? + "\n");
        }
        // #9758: an orphan archive line from a crashed prototype commit
        // must go before this commit reuses its `cv`.
        reconcile_archive(&self.paths.archive, self.loaded_v)?;
        let journal = Journal {
            v: st.v,
            archive_at: file_len(&self.paths.archive),
            archive,
            outbox_at: file_len(&self.paths.outbox),
            outbox,
            log_at: file_len(&self.paths.log),
            log,
            state: text,
        };
        write_atomic(&self.paths.journal, &serde_json::to_string(&journal)?)?;
        faults
            .at(CommitStep::Staged, self.paths)
            .map_err(LedgerError::io(&self.paths.journal))?;
        let on_disk = fs::read_to_string(&self.paths.state)
            .ok()
            .and_then(|t| state_v(&t));
        if on_disk != Some(self.loaded_v) {
            remove(&self.paths.journal)?;
            let seen = on_disk.map_or_else(|| "unreadable".to_owned(), |v| v.to_string());
            let msg = format!(
                "state.json v changed from {} to {seen} during a write; not overwritten",
                self.loaded_v
            );
            append_notice(self.paths, now, self.session, "tamper", &msg)?;
            return Err(LedgerError::Conflict(msg));
        }
        complete(self.paths, &journal, faults)?;
        Ok(result)
    }
}

/// The first string in `v` (object keys included) that `scan` flags.
fn scan_strings(v: &Value, scan: SecretScan<'_>) -> Option<String> {
    match v {
        Value::String(s) => scan(s),
        Value::Array(a) => a.iter().find_map(|x| scan_strings(x, scan)),
        Value::Object(o) => o
            .iter()
            .find_map(|(k, x)| scan(k).or_else(|| scan_strings(x, scan))),
        _ => None,
    }
}

/// Ids in `archive.jsonl` (LDG-2.2.3: an archived id is never reissued).
/// #9758: a read error refuses the batch; only a missing file is empty.
/// Test: `ledger_writer_never_reissues_an_archived_id_past_a_non_utf8_archive_line`.
fn archived_ids(paths: &StorePaths) -> Result<Vec<LedgerId>, LedgerError> {
    let text = read_lossy(&paths.archive)?.unwrap_or_default();
    let id = |l: &str| {
        let v: Value = serde_json::from_str(l).ok()?;
        LedgerId::parse(v["item"]["id"].as_str()?).ok()
    };
    Ok(text.lines().filter_map(id).collect())
}

/// The last `answered` time per id in `log.jsonl`, either log shape.
fn answered_times(paths: &StorePaths) -> Result<BTreeMap<String, f64>, LedgerError> {
    let text = read_lossy(&paths.log)?.unwrap_or_default();
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let kind = v
            .get("ev")
            .or_else(|| v.get("kind"))
            .and_then(Value::as_str);
        let at = v
            .get("t")
            .or_else(|| v.get("ts"))
            .and_then(Value::as_str)
            .and_then(ts_unix);
        if let (Some("answered"), Some(at), Some(id)) = (kind, at, v["id"].as_str()) {
            out.insert(id.to_owned(), at);
        }
    }
    Ok(out)
}

/// Seconds since the epoch of a minute- or second-precision UTC time.
fn ts_unix(ts: &str) -> Option<f64> {
    let n = super::limits::normalize_ts(ts).ok()?;
    let num = |r: std::ops::Range<usize>| n.get(r)?.parse::<i64>().ok();
    let days = crate::classify::alerts::days_from_civil(num(0..4)?, num(5..7)?, num(8..10)?);
    let secs = days * 86_400 + num(11..13)? * 3600 + num(14..16)? * 60 + num(17..19)?;
    Some(secs as f64)
}

/// The three largest records, as the prototype's `_largest` names them.
fn largest(st: &State) -> String {
    let size = |o: Result<String, serde_json::Error>| o.map_or(0, |t| t.len());
    let mut objs: Vec<(usize, &str)> = st
        .goals
        .iter()
        .map(|g| (size(to_python_json(g, false)), g.id.as_str()))
        .collect();
    objs.extend(
        st.items
            .iter()
            .map(|i| (size(to_python_json(i, false)), i.id.as_str())),
    );
    objs.extend(
        st.tasks
            .iter()
            .map(|t| (size(to_python_json(t, false)), t.id.as_str())),
    );
    objs.sort_by(|a, b| b.cmp(a));
    let named: Vec<String> = objs
        .iter()
        .take(3)
        .map(|(b, id)| format!("{id} ({b} B)"))
        .collect();
    named.join(", ")
}

fn archived_id(a: &Archived) -> &str {
    match a {
        Archived::Item(i) => &i.id,
        Archived::Task(t) => &t.id,
    }
}
