//! Append-only JSONL audit logger (overseer decisions + peer-bus envelopes).
//!
//! Why: every overseer decision (allow / block / respond / flag) must leave a
//! durable, append-only trail an operator can review — both for security
//! forensics and to tune the policy. A daily JSONL file under the daemon's log
//! directory keeps the trail greppable and rotation-free. DOC-60 §9 requires
//! exactly that shape for peer-bus traffic too, and says so explicitly:
//! "redirect the existing audit logger, don't build a new store". This module
//! is therefore the ONE JSONL writer in the daemon; the bus reaches it through
//! [`AuditLogger::for_stream`] rather than shipping a second implementation of
//! a capability that already exists (common-entry-point principle).
//! What: [`AuditLogger`] resolves a `logs_dir/<stream>/YYYY-MM-DD.jsonl` path
//! (`overseer` for decisions, `bus` for envelopes) and appends one serialized
//! line per call, never propagating IO errors (oversight must not break the
//! hook hot path). [`AuditLogger::log`] is the typed overseer entry point;
//! [`AuditLogger::log_record`] is the generic one every other stream uses.
//! Test: `cargo test -p trusty-mpm-daemon audit` writes entries to a temp
//! directory and reads them back as valid JSONL; `for_stream_resolves_named_path`
//! and `log_record_writes_arbitrary_serializable` cover the bus stream.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::core::overseer::{OverseerContext, OverseerDecision};
use serde::{Deserialize, Serialize};

/// Log-directory subdirectory for overseer decision records.
pub const OVERSEER_STREAM: &str = "overseer";

/// Log-directory subdirectory for peer-bus envelope records (DOC-60 §9).
pub const BUS_STREAM: &str = "bus";

/// One audited overseer decision, serialized as a single JSONL line.
///
/// Why: a flat, self-describing record lets log consumers filter by session,
/// event, decision, or handler without joining against other state.
/// What: an RFC3339 timestamp, the session's tmux name, the hook event, the
/// optional tool, the decision tag, its reason, and which overseer produced it.
/// Test: `entry_serializes_to_json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditEntry {
    /// Decision timestamp, RFC3339.
    pub ts: String,
    /// Friendly tmux session name the decision applied to.
    pub session: String,
    /// Hook event: `"PreToolUse" | "PostToolUse" | "SessionQuestion"`.
    pub event: String,
    /// Tool name involved, if any.
    pub tool: Option<String>,
    /// Decision tag: `"allow" | "block" | "respond" | "flag"`.
    pub decision: String,
    /// Human-readable reason / response / summary for the decision.
    pub reason: String,
    /// Which overseer produced the decision: `"deterministic" | "llm" |
    /// "auto_responder"`.
    pub handler: String,
}

impl AuditEntry {
    /// Build an audit entry from an overseer call's context and verdict.
    ///
    /// Why: every call site that logs a decision needs the same field mapping
    /// (decision → tag/reason, timestamp → now); centralizing it prevents
    /// drift between the three hook paths.
    /// What: stamps `ts` to the current UTC time and copies the session name,
    /// tool, decision tag, and reason out of the inputs.
    /// Test: `entry_from_context_maps_fields`.
    pub fn from_decision(
        ctx: &OverseerContext,
        event: &str,
        decision: &OverseerDecision,
        handler: &str,
    ) -> Self {
        Self {
            ts: chrono::Utc::now().to_rfc3339(),
            session: ctx.tmux_name.clone(),
            event: event.to_string(),
            tool: ctx.tool_name.clone(),
            decision: decision.tag().to_string(),
            reason: decision.reason().to_string(),
            handler: handler.to_string(),
        }
    }
}

