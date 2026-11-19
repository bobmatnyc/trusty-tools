//! Applying a checked IL packet to a session's `datg` store (G0b).
//!
//! Why: the harness, not the model, writes the ledger, with the same bytes
//! `ilio.py apply` leaves through `datg.py write_ops`.
//! What: [`apply`] checks the block ([`super::check`]), answers a packet
//! whose id the log already records as a no-op, builds the ops ilio builds
//! (`session next`, `close` for live ids, `act` records), commits the state
//! ops in one [`Writer::batch_at`] (atomic replace, `written_epoch` set,
//! request id = packet id) and then appends the `act` lines to `log.jsonl`
//! as datg's `append_log` does, then records the packet in `APPLIED_FILE`;
//! a committed packet missing there gets its acts appended on a retry. Fields ilio routes to the dashboard or to
//! `pulse` (`new`, `remind`, `pct`/`est`/`remaining`/`decided`/`delta`) are
//! not applied and are named in [`Applied::skipped`].
//! Test: `tests/il_packet.rs`.

use std::fs::OpenOptions;
use std::io::Write as _;

use serde::Serialize;
use serde_json::{Value, json};

use super::{ACT_LIMITS, Reject, Row, act_parts, check, get};
use crate::error::LedgerError;
use crate::ledger::model::State;
use crate::ledger::ops::Now;
use crate::ledger::rpc::{BatchRequest, Op, StoreRef};
use crate::ledger::store::StorePaths;
use crate::ledger::writer::{Writer, seen};

/// Fields ilio applies outside `datg write_ops`' session/close/act ops.
const SKIPPED: [&str; 7] = [
    "new",
    "remind",
    "pct",
    "est",
    "remaining",
    "decided",
    "delta",
];

/// What one apply did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// The packet id (the batch `request_id`).
    pub packet: String,
    /// True when the log already held this packet: nothing was written.
    pub replayed: bool,
    /// The store's `v` after the write (0 on a replay).
    pub v: u64,
    /// The event ids the batch logged.
    pub events: Vec<String>,
    /// `act` lines appended.
    pub acts: usize,
    /// Fields present but not applied here.
    pub skipped: Vec<String>,
}

/// Why an apply wrote nothing, or wrote only part.
#[derive(Debug)]
pub enum IlError {
    /// The block broke a rule; nothing was written.
    Reject(Reject),
    /// The store could not be read or the batch was refused; nothing written.
    Write(LedgerError),
    /// The state committed, but the `act` lines (or the sidecar) could not
    /// be appended; a retry of the packet appends them.
    Act(std::io::Error),
}

impl IlError {
    /// The rule slug the error log records.
    pub fn code(&self) -> &str {
        match self {
            Self::Reject(r) => r.code,
            Self::Write(_) => "WRITE",
            Self::Act(_) => "WRITE-ACT",
        }
    }
}

impl std::fmt::Display for IlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Reject(r) => write!(f, "{r}"),
            Self::Write(e) => write!(f, "IL-REJECT WRITE ledger: {e}"),
            Self::Act(e) => write!(
                f,
                "IL-PARTIAL WRITE-ACT log.jsonl: state written, acts pending a retry: {e}"
            ),
        }
    }
}

impl std::error::Error for IlError {}

/// datg `apply_act`'s log record, in its key order.
#[derive(Serialize)]
struct ActLine {
    ts: String,
    kind: &'static str,
    session: String,
    who: String,
    what: String,
    owner_words: String,
    evidence: String,
    goal: String,
    refs: String,
}

