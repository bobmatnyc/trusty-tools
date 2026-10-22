//! Receiver-side `message` events: a cross-session message as it arrives
//! (#9167 PR2).
//!
//! Why: PR1 records each `SendMessage` relay at the sender's `PreToolUse`.
//! The owner asked for the arrival too, so one message is one `sent` plus
//! one `received` event. No hook fires on arrival: Claude Code runs none for
//! a peer turn or for a message delivered mid-turn (checked against live
//! transcripts, 2026-10-22). The arrival is recorded only in the receiving
//! session's transcript, as one `queue-operation` `enqueue` line whose
//! `content` is `<cross-session-message from="…" …>\n<text>\n</cross-session-message>`.
//! There is one such line per arrival, whether the message then starts a turn
//! or rides a tool result, and `<text>` is the sender's `input.message`
//! byte for byte.
//! What: [`Tails`] remembers, per transcript a hook record named, how far it
//! has been read. [`Tails::note`] runs on the hook path and does no I/O; it
//! marks the transcript due. [`read`] runs on the blocking pool from the
//! flush task: it reads a due transcript from its offset and returns the
//! [`Arrival`]s in its complete lines ([`arrival`]). [`events`] makes each a
//! `received` `message` event through the same scan and digest as the sender
//! (`super::message::message_event`), so the two sha256 match.
//!
//! No double count: an offset only moves forward, and a transcript first seen
//! after the daemon started counts only arrivals stamped at or after that
//! first hook (a daemon restart never replays an earlier arrival). The cost is
//! that an arrival between a restart and the session's next hook is not
//! recorded. A transcript that shrinks is skipped to its end.
//! Test: `received_tests.rs`.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;
use trusty_common::control_bus::{
    EventId, HarnessEvent, HarnessPayload, HarnessSource, MessageChannel, MessageDirection,
    MessageEvent,
};

use super::message::{MAX_TO_CHARS, message_event};
use crate::core::hook::HookEventRecord;
use crate::core::session::SessionId;
use crate::daemon::bug_report::scrubber::scrub_credentials;

/// Transcripts tracked at once; the least recently noted is dropped.
pub const MAX_TRANSCRIPTS: usize = 256;

/// How far back a transcript's first read starts, in bytes.
pub const FIRST_LOOK_BYTES: u64 = 256 * 1024;

/// The most one read takes from one transcript; the rest waits a tick.
pub const MAX_READ_BYTES: u64 = 8 * 1024 * 1024;

const OPEN_TAG: &str = "<cross-session-message";
const CLOSE_TAG: &str = "\n</cross-session-message>";

/// One cross-session message as the receiving transcript recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arrival {
    /// When the harness queued it (the line's `timestamp`).
    pub at: DateTime<Utc>,
    /// The sender as the harness names it (the `from` attribute).
    pub from: String,
    /// The text, exactly as sent.
    pub text: String,
}

struct Tail {
    session: SessionId,
    offset: Option<u64>,
    since: DateTime<Utc>,
    mid_line: bool,
    due: bool,
    touched: u64,
}

/// A due transcript, taken by [`Tails::take_due`] for one [`read`].
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    /// The transcript.
    pub path: PathBuf,
    /// The receiving session.
    pub session: SessionId,
    /// Where the last read stopped; `None` before the first read.
    pub offset: Option<u64>,
    /// The first read counts only arrivals at or after this.
    pub since: DateTime<Utc>,
    /// Whether the read starts inside a line it must skip.
    pub mid_line: bool,
}

/// What one [`read`] found, and where the next one starts.
#[derive(Debug, Clone, PartialEq)]
pub struct Scan {
    /// The arrivals in the complete lines read.
    pub arrivals: Vec<Arrival>,
    /// The next read's offset.
    pub offset: u64,
    /// Whether that offset is inside a line.
    pub mid_line: bool,
    /// Whether bytes were left unread past [`MAX_READ_BYTES`].
    pub more: bool,
}