/// Append-only JSONL audit logger for one named daemon stream.
///
/// Why: the daemon needs a cheap, fire-and-forget sink for durable records;
/// holding only the resolved file path keeps the logger trivially `Clone`-free
/// and shareable behind an `Arc`. One logger type serves every stream so a new
/// durable record type (DOC-60 §9's bus envelopes) reuses this writer instead
/// of adding a parallel one.
/// What: stores the day's `<stream>/YYYY-MM-DD.jsonl` path under the configured
/// logs directory; [`log_record`](Self::log_record) opens-appends-closes per
/// call.
///
/// **Rotation caveat (pre-existing, inherited by every stream):** the date is
/// resolved once, at construction, so a daemon running across midnight keeps
/// appending to the day it started under. This is the shipped overseer
/// behavior and is deliberately NOT changed here — fixing it would alter the
/// overseer stream's on-disk layout, which is outside this change's scope.
/// Test: `log_writes_jsonl_line`, `log_appends_multiple_lines`,
/// `for_stream_resolves_named_path`.
#[derive(Debug, Clone)]
pub struct AuditLogger {
    /// Resolved JSONL file path for the current day.
    path: PathBuf,
}

impl AuditLogger {
    /// Create a logger writing to `logs_dir/overseer/YYYY-MM-DD.jsonl`.
    ///
    /// Why: a per-day file gives natural rotation without a rotation cron; the
    /// `overseer/` subdirectory keeps audit logs separate from other daemon
    /// logs.
    /// What: delegates to [`for_stream`](Self::for_stream) with
    /// [`OVERSEER_STREAM`].
    /// Test: `new_resolves_dated_path`.
    pub fn new(logs_dir: &Path) -> Self {
        Self::for_stream(logs_dir, OVERSEER_STREAM)
    }

    /// Create a logger writing to `logs_dir/<stream>/YYYY-MM-DD.jsonl`.
    ///
    /// Why: DOC-60 §9 puts peer-bus envelopes in a `bus/` stream *alongside*
    /// the existing `overseer/` one — same writer, same durability discipline,
    /// different subdirectory. Parameterizing the subdirectory is the whole
    /// change that requirement needs.
    /// What: resolves the dated path under `<logs_dir>/<stream>/`; the
    /// directory is created lazily on the first write so constructing a logger
    /// never performs IO.
    /// Test: `for_stream_resolves_named_path`.
    pub fn for_stream(logs_dir: &Path, stream: &str) -> Self {
        let date = chrono::Utc::now().format("%Y-%m-%d");
        let path = logs_dir.join(stream).join(format!("{date}.jsonl"));
        Self { path }
    }

    /// The resolved JSONL file path this logger appends to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one audit entry as a JSONL line.
    ///
    /// Why: oversight must never break the hook relay; a failed audit write is
    /// logged and swallowed rather than propagated.
    /// What: ensures the parent directory exists, then opens the dated file in
    /// append mode and writes `<json>\n`. All IO errors are logged via
    /// `tracing::warn!` and discarded.
    /// Test: `log_writes_jsonl_line`, `log_appends_multiple_lines`.
    pub fn log(&self, entry: AuditEntry) {
        self.log_record(&entry);
    }

    /// Append any serializable record as a JSONL line.
    ///
    /// Why: the peer bus (DOC-60 §9) writes envelopes, not overseer decisions,
    /// to its own stream. Making the append generic over `Serialize` lets it
    /// reuse this writer verbatim rather than duplicating the
    /// ensure-dir/open-append/write-line discipline in a second module.
    /// What: serializes `record` and appends `<json>\n`, logging and swallowing
    /// every IO error exactly as [`log`](Self::log) does — a failed durability
    /// write must never break the caller's hot path.
    /// Test: `log_record_writes_arbitrary_serializable`.
    pub fn log_record<T: Serialize>(&self, record: &T) {
        if let Err(e) = self.try_log(record) {
            tracing::warn!("audit write to {} failed: {e}", self.path.display());
        }
    }

