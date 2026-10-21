//! The delivery trace of inbound channel answers, `delivery.jsonl` (#9873 Q5).
//!
//! Why: an answer is final once its question resolves (#8433 Q4), because the
//! table keeps only the text's digest. A delivery that failed, or that never
//! settled because the daemon died mid-send, must stay visible to `tm doctor`.
//! What: [`DeliveryLine`], one append-only line per step: `pending` before the
//! send, then `delivered` or `failed`. The file sits in the anchored taint
//! directory beside [`crate::core::channels_taint::REAPPLY_FAILURES_FILE`],
//! so no session can edit it, and a retire leaves it alone (it is not a
//! `*.json` taint).
//! Test: `channels_delivery_tests.rs`, `deliver_tests.rs`,
//! `doctor_channel_taint_tests.rs`.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::channels_taint::TaintError;

/// The delivery trace's file name in the taint directory.
pub const DELIVERY_FILE: &str = "delivery.jsonl";

/// The record format version.
pub const FORMAT_VERSION: u32 = 1;

/// One delivery step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    /// The asker is tainted and the send is about to run.
    Pending,
    /// The pane accepted the answer.
    Delivered,
    /// The answer was not delivered; it is lost (#8433 Q4).
    Failed,
}

/// What a trace line delivers (#10127).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryKind {
    /// A resolved question's answer; a line without `kind` is one.
    #[default]
    Answer,
    /// A held reply's notice; its question is still open.
    Notice,
}

impl DeliveryKind {
    fn is_answer(&self) -> bool {
        *self == Self::Answer
    }
}

/// One line of `delivery.jsonl`. It holds no answer text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryLine {
    /// [`FORMAT_VERSION`].
    pub v: u32,
    /// When the step was recorded.
    pub ts: DateTime<Utc>,
    /// The step.
    pub state: DeliveryState,
    /// The resolved question's id, decimal.
    pub question_id: String,
    /// The asker's managed session id.
    pub asker: String,
    /// Why a delivery failed: a content-free code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// What the line delivers (#10127). Not written for an answer, so an
    /// older line, which has no `kind`, reads as an answer's.
    #[serde(default, skip_serializing_if = "DeliveryKind::is_answer")]
    pub kind: DeliveryKind,
}

impl DeliveryLine {
    /// An answer line for `state` stamped now.
    pub fn new(state: DeliveryState, question_id: &str, asker: &str, reason: Option<&str>) -> Self {
        Self {
            v: FORMAT_VERSION,
            ts: Utc::now(),
            state,
            question_id: question_id.to_owned(),
            asker: asker.to_owned(),
            reason: reason.map(str::to_owned),
            kind: DeliveryKind::Answer,
        }
    }

    /// This line, marked as delivering `kind`.
    pub fn of_kind(self, kind: DeliveryKind) -> Self {
        Self { kind, ..self }
    }
}

/// Append `line` to `dir`'s delivery trace, durably, owner-only.
///
/// Why: #9873 Q5: the `pending` line must be on disk before the send, so a
/// crash mid-send leaves an unsettled line the doctor reports.
/// What: the #8545 home-write fence, the directory created 0700, then the
/// file opened for append (created 0600; no symlink followed, no wait on a
/// FIFO; anything but a regular file refused), the whole line in one write,
/// then `sync_data`. A non-empty file whose last byte is not a newline (a
/// torn append) gets a leading newline first, so the torn line cannot
/// swallow this record. Any step's error is returned.
/// Test: `delivery_trace_is_0600_and_one_line_per_step`,
/// `pending_write_failure_refuses_delivery`,
/// `torn_last_trace_line_never_swallows_the_next_record`.
pub fn record_delivery(dir: &Path, line: &DeliveryLine) -> Result<(), TaintError> {
    let path = dir.join(DELIVERY_FILE);
    crate::core::home_write_fence::check(&path);
    let io = |path: &Path, source| TaintError::Io {
        path: path.to_owned(),
        source,
    };
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| io(dir, e))?;
    let mut bytes = serde_json::to_vec(line).map_err(|e| TaintError::Malformed {
        path: path.clone(),
        reason: e.to_string(),
    })?;
    bytes.push(b'\n');
    std::fs::OpenOptions::new()
        .read(true)
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
        .and_then(|mut file| {
            // #9873 fix round: a torn last line must not swallow this one.
            if ends_unterminated(&file)? {
                bytes.insert(0, b'\n');
            }
            file.write_all(&bytes).and_then(|()| file.sync_data())
        })
        .map_err(|e| io(&path, e))
}

/// Whether regular file `file` is non-empty and its last byte is not `\n`.
fn ends_unterminated(file: &std::fs::File) -> std::io::Result<bool> {
    use std::os::unix::fs::FileExt as _;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::other(
            "the delivery trace is not a regular file",
        ));
    }
    let Some(last) = meta.len().checked_sub(1) else {
        return Ok(false);
    };
    let mut byte = [0u8; 1];
    file.read_exact_at(&mut byte, last)?;
    Ok(byte[0] != b'\n')
}