/// Per-transcript read positions.
///
/// Why: see the module doc; keyed by path, so a record naming another
/// transcript for the same session never resets this one.
/// What: bounded by [`MAX_TRANSCRIPTS`]; a dropped entry is a fresh first
/// look when it comes back, which can miss arrivals but never repeats one.
/// Test: `a_received_message_is_one_received_event_with_no_body_9167`,
/// `rescanning_and_a_restart_never_repeat_an_arrival_9167`.
#[derive(Default)]
pub struct Tails {
    by_path: HashMap<PathBuf, Tail>,
    tick: u64,
}

impl Tails {
    /// Mark the transcript `record` names as due a read. No I/O.
    pub fn note(&mut self, record: &HookEventRecord) {
        let Some(path) = transcript_path(&record.payload) else {
            return;
        };
        self.tick += 1;
        if let Some(tail) = self.by_path.get_mut(path) {
            tail.session = record.session;
            tail.due = true;
            tail.touched = self.tick;
            return;
        }
        if self.by_path.len() >= MAX_TRANSCRIPTS
            && let Some(oldest) = self
                .by_path
                .iter()
                .min_by_key(|(_, t)| t.touched)
                .map(|(p, _)| p.clone())
        {
            self.by_path.remove(&oldest);
        }
        let tail = Tail {
            session: record.session,
            offset: None,
            since: record.at,
            mid_line: false,
            due: true,
            touched: self.tick,
        };
        self.by_path.insert(path.to_path_buf(), tail);
    }

    /// The due transcripts, which stop being due.
    pub fn take_due(&mut self) -> Vec<Job> {
        self.by_path
            .iter_mut()
            .filter(|(_, t)| t.due)
            .map(|(path, t)| {
                t.due = false;
                Job {
                    path: path.clone(),
                    session: t.session,
                    offset: t.offset,
                    since: t.since,
                    mid_line: t.mid_line,
                }
            })
            .collect()
    }

    /// Record where `job`'s read stopped; due again when bytes were left.
    pub fn finish(&mut self, job: &Job, read: &Scan) {
        if let Some(tail) = self.by_path.get_mut(&job.path) {
            tail.offset = Some(read.offset);
            tail.mid_line = read.mid_line;
            tail.due |= read.more;
        }
    }
}

/// An absolute `*.jsonl` `transcript_path` in a hook payload.
fn transcript_path(payload: &Value) -> Option<&Path> {
    let path = Path::new(payload.get("transcript_path")?.as_str()?);
    (path.is_absolute() && path.extension().is_some_and(|e| e == "jsonl")).then_some(path)
}