    /// Fallible core of [`log_record`](Self::log_record), separated for testability.
    ///
    /// #4271: the record and its newline go out in ONE `write_all` on an
    /// append-only descriptor. `writeln!` issued them as separate writes — a
    /// `File` is unbuffered, so `write_fmt` calls through once per fragment —
    /// and two threads appending at once interleaved into a line no reader
    /// could parse. Two concurrent bus publishes are an ordinary event, so the
    /// stream this daemon relies on to answer "sent, or never read?" has to
    /// survive them.
    /// Test: `concurrent_publishers_lose_nothing_without_a_record` — it parses
    /// every line the concurrent publishes produced, so an interleaved one
    /// fails it before its accounting runs.
    fn try_log<T: Serialize>(&self, entry: &T) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut line = serde_json::to_string(entry)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        line.push('\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(line.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::session::SessionId;

    fn sample_entry() -> AuditEntry {
        AuditEntry {
            ts: "2026-05-16T00:00:00Z".into(),
            session: "tmpm-test-session".into(),
            event: "PreToolUse".into(),
            tool: Some("Bash".into()),
            decision: "block".into(),
            reason: "matched blocklist".into(),
            handler: "deterministic".into(),
        }
    }

    #[test]
    fn new_resolves_dated_path() {
        let dir = tempfile::tempdir().unwrap();
        let logger = AuditLogger::new(dir.path());
        let path = logger.path();
        assert!(path.starts_with(dir.path().join("overseer")));
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("jsonl"));
        // Constructing the logger must not have created anything yet.
        assert!(!path.exists());
    }

    #[test]
    fn for_stream_resolves_named_path() {
        // DOC-60 §9's `bus/` stream is a sibling of `overseer/`, same writer.
        let dir = tempfile::tempdir().unwrap();
        let logger = AuditLogger::for_stream(dir.path(), BUS_STREAM);
        assert!(logger.path().starts_with(dir.path().join("bus")));
        assert_eq!(
            logger.path().extension().and_then(|e| e.to_str()),
            Some("jsonl")
        );
        assert!(!logger.path().exists());
    }

    #[test]
    fn log_record_writes_arbitrary_serializable() {
        // The generic append is what lets the bus reuse this writer; it must
        // accept a record type this module knows nothing about.
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Foreign {
            message_id: String,
        }
        let dir = tempfile::tempdir().unwrap();
        let logger = AuditLogger::for_stream(dir.path(), BUS_STREAM);
        let record = Foreign {
            message_id: "01J-test".into(),
        };
        logger.log_record(&record);

        let contents = std::fs::read_to_string(logger.path()).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 1);
        let back: Foreign = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(back, record);
    }

    #[test]
    fn entry_serializes_to_json() {
        let json = serde_json::to_string(&sample_entry()).unwrap();
        let back: AuditEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back, sample_entry());
    }

    #[test]
    fn entry_from_context_maps_fields() {
        let ctx = OverseerContext::new(SessionId::new(), "tmpm-mapped", Some("Edit".into()), None);
        let decision = OverseerDecision::Block {
            reason: "danger".into(),
        };
        let entry = AuditEntry::from_decision(&ctx, "PreToolUse", &decision, "deterministic");
        assert_eq!(entry.session, "tmpm-mapped");
        assert_eq!(entry.tool.as_deref(), Some("Edit"));
        assert_eq!(entry.decision, "block");
        assert_eq!(entry.reason, "danger");
        assert!(!entry.ts.is_empty());
    }

    #[test]
    fn log_writes_jsonl_line() {
        // A single log() call must produce one parseable JSONL line.
        let dir = tempfile::tempdir().unwrap();
        let logger = AuditLogger::new(dir.path());
        logger.log(sample_entry());

        let contents = std::fs::read_to_string(logger.path()).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 1);
        let parsed: AuditEntry = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed, sample_entry());
    }

    #[test]
    fn log_appends_multiple_lines() {
        // Repeated log() calls append rather than truncate.
        let dir = tempfile::tempdir().unwrap();
        let logger = AuditLogger::new(dir.path());
        for _ in 0..3 {
            logger.log(sample_entry());
        }
        let contents = std::fs::read_to_string(logger.path()).unwrap();
        assert_eq!(contents.lines().count(), 3);
        for line in contents.lines() {
            // Every line must independently parse as a valid entry.
            let _: AuditEntry = serde_json::from_str(line).unwrap();
        }
    }
}