/// A read of the delivery trace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeliveryTrace {
    /// The lines that parse, in file order.
    pub lines: Vec<DeliveryLine>,
    /// One message per newline-terminated line that does not parse, naming
    /// its line number.
    pub malformed: Vec<String>,
}

/// Every line of `dir`'s delivery trace; an absent trace has none.
///
/// What: a read error other than absence is an `Err`. #9873 fix round: an
/// unterminated last line is a torn append (ENOSPC, a crash) and is skipped,
/// as `question_table::answered_in_log` does; a terminated line that does
/// not parse is named in [`DeliveryTrace::malformed`], and the read goes on.
/// Test: `delivery_trace_is_0600_and_one_line_per_step`,
/// `malformed_delivery_line_is_named_and_a_torn_last_line_is_skipped`,
/// `malformed_middle_trace_line_is_a_doctor_fail_naming_it`.
pub fn read_deliveries(dir: &Path) -> Result<DeliveryTrace, TaintError> {
    let path = dir.join(DELIVERY_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(DeliveryTrace::default()),
        Err(source) => return Err(TaintError::Io { path, source }),
    };
    let mut trace = DeliveryTrace::default();
    for (n, line) in bytes.split_inclusive(|b| *b == b'\n').enumerate() {
        if !line.ends_with(b"\n") || line.trim_ascii().is_empty() {
            continue;
        }
        match serde_json::from_slice(line) {
            Ok(parsed) => trace.lines.push(parsed),
            Err(e) => trace.malformed.push(format!(
                "{DELIVERY_FILE} line {}: malformed ({e}); a delivery record may be lost",
                n + 1
            )),
        }
    }
    Ok(trace)
}

/// The deliveries an operator must look at, one message each.
///
/// Why: #8433 Q4 makes a lost answer final, so it must be visible; Q4a
/// accepts the crash window between `resolve` and the taint, and this is
/// where it shows.
/// What: per question, its last line: `pending` is "delivery unconfirmed",
/// `failed` is "delivery failed (<reason>)". A `resolved` question (id,
/// asker) with no line at all is "resolved with no delivery record". Each
/// message names the question and the asker. #10127: answer lines only; a
/// notice line never settles or stands in for an answer.
/// Test: `delivery_problems_name_unsettled_failed_and_unrecorded`,
/// `delivered_notice_never_hides_a_resolved_answer_with_no_delivery`.
pub fn delivery_problems(lines: &[DeliveryLine], resolved: &[(String, String)]) -> Vec<String> {
    let last = last_lines(lines, DeliveryKind::Answer);
    let mut problems = Vec::new();
    for (id, line) in &last {
        let asker = &line.asker;
        match line.state {
            DeliveryState::Delivered => {}
            DeliveryState::Pending => problems.push(format!(
                "Q-{id} to asker {asker}: delivery unconfirmed (pending, never settled)"
            )),
            DeliveryState::Failed => problems.push(format!(
                "Q-{id} to asker {asker}: delivery failed ({}); the answer is lost",
                line.reason.as_deref().unwrap_or("unknown")
            )),
        }
    }
    for (id, asker) in resolved {
        if !last.contains_key(id.as_str()) {
            problems.push(format!(
                "Q-{id} to asker {asker}: resolved with no delivery record (the daemon stopped \
                 between resolve and taint)"
            ));
        }
    }
    problems
}

/// The held-reply notices an operator must look at, one message each
/// (#10127).
///
/// What: per question, its last notice line: `pending` is "notice
/// unconfirmed", `failed` is "notice failed (<reason>)"; each says the
/// question is still open. Answer lines are not read.
/// Test: `notice_send_failure_is_recorded_and_not_retried`.
pub fn notice_problems(lines: &[DeliveryLine]) -> Vec<String> {
    let mut problems = Vec::new();
    for (id, line) in last_lines(lines, DeliveryKind::Notice) {
        let asker = &line.asker;
        match line.state {
            DeliveryState::Delivered => {}
            DeliveryState::Pending => problems.push(format!(
                "Q-{id} to asker {asker}: held-reply notice unconfirmed (pending, never \
                 settled); the reply is held and the question is still open"
            )),
            DeliveryState::Failed => problems.push(format!(
                "Q-{id} to asker {asker}: held-reply notice failed ({}); the reply is held and \
                 the question is still open",
                line.reason.as_deref().unwrap_or("unknown")
            )),
        }
    }
    problems
}

/// The last line of `kind` per question id.
fn last_lines(lines: &[DeliveryLine], kind: DeliveryKind) -> BTreeMap<&str, &DeliveryLine> {
    let mut last = BTreeMap::new();
    for line in lines.iter().filter(|l| l.kind == kind) {
        last.insert(line.question_id.as_str(), line);
    }
    last
}

#[cfg(test)]
#[path = "channels_delivery_tests.rs"]
mod tests;
