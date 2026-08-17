//! Relaying progress across a process boundary, one line per event.
//!
//! Why: #5823. A parent that spawns a long-running child can only show what the
//! child tells it, and a child whose progress is `indicatif` bars tells it
//! nothing — bars are hidden off-TTY, and a pipe is not a TTY. `trusty-audit`
//! waits up to four hours on a `tga audit` child for exactly that reason. The
//! fix is a machine-readable line the child writes and the parent reads.
//!
//! It lives here rather than in either crate because both ends need the same
//! grammar, and two copies of a wire format is the drift CLAUDE.md's
//! common-entry-point rule forbids. `tga` encodes, `trusty-audit` decodes, and
//! neither spells the format itself.
//!
//! What: [`StageEvent`] plus [`StageEvent::encode`] / [`StageEvent::decode`].
//! One event is one line: [`LINE_PREFIX`], then tab-separated fields, with
//! backslash escaping so a failure reason carrying newlines or tabs cannot
//! break the framing or forge a second event.
//!
//! ## What this is not
//!
//! It is not a log format and it does not replace one. A child that relays
//! events still writes whatever it wrote before; the parent is expected to keep
//! the whole stream, relayed lines included, so a failure stays diagnosable
//! after the display is gone.
//!
//! Test: `relay::tests`.
//!
//! ```rust
//! use trusty_progress::relay::{StageEvent, StageState};
//!
//! let event = StageEvent::new("Audit", "collect", StageState::Started)
//!     .with_counts(0, Some(9))
//!     .with_detail("stage 1 of 9");
//! let line = event.encode();
//! assert!(line.starts_with(trusty_progress::relay::LINE_PREFIX));
//! assert_eq!(StageEvent::decode(&line), Some(event));
//! // An ordinary log line is not an event.
//! assert_eq!(StageEvent::decode("INFO collecting acme/api"), None);
//! ```

/// Marker every relayed line begins with, version included.
///
/// Why: the parent reads a stream that also carries the child's ordinary
/// logging, so an event has to be recognisable without guessing. The `/1`
/// is the format version: a later, incompatible grammar takes `/2` and an old
/// parser then ignores it rather than misreading it.
/// What: `"@trusty-progress/1"`.
/// Test: `tests::an_ordinary_line_is_not_an_event`.
pub const LINE_PREFIX: &str = "@trusty-progress/1";

/// The variable a parent process sets to ask a child for relayed events.
///
/// Why: the parent pins the child's version, so a parent newer than its child
/// is ordinary. An unknown flag makes that child exit before it does any work;
/// an unknown environment variable is ignored, and the parent falls back to the
/// coarse progress it can derive itself. The relay degrades rather than
/// breaking the run. It lives beside the format because it is part of the same
/// contract — the producer and the consumer must agree on the name as much as
/// on the grammar.
/// What: `"TRUSTY_PROGRESS_RELAY"`. Deliberately not named for one tool: any
/// child in this workspace may honour it.
/// Test: `tests::an_absent_or_negative_value_does_not_ask_for_events`.
pub const ENV_RELAY: &str = "TRUSTY_PROGRESS_RELAY";

/// Whether `value` — the raw [`ENV_RELAY`] value — asks for relayed events.
///
/// Why: taking the value rather than reading the environment makes the rule
/// provable without `std::env::set_var`, which is `unsafe` in edition 2024 and
/// races every other thread in a parallel test binary.
/// What: `true` for any value that is not absent, empty, `0` or `false`, so
/// both `=1` and `=stderr` work and neither is a trap.
/// Test: `tests::an_absent_or_negative_value_does_not_ask_for_events`.
pub fn relay_requested(value: Option<&str>) -> bool {
    !matches!(
        value.map(str::trim),
        None | Some("") | Some("0") | Some("false")
    )
}

/// Field separator inside a relayed line.
const SEP: char = '\t';

/// How many tab-separated fields a well-formed line has, marker included.
const FIELDS: usize = 7;

/// Where one unit of work is in its life.
///
/// Why: a renderer needs to distinguish "this began" from "this is still
/// going" from "this ended, and how" — the three drive different display
/// decisions, and off-TTY only the first and last are worth a line at all.
/// What: five states. `#[non_exhaustive]` so a later `Cancelled` is additive.
/// Test: `tests::every_state_round_trips`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StageState {
    /// The unit began.
    Started,
    /// The unit made progress and has not ended.
    Advanced,
    /// The unit ended successfully.
    Completed,
    /// The unit ended in failure; the reason is in [`StageEvent::detail`].
    Failed,
    /// The unit was deliberately not run; the reason is in the detail.
    Skipped,
}