fn first(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn done(v: Value) -> bool {
    v == "done"
}

/// Ids not done (ilio `live_ids`).
fn live(st: &State, id: &str) -> bool {
    let s = |v: Result<Value, _>| v.is_ok_and(|v| !done(v));
    st.goals
        .iter()
        .any(|g| g.id == id && s(serde_json::to_value(g.status)))
        || st
            .items
            .iter()
            .any(|i| i.id == id && s(serde_json::to_value(i.status)))
        || st
            .tasks
            .iter()
            .any(|t| t.id == id && s(serde_json::to_value(t.status)))
}

/// ilio `parent_goal` without the ad-hoc default: the block task's goal.
fn parent_goal(st: &State, task: &str) -> String {
    let tok = task.split([' ', ',']).next().unwrap_or("");
    let tok = tok.split_once('@').map_or(tok, |(id, _)| id);
    st.tasks
        .iter()
        .find(|t| t.id == tok || (!tok.contains('-') && t.id.ends_with(&format!("-{tok}"))))
        .map(|t| t.goal.split(',').next().unwrap_or("").trim().to_owned())
        .unwrap_or_default()
}

fn act_line(
    raw: &str,
    rows: &[Row],
    goal: &str,
    now: &Now,
    session: &str,
    who: &str,
) -> Result<ActLine, Reject> {
    let bad = |e: String| Reject {
        code: "ACT",
        field: "act".to_owned(),
        expected: e,
    };
    let (what, kv) = act_parts(raw).map_err(bad)?;
    let part = |k: &str| {
        kv.iter()
            .rev()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
    };
    let or =
        |v: Option<String>, d: &str| v.filter(|v| !v.is_empty()).unwrap_or_else(|| d.to_owned());
    let line = ActLine {
        ts: now.minute.clone(),
        kind: "act",
        session: session.to_owned(),
        who: who.trim().to_owned(),
        what: what.trim().to_owned(),
        owner_words: part("owner").unwrap_or_default(),
        evidence: or(part("ev"), get(rows, "evidence")),
        goal: or(part("goal"), goal),
        refs: or(part("refs"), get(rows, "refs")),
    };
    let fields = [
        &line.who,
        &line.what,
        &line.owner_words,
        &line.evidence,
        &line.goal,
        &line.refs,
    ];
    for ((k, max, req), v) in ACT_LIMITS.iter().zip(fields) {
        if v.contains('\n') || v.chars().count() > *max || (*req && v.is_empty()) {
            return Err(bad(format!("act {k} must be one line, <= {max} chars")));
        }
    }
    Ok(line)
}

/// Applies the IL block in `text` to the store in `paths`, for `session`;
/// `who` is the `act` actor. Every check runs before any write.
///
/// # Errors
/// [`IlError::Reject`] or [`IlError::Write`] leave every file as it was;
/// [`IlError::Act`] means the state committed and the act lines did not;
/// applying the same packet again appends them.
pub fn apply(
    writer: &Writer<'_>,
    paths: &StorePaths,
    session: &str,
    who: &str,
    text: &str,
) -> Result<Applied, IlError> {
    // See G0b IL auto-apply
    let raw = std::fs::read_to_string(&paths.state)
        .map_err(|e| IlError::Write(LedgerError::io(&paths.state)(e)))?;
    let st = State::from_json(&raw).map_err(IlError::Write)?;
    // A replay is answered before the check: the first apply may have
    // archived the ids the block closes, which would now read as stale.
    // G0b critic: a packet is replayed only once its acts are written too
    // (the APPLIED_FILE sidecar); a committed batch without it re-appends.
    let packet = super::packet_id(text).unwrap_or_default();
    let committed = !packet.is_empty() && seen(paths, &packet).map_err(IlError::Write)?;
    if committed && super::route::ilio_applied(paths, &packet)? {
        let (v, events, acts, skipped) = (0, Vec::new(), 0, Vec::new());
        return Ok(Applied {
            packet,
            replayed: true,
            v,
            events,
            acts,
            skipped,
        });
    }
    let rows = if committed {
        // Checked before the batch committed; the state has moved since.
        super::parse_block(text).unwrap_or_default()
    } else {
        check(text, &st, writer.scan).map_err(IlError::Reject)?
    };
    let skipped: Vec<String> = SKIPPED
        .iter()
        .filter(|f| rows.iter().any(|(k, _)| k == *f))
        .map(|f| (*f).to_owned())
        .collect();
    let now = Now::at(writer.clock.now_unix());
    let mut ops = vec![json!({"op": "session", "next": first(get(&rows, "next"), 200)})];
    let outcome = [get(&rows, "evidence"), get(&rows, "done")]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or("closed");
    for tok in get(&rows, "close")
        .split([' ', ','])
        .filter(|t| !t.is_empty())
    {
        let id = tok.split_once('@').map_or(tok, |(id, _)| id);
        if id.contains('-') && live(&st, id) {
            ops.push(json!({"op": "close", "id": id, "outcome": first(outcome, 200)}));
        }
    }
    let goal = parent_goal(&st, get(&rows, "task"));
    let acts = rows
        .iter()
        .filter(|(k, _)| k == "act")
        .map(|(_, v)| act_line(v, &rows, &goal, &now, session, who))
        .collect::<Result<Vec<_>, _>>()
        .map_err(IlError::Reject)?;
    let ops: Vec<Op> = serde_json::from_value(Value::Array(ops))
        .map_err(|e| IlError::Write(LedgerError::Json(e)))?;
    let req = BatchRequest {
        store: StoreRef {
            project: paths.dir.clone(),
            session: session.to_owned(),
        },
        request_id: packet.clone(),
        if_v: None,
        ops,
    };
    let (v, events) = if committed {
        (st.v, Vec::new())
    } else {
        let resp = writer.batch_at(paths, &req).map_err(IlError::Write)?;
        (resp.v, resp.events.into_iter().map(|e| e.id).collect())
    };
    let mut text = String::new();
    for a in &acts {
        text += &serde_json::to_string(a).map_err(|e| IlError::Act(e.into()))?;
        text.push('\n');
    }
    if !text.is_empty() {
        writer
            .faults
            .at(crate::ledger::writer::CommitStep::Acts, paths)
            .map_err(IlError::Act)?;
        // datg `append_log`: O_APPEND, one write.
        OpenOptions::new()
            .append(true)
            .create(true)
            .open(&paths.log)
            .and_then(|mut f| f.write_all(text.as_bytes()))
            .map_err(IlError::Act)?;
    }
    if !packet.is_empty() {
        super::route::mark_applied(paths, &packet).map_err(IlError::Act)?;
    }
    Ok(Applied {
        packet,
        replayed: false,
        v,
        events,
        acts: acts.len(),
        skipped,
    })
}