/// Read `job`'s transcript from its offset; see the module doc.
///
/// What: a first read starts [`FIRST_LOOK_BYTES`] before the end, skips the
/// partial line it lands in, and keeps only arrivals at or after
/// `job.since`. A later read starts at the offset. At most
/// [`MAX_READ_BYTES`] are read; only complete lines are parsed, and the
/// offset stops after the last one. A file shorter than the offset is
/// skipped to its end.
/// Test: `a_received_message_is_one_received_event_with_no_body_9167`,
/// `a_partial_last_line_is_read_once_it_is_complete_9167`.
///
/// # Errors
/// The transcript cannot be opened or read.
pub fn read(job: &Job) -> io::Result<Scan> {
    let mut file = File::open(&job.path)?;
    let len = file.metadata()?.len();
    let (start, mut mid_line, since) = match job.offset {
        Some(o) if o <= len => (o, job.mid_line, None),
        Some(_) => return Ok(Scan::empty(len)),
        None => {
            let start = len.saturating_sub(FIRST_LOOK_BYTES);
            (start, start > 0, Some(job.since))
        }
    };
    let take = (len - start).min(MAX_READ_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity(usize::try_from(take).unwrap_or_default());
    file.take(take).read_to_end(&mut buf)?;
    let mut from = 0;
    if mid_line {
        match buf.iter().position(|b| *b == b'\n') {
            Some(nl) => {
                from = nl + 1;
                mid_line = false;
            }
            None => {
                let offset = start + buf.len() as u64;
                return Ok(Scan {
                    arrivals: Vec::new(),
                    offset,
                    mid_line: true,
                    more: offset < len,
                });
            }
        }
    }
    let end = buf[from..]
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(from, |nl| from + nl + 1);
    let arrivals = String::from_utf8_lossy(&buf[from..end])
        .lines()
        .filter_map(arrival)
        .filter(|a| since.is_none_or(|s| a.at >= s))
        .collect();
    // A line longer than one read is skipped rather than read forever.
    let (offset, mid_line) = if end == from && buf.len() as u64 == MAX_READ_BYTES {
        (start + buf.len() as u64, true)
    } else {
        (start + end as u64, mid_line)
    };
    Ok(Scan {
        arrivals,
        offset,
        mid_line,
        more: start + take < len,
    })
}

impl Scan {
    fn empty(offset: u64) -> Self {
        Self {
            arrivals: Vec::new(),
            offset,
            mid_line: false,
            more: false,
        }
    }
}

/// The arrival one transcript line records, if it records one.
///
/// What: a `queue-operation` `enqueue` line whose `content` is a whole
/// `<cross-session-message …>` element with a `from` attribute and whose
/// `timestamp` parses. Every other line, including the same message's
/// `dequeue`, `remove` and peer turn, is `None`.
/// Test: `only_the_enqueue_line_is_an_arrival_9167`.
pub fn arrival(line: &str) -> Option<Arrival> {
    if !line.contains("\"queue-operation\"") || !line.contains(OPEN_TAG) {
        return None;
    }
    let v: Value = serde_json::from_str(line).ok()?;
    let field = |k: &str| v.get(k).and_then(Value::as_str);
    if field("type")? != "queue-operation" || field("operation")? != "enqueue" {
        return None;
    }
    let (from, text) = element(field("content")?)?;
    let at = DateTime::parse_from_rfc3339(field("timestamp")?)
        .ok()?
        .with_timezone(&Utc);
    Some(Arrival { at, from, text })
}

/// `(from, text)` of `<cross-session-message a="…" …>\n<text>\n</cross-session-message>`.
fn element(content: &str) -> Option<(String, String)> {
    let mut rest = content.strip_prefix(OPEN_TAG)?;
    let mut from = None;
    loop {
        rest = rest.trim_start_matches(' ');
        if let Some(after) = rest.strip_prefix('>') {
            rest = after;
            break;
        }
        let (name, after) = rest.split_once("=\"")?;
        let (value, after) = after.split_once('"')?;
        if name == "from" {
            from = Some(value.to_string());
        }
        rest = after;
    }
    let text = rest.strip_prefix('\n')?.strip_suffix(CLOSE_TAG)?;
    Some((from?, text.to_string()))
}

/// The bus events for `arrivals` in `session`, children of `root` when the
/// session has one.
///
/// What: one `received` `sendmessage` event per arrival, `at` the arrival's
/// time, from the sender as the harness named it (scanned, capped) to the
/// receiving session; `bytes`, `sha256` and `body` as the sender's.
/// Test: `a_received_message_is_one_received_event_with_no_body_9167`,
/// `a_secret_in_a_received_message_reaches_no_field_9167`.
pub fn events(
    session: SessionId,
    arrivals: Vec<Arrival>,
    bodies: bool,
    root: Option<EventId>,
) -> Vec<HarnessEvent> {
    let to = session.0.to_string();
    arrivals
        .into_iter()
        .map(|a| HarnessEvent {
            source: HarnessSource::Mpm,
            session: Some(to.clone()),
            seq: 0,
            at: a.at,
            payload: HarnessPayload::Message(received_event(&a, &to, bodies)),
            id: EventId::new(),
            parent_id: root,
        })
        .collect()
}

/// The `received` `MessageEvent` for `arrival` in session `to`.
pub fn received_event(arrival: &Arrival, to: &str, bodies: bool) -> MessageEvent {
    let mut m = message_event(
        arrival.at,
        MessageChannel::SendMessage,
        "",
        to,
        &arrival.text,
        bodies,
    );
    m.direction = MessageDirection::Received;
    m.from = scrub_credentials(&arrival.from)
        .chars()
        .take(MAX_TO_CHARS)
        .collect();
    m
}
