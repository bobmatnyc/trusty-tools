//! The PM ledger's on-disk records: `state.json`, `archive.jsonl` and
//! `log.jsonl` lines, as the prototype writes them today (#9757 AC1).
//!
//! Why: the Rust port must read every live prototype store and write it back
//! without losing a field, so the dual-run diff (#9756 plan section 3) starts
//! from equal bytes.
//! What: serde types whose field order is the prototype's insertion order,
//! so `serde_json`'s compact output equals Python's
//! `json.dumps(..., separators=(",", ":"), ensure_ascii=False)`. Unknown
//! keys on a record (state, config, goal, item, task, option object, session
//! record, log and archive line) are kept in `extra` and written back after
//! the known keys; the session record's rows (waiting-on, owed, agents,
//! crons, refs, ctx use) are closed shapes. A new field is a schema bump ([`super::SCHEMA_VERSION`]).
//! Byte parity holds for the prototype's key orders; a session record whose
//! keys were inserted in another order re-serializes in this order.
//! Test: `ledger_goldens_round_trip_byte_identical`,
//! `ledger_reader_keeps_unknown_keys`, `ledger_option_extras_round_trip`,
//! `ledger_stale_il_line_round_trips`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ids::{Code, LedgerId, Prefix, hex2};
use crate::error::LedgerError;

/// Unknown keys of a record, kept for the round trip.
pub type Extra = BTreeMap<String, Value>;

/// `state.json`: one PM session's snapshot (LDG-2.1.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// Store schema; absent means 1 (#9757 AC4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<u64>,
    /// The session's id namespace.
    pub code: Code,
    /// Whole-file version, bumped by one per write (LDG-2.8.2).
    pub v: u64,
    /// Dispatch policy.
    #[serde(default)]
    pub config: Config,
    /// G records.
    pub goals: Vec<Goal>,
    /// D and A records.
    pub items: Vec<Item>,
    /// T records.
    pub tasks: Vec<Task>,
    /// Next id per prefix, two hex digits.
    pub next: Next,
    /// The session record; absent in a legacy store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionRecord>,
    /// Unknown keys.
    #[serde(flatten)]
    pub extra: Extra,
}

/// `state.json` `config`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Warn before this time, block after it, unless `mode` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_after: Option<String>,
    /// `warn` or `block`; overrides `block_after`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Unknown keys.
    #[serde(flatten)]
    pub extra: Extra,
}

/// A goal (G).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Goal {
    /// `<code>-G<hex2>`.
    pub id: String,
    /// 1 is first; only the owner changes it.
    pub rank: u32,
    /// One line.
    pub title: String,
    /// The finish line.
    pub finish: String,
    /// Lifecycle.
    pub status: GoalStatus,
    /// The Architect goal this maps to, or empty.
    pub maps: String,
    /// Unknown keys.
    #[serde(flatten)]
    pub extra: Extra,
}

/// A decision (D, kind question) or an ask (A, kind action).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    /// `<code>-D<hex2>` or `<code>-A<hex2>`.
    pub id: String,
    /// Question (D) or action (A).
    pub kind: ItemKind,
    /// Lifecycle.
    pub status: ItemStatus,
    /// The goal id it serves.
    pub goal: String,
    /// One line.
    pub title: String,
    /// The options, recommendation first.
    pub opts: Opts,
    /// The owner's answer, or empty.
    pub answer: String,
    /// The verified outcome, or empty.
    pub outcome: String,
    /// `architect` when escalated; written as `null` otherwise.
    pub esc: Option<String>,
    /// Unknown keys.
    #[serde(flatten)]
    pub extra: Extra,
}

/// A task (T).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    /// `<code>-T<hex2>`.
    pub id: String,
    /// The goal id it serves.
    pub goal: String,
    /// One line.
    pub title: String,
    /// Lifecycle.
    pub status: TaskStatus,
    /// Who it is with, or empty.
    pub to: String,
    /// The verified outcome, or empty.
    pub outcome: String,
    /// Unknown keys.
    #[serde(flatten)]
    pub extra: Extra,
}

macro_rules! status_enum {
    ($(#[$doc:meta])* $name:ident { $($variant:ident = $text:literal),+ }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $(#[doc = $text] $variant),+ }
    };
}

status_enum!(/// A goal's lifecycle.
    GoalStatus { Active = "active", Done = "done", Parked = "parked" });
status_enum!(/// A D or A item's lifecycle.
    ItemStatus { Open = "open", Answered = "answered", Done = "done" });
status_enum!(/// A task's lifecycle (#10062 gap G3: `running` is kept).
    TaskStatus { Open = "open", Running = "running", Done = "done" });
status_enum!(/// D items are questions, A items actions (#10062 gap G4).
    ItemKind { Question = "question", Action = "action" });

/// `opts` as stored: a list, or (in old stores) one string.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Opts {
    /// The batch form and the CLI's parsed form.
    List(Vec<Opt>),
    /// An early prototype form: the raw comma-separated text.
    Text(String),
}