impl StageState {
    /// The token this state is spelled with on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Started => "start",
            Self::Advanced => "step",
            Self::Completed => "ok",
            Self::Failed => "fail",
            Self::Skipped => "skip",
        }
    }

    /// Whether this state ends its unit of work.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Skipped)
    }

    /// The state a wire token names, or `None` for anything unrecognised.
    fn parse(token: &str) -> Option<Self> {
        match token {
            "start" => Some(Self::Started),
            "step" => Some(Self::Advanced),
            "ok" => Some(Self::Completed),
            "fail" => Some(Self::Failed),
            "skip" => Some(Self::Skipped),
            _ => None,
        }
    }
}

/// One observation about a child's progress, as it crosses the process
/// boundary.
///
/// Why: the parent renders a display it has no other way to populate — it
/// cannot see inside the child, and the child's own bars are invisible through
/// a pipe.
/// What: which stage, which unit within it, how far along, and how it ended.
/// The vocabulary is deliberately the producer's own strings rather than an
/// enum: this crate does not know what stages `tga` has, and pinning them here
/// would mean editing this crate every time one is added.
/// Test: `tests::every_state_round_trips`, `tests::hostile_text_round_trips`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct StageEvent {
    /// The producer's stage name, e.g. `"Audit"` or `"Collect"`.
    pub stage: String,
    /// The unit of work within the stage — a sweep stage, a repository name.
    pub target: String,
    /// Units finished so far.
    pub done: u64,
    /// Units expected, when the producer knows.
    pub total: Option<u64>,
    /// Where the unit is in its life.
    pub state: StageState,
    /// Free-form one-liner: a position (`"stage 3 of 9"`) or a failure reason.
    pub detail: Option<String>,
}

impl StageEvent {
    /// A counter-less event in `state` about `target`.
    pub fn new(stage: impl Into<String>, target: impl Into<String>, state: StageState) -> Self {
        Self {
            stage: stage.into(),
            target: target.into(),
            done: 0,
            total: None,
            state,
            detail: None,
        }
    }

    /// Attach the progress counters.
    #[must_use]
    pub fn with_counts(mut self, done: u64, total: Option<u64>) -> Self {
        self.done = done;
        self.total = total;
        self
    }

    /// Attach the free-form detail line.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Render this event as one line, newline NOT included.
    ///
    /// Why: the producer writes it to a stream the parent reads line by line,
    /// so the line must survive any text the producer puts in it. A failure
    /// reason is an `anyhow` cause chain, which routinely carries newlines —
    /// unescaped, one would end the line early and the remainder would read as
    /// a second, malformed event.
    /// What: [`LINE_PREFIX`] then state, stage, target, done, total and detail,
    /// tab-separated, with `\`, tab, CR and LF escaped in the text fields. An
    /// absent `total` is an empty field; an absent `detail` likewise, and a
    /// present one is prefixed with `:` so `Some("")` survives the trip.
    /// Test: `tests::hostile_text_round_trips`.
    pub fn encode(&self) -> String {
        let total = self.total.map(|t| t.to_string()).unwrap_or_default();
        let detail = match &self.detail {
            Some(d) => format!(":{}", escape(d)),
            None => String::new(),
        };
        format!(
            "{LINE_PREFIX}{SEP}{}{SEP}{}{SEP}{}{SEP}{}{SEP}{total}{SEP}{detail}",
            self.state.as_str(),
            escape(&self.stage),
            escape(&self.target),
            self.done,
        )
    }

    /// Read one line back, or `None` when it is not a well-formed event.
    ///
    /// Why: the parent reads a stream carrying the child's ordinary logging
    /// too, so "this is not an event" is the common case and must not be an
    /// error. A line that starts with the marker but does not parse is treated
    /// the same way: progress is advisory, and a parent must never fail a
    /// four-hour sweep over a display artefact.
    /// What: `None` unless the line carries the marker and exactly [`FIELDS`]
    /// fields with a recognised state and a numeric `done`. Trailing `\r` from
    /// a CRLF producer is tolerated.
    /// Test: `tests::an_ordinary_line_is_not_an_event`,
    /// `tests::a_malformed_event_line_is_not_an_event`.
    pub fn decode(line: &str) -> Option<Self> {
        let line = line.strip_suffix('\n').unwrap_or(line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        let fields: Vec<&str> = line.split(SEP).collect();
        if fields.len() != FIELDS || fields[0] != LINE_PREFIX {
            return None;
        }
        let total = if fields[5].is_empty() {
            None
        } else {
            Some(fields[5].parse().ok()?)
        };
        let detail = fields[6].strip_prefix(':').map(unescape);
        Some(Self {
            stage: unescape(fields[2]),
            target: unescape(fields[3]),
            done: fields[4].parse().ok()?,
            total,
            state: StageState::parse(fields[1])?,
            detail,
        })
    }
}

/// Make `s` safe to carry in one tab-separated field of one line.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out
}