/// One option: `{l, rec}`, or (in old stores) a bare label or `{label, rec?}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Opt {
    /// An option object.
    Object(OptObject),
    /// A bare label.
    Plain(String),
}

/// An option object; unknown keys are kept (#9757 AC1, critic round 1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptObject {
    /// Label, at most 160 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub l: Option<String>,
    /// The key the prototype once accepted for `l` (#9995); read and kept,
    /// never written by a Rust op.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// True for the recommendation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rec: Option<bool>,
    /// Unknown keys.
    #[serde(flatten)]
    pub extra: Extra,
}

/// `next`: the next id number per prefix, two hex digits (LDG-2.2.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Next {
    /// Goals.
    #[serde(rename = "G")]
    pub goal: String,
    /// Decisions.
    #[serde(rename = "D")]
    pub decision: String,
    /// Asks.
    #[serde(rename = "A")]
    pub ask: String,
    /// Tasks.
    #[serde(rename = "T")]
    pub task: String,
    /// Briefs (#9759); absent in prototype stores.
    #[serde(rename = "B", default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<String>,
    /// Unknown keys.
    #[serde(flatten)]
    pub extra: Extra,
}

impl Default for Next {
    fn default() -> Self {
        let one = || "01".to_owned();
        Self {
            goal: one(),
            decision: one(),
            ask: one(),
            task: one(),
            brief: None,
            extra: Extra::new(),
        }
    }
}

impl Next {
    fn slot(&mut self, prefix: Prefix) -> &mut String {
        match prefix {
            Prefix::Goal => &mut self.goal,
            Prefix::Decision => &mut self.decision,
            Prefix::Ask => &mut self.ask,
            Prefix::Task => &mut self.task,
            Prefix::Brief => self.brief.get_or_insert_with(|| "01".to_owned()),
        }
    }
}

/// The session record (LDG-2.4.1), rewritten whole, at most 2 KB.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[allow(missing_docs)] // field names are the contract; DOC-82 LDG-2.4.1 defines them
pub struct SessionRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub written_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub written_epoch: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_on: Option<Vec<WaitingOn>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owed: Option<Vec<Owed>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owed_none: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents: Option<Vec<Agent>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crons: Option<Vec<Cron>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundary_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx_use: Option<CtxUse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refs: Option<Vec<SessionRef>>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The session record's row shapes, closed (unknown keys are dropped).
macro_rules! row {
    ($(#[$doc:meta])* $name:ident { $($(#[$f:meta])* $field:ident: $ty:ty),+ }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[allow(missing_docs)]
        pub struct $name { $($(#[$f])* pub $field: $ty),+ }
    };
}

row!(/// `waiting_on` row: `who` is owner, architect, ci, agent or other.
    WaitingOn { who: String, what: String, since: String });
row!(/// `owed` row: an exact expected value, never a vague word.
    Owed { check: String, expect: String, #[serde(rename = "ref")] reference: String });
row!(/// `agents` row.
    Agent { id: String, goal: String, what: String });
row!(/// `crons` row.
    Cron { id: String, schedule: String, prompt: String });
row!(/// `refs` row: a named pointer.
    SessionRef { key: String, value: String });
row!(/// `ctx_use`: self-reported context use at a boundary.
    CtxUse { at: String, active: u8, stale: u8, ctx: u64, keep: String });

/// One `archive.jsonl` line: a closed record, written before its removal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchiveLine {
    /// When it was archived.
    pub archived_at: String,
    /// When it was closed.
    pub closed_at: String,
    /// `item` or `task`.
    pub kind: String,
    /// The record as it stood.
    pub item: Archived,
    /// The `state.json` `v` of the commit; a line above the store's `v` is
    /// an orphan of a failed commit.
    pub cv: u64,
    /// Unknown keys.
    #[serde(flatten)]
    pub extra: Extra,
}

/// The record in an archive line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Archived {
    /// A D or A item.
    Item(Item),
    /// A task.
    Task(Task),
}

/// One `log.jsonl` line, in either prototype shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LogLine {
    /// `{t, id, ev, note?}`: a record event.
    Event {
        /// Time.
        t: String,
        /// Record id, `<code>-init` or `<code>-session`.
        id: String,
        /// open, escalated, answered, done, drift, reprio or update.
        ev: String,
        /// The title or a note.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        /// Unknown keys.
        #[serde(flatten)]
        extra: Extra,
    },
    /// `{ts, session, kind, text?, n?}`: a notice (size_warning, stale_il, …).
    Notice {
        /// Time.
        ts: String,
        /// Session name, or empty.
        session: String,
        /// The notice kind.
        kind: String,
        /// Consecutive stale turns (`stale_il`); before `text`, as the
        /// prototype writes it (critic round 1).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        n: Option<u64>,
        /// The text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        /// Unknown keys.
        #[serde(flatten)]
        extra: Extra,
    },
}

impl State {
    /// An empty schema-1 store for `code` (what `init` writes).
    pub fn new(code: Code) -> Self {
        Self {
            schema: Some(super::SCHEMA_VERSION),
            code,
            v: 0,
            config: Config::default(),
            goals: Vec::new(),
            items: Vec::new(),
            tasks: Vec::new(),
            next: Next::default(),
            session: None,
            extra: Extra::new(),
        }
    }

    /// Parses `state.json` text.
    ///
    /// # Errors
    /// [`LedgerError::Json`] when the text is not the contract's shape.
    pub fn from_json(text: &str) -> Result<Self, LedgerError> {
        Ok(serde_json::from_str(text)?)
    }

    /// `state.json` text: compact JSON and a newline, the prototype's form.
    ///
    /// # Errors
    /// [`LedgerError::Json`] if a value cannot be serialized.
    pub fn to_json(&self) -> Result<String, LedgerError> {
        Ok(serde_json::to_string(self)? + "\n")
    }

    /// The store's schema; absent is 1.
    pub fn schema_version(&self) -> u64 {
        self.schema.unwrap_or(1)
    }

    /// Takes the next free id of `prefix` and advances `next` (LDG-2.2.3):
    /// the first number at or after `next` that is not live, wrapping to the
    /// lowest such number. Ids are ephemeral, so an archived id is free again
    /// and is reissued (#10235, Bob ruling 2026-10-24).
    ///
    /// # Errors
    /// [`LedgerError::Invalid`] when `next` is not `01`..`ff`;
    /// [`LedgerError::NoFreeId`] when every number is live. `next` is
    /// unchanged on error.
    pub fn allocate(&mut self, prefix: Prefix) -> Result<LedgerId, LedgerError> {
        let ids = self.goals.iter().map(|g| &g.id);
        let ids = ids.chain(self.items.iter().map(|i| &i.id));
        let live = ids
            .chain(self.tasks.iter().map(|t| &t.id))
            .filter_map(|id| LedgerId::parse(id).ok());
        let used: Vec<u8> = live
            .filter(|id| id.code() == &self.code && id.prefix() == prefix)
            .map(|id| id.number())
            .collect();
        let slot = self.next.slot(prefix);
        // #9757 critic round 1: a malformed counter is refused, never read as 01.
        let start = hex2(slot).filter(|n| *n >= 1).ok_or_else(|| {
            LedgerError::Invalid(format!("next.{} is {slot:?}; want 01..ff", prefix.letter()))
        })?;
        let free = (start..=255)
            .chain(1..start)
            .find(|n| !used.contains(n))
            .ok_or(LedgerError::NoFreeId(prefix.letter()))?;
        // As the prototype: after `ff` the counter stays at `ff`.
        *slot = format!("{:02x}", free.saturating_add(1));
        Ok(LedgerId::new(self.code.clone(), prefix, free))
    }
}

impl ArchiveLine {
    /// Parses one line (no newline).
    ///
    /// # Errors
    /// [`LedgerError::Json`] when the line is not an archive line.
    pub fn from_line(line: &str) -> Result<Self, LedgerError> {
        Ok(serde_json::from_str(line)?)
    }

    /// The line, compact, without a newline.
    ///
    /// # Errors
    /// [`LedgerError::Json`] if a value cannot be serialized.
    pub fn to_line(&self) -> Result<String, LedgerError> {
        Ok(serde_json::to_string(self)?)
    }
}

impl LogLine {
    /// Parses one line (no newline).
    ///
    /// # Errors
    /// [`LedgerError::Json`] when the line is neither log shape.
    pub fn from_line(line: &str) -> Result<Self, LedgerError> {
        Ok(serde_json::from_str(line)?)
    }

    /// The line, compact, without a newline.
    ///
    /// # Errors
    /// [`LedgerError::Json`] if a value cannot be serialized.
    pub fn to_line(&self) -> Result<String, LedgerError> {
        Ok(serde_json::to_string(self)?)
    }
}