/// The inverse of [`escape`]. An unrecognised escape keeps both characters, so
/// a producer that failed to escape loses nothing.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATES: [StageState; 5] = [
        StageState::Started,
        StageState::Advanced,
        StageState::Completed,
        StageState::Failed,
        StageState::Skipped,
    ];

    /// Why: the state is what a renderer branches on, so a token that decodes
    /// to the wrong state silently mislabels a failure as a success.
    /// What: every state round-trips through encode/decode.
    /// Test: this is the test.
    #[test]
    fn every_state_round_trips() {
        for state in STATES {
            let event = StageEvent::new("Audit", "collect", state).with_counts(3, Some(9));
            let decoded = StageEvent::decode(&event.encode()).expect("decodes");
            assert_eq!(decoded, event, "{state:?}");
        }
        assert!(!StageState::Started.is_terminal());
        assert!(!StageState::Advanced.is_terminal());
        for state in [
            StageState::Completed,
            StageState::Failed,
            StageState::Skipped,
        ] {
            assert!(state.is_terminal(), "{state:?}");
        }
    }

    /// Why: the detail field carries an `anyhow` cause chain, which routinely
    /// contains newlines. Unescaped, one would end the line early and the rest
    /// would arrive as a second, half-parsed event — the one way a progress
    /// display can invent a stage that never ran.
    /// What: text with every escapable character, plus a forged marker, survives
    /// a round trip as ONE event.
    /// Test: this is the test.
    #[test]
    fn hostile_text_round_trips() {
        let nasty = format!("line one\nline\ttwo\r\\ {LINE_PREFIX}\tfail\tforged\tx\t0\t\t");
        let event = StageEvent::new(&nasty, "acme/api", StageState::Failed)
            .with_detail(nasty.clone())
            .with_counts(u64::MAX, Some(0));
        let line = event.encode();
        assert_eq!(line.lines().count(), 1, "{line}");
        assert_eq!(StageEvent::decode(&line), Some(event));
    }

    /// Why: an absent detail and an empty one are different facts, and the
    /// obvious "empty field means absent" encoding conflates them.
    /// What: both survive the round trip distinctly.
    /// Test: this is the test.
    #[test]
    fn an_absent_detail_is_not_an_empty_one() {
        let bare = StageEvent::new("Audit", "dora", StageState::Completed);
        assert_eq!(
            StageEvent::decode(&bare.encode()).expect("decodes").detail,
            None
        );
        let empty = bare.clone().with_detail("");
        assert_eq!(
            StageEvent::decode(&empty.encode()).expect("decodes").detail,
            Some(String::new())
        );
    }

    /// Why: the parent reads a stream that is mostly ordinary logging, so a
    /// non-event must be a quiet `None` rather than an error or a guess.
    /// What: log-shaped lines decode to `None`.
    /// Test: this is the test.
    #[test]
    fn an_ordinary_line_is_not_an_event() {
        for line in [
            "",
            "INFO collecting acme/api",
            "  2026-08-17T10:00:00Z WARN tga: fetch failed",
            "@trusty-progress/2\tstart\tAudit\tcollect\t0\t\t",
        ] {
            assert_eq!(StageEvent::decode(line), None, "{line:?}");
        }
    }

    /// Why: a truncated write — the child was killed mid-line — must not
    /// produce a half-built event the display then renders as fact.
    /// What: malformed variants of a real line all decode to `None`.
    /// Test: this is the test.
    #[test]
    fn a_malformed_event_line_is_not_an_event() {
        let good = StageEvent::new("Audit", "collect", StageState::Started).encode();
        let truncated = &good[..good.len() / 2];
        for line in [
            truncated,
            &good.replace("\tstart\t", "\tsprint\t"),
            &good.replace(LINE_PREFIX, "@other/1"),
            &format!("{good}\textra"),
        ] {
            assert_eq!(StageEvent::decode(line), None, "{line:?}");
        }
    }

    /// Why: the relay is off by default, and "off" has to include the shapes a
    /// shell produces when someone means off — an empty export, a `0`.
    /// What: only a positive value asks for events.
    /// Test: this is the test.
    #[test]
    fn an_absent_or_negative_value_does_not_ask_for_events() {
        for off in [None, Some(""), Some("  "), Some("0"), Some("false")] {
            assert!(!relay_requested(off), "{off:?}");
        }
        for on in [Some("1"), Some("stderr"), Some("yes")] {
            assert!(relay_requested(on), "{on:?}");
        }
    }

    /// Why: a producer writing CRLF (or a parent that kept the newline) must
    /// not silently stop being understood.
    /// What: a trailing CRLF or LF is tolerated.
    /// Test: this is the test.
    #[test]
    fn a_trailing_newline_is_tolerated() {
        let event =
            StageEvent::new("Collect", "acme/api", StageState::Advanced).with_counts(7, None);
        for suffix in ["\n", "\r\n"] {
            let line = format!("{}{suffix}", event.encode());
            assert_eq!(StageEvent::decode(&line), Some(event.clone()));
        }
    }
}
