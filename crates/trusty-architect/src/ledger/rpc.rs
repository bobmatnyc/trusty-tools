//! Request and response types of the `mpm.dbatg.*` daemon methods (#9758).
//!
//! Why: the daemon method, the MCP tool and `tm dbatg` must share one schema,
//! limits and exit codes (LDG-3.3); defining the types in the leaf crate lets
//! all three import them before any of them exists (#9756 P0).
//! What: [`METHODS`] (the owner's `mpm.<family>.<verb>` names; DOC-82 amendment in #10068), one
//! request and response type per method, and [`Op`], the batch op. Requests
//! refuse unknown keys (#9995 AC5: an unknown option key exits 2), unlike the
//! stored records, which keep them. [`Op::check`] applies the field limits
//! before any write. [`StoreRef::paths`] normalises the project path. The
//! daemon serves `init`, `batch` and `show` (#9758); the rest is #9995.
//! Test: `ledger_rpc_types_use_the_mpm_prefix_and_round_trip`,
//! `ledger_request_refuses_unknown_keys`, `ledger_title_and_paragraph_limits`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::ids::Prefix;
use super::limits::{PARAGRAPH_MAX, check_paragraph, check_title};
use super::store::{StorePaths, resolve_project};
use crate::error::LedgerError;

/// Every `mpm.dbatg.*` method, in the plan's order.
pub const METHODS: &[&str] = &[
    "mpm.dbatg.init",
    "mpm.dbatg.batch",
    "mpm.dbatg.show",
    "mpm.dbatg.digest",
    "mpm.dbatg.fresh",
    "mpm.dbatg.archive_show",
    "mpm.dbatg.stamp",
    "mpm.dbatg.check_dispatch",
    "mpm.dbatg.rotate",
];

/// A method's error class and the prototype exit code it keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RpcErrorCode {
    /// `if_v` did not match the store's `v`; nothing written (exit 1).
    Conflict,
    /// Invalid input or a refused store; nothing written (exit 2).
    Invalid,
    /// The store could not be read or locked (exit 3).
    Unavailable,
}

impl RpcErrorCode {
    /// The CLI exit code.
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Conflict => 1,
            Self::Invalid => 2,
            Self::Unavailable => 3,
        }
    }
}

/// Names one session's store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreRef {
    /// The launch directory.
    pub project: PathBuf,
    /// The session name.
    pub session: String,
}

macro_rules! message {
    ($(#[$doc:meta])* $name:ident { $($(#[$f:meta])* $field:ident: $ty:ty),* $(,)? }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        #[allow(missing_docs)] // fields are the plan's method table
        pub struct $name { $($(#[$f])* pub $field: $ty),* }
    };
}

message!(/// `mpm.dbatg.init`: same code and session is a no-op.
    InitRequest { project: PathBuf, session: String, code: String });
message!(/// `mpm.dbatg.init` reply.
    InitResponse { store: StoreRef, v: u64 });
message!(/// `mpm.dbatg.batch`: all ops or none; a retried `request_id` returns the first result.
    BatchRequest { store: StoreRef, request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")] if_v: Option<u64>, ops: Vec<Op> });
message!(/// `mpm.dbatg.batch` reply: new `v`, the events logged, the ids archived.
    BatchResponse { v: u64, events: Vec<BatchEvent>, archived: Vec<String>, warnings: Vec<String> });
message!(/// One logged event.
    BatchEvent { id: String, ev: String, #[serde(default)] note: String });
message!(/// `mpm.dbatg.show`.
    ShowRequest { store: StoreRef, #[serde(default)] brief: bool });
message!(/// `mpm.dbatg.digest`.
    DigestRequest { store: StoreRef, #[serde(default)] max_tokens: Option<u32> });
message!(/// `mpm.dbatg.show`, `digest` and `archive_show` reply.
    TextResponse { text: String });
message!(/// `mpm.dbatg.fresh`: `since` is ISO or epoch seconds; `max_age` minutes.
    FreshRequest { store: StoreRef, #[serde(default)] since: Option<String>,
        #[serde(default)] max_age: Option<u32> });
message!(/// `mpm.dbatg.fresh` reply.
    FreshResponse { fresh: bool, written_at: Option<String> });
message!(/// `mpm.dbatg.archive_show`.
    ArchiveShowRequest { store: StoreRef, id: String });
message!(/// `mpm.dbatg.stamp`: harness-neutral turn facts; keyed on `turn_id`.
    StampRequest { store: StoreRef, turn_id: String, ended_at: String, mutated: bool });
message!(/// `mpm.dbatg.stamp` reply: consecutive stale turns.
    StampResponse { stale_n: u32 });
message!(/// `mpm.dbatg.check_dispatch`.
    CheckDispatchRequest { store: StoreRef, prompt: String });
message!(/// `mpm.dbatg.check_dispatch` reply.
    CheckDispatchResponse { decision: DispatchDecision, msg: String });
message!(/// `mpm.dbatg.rotate`.
    RotateRequest { store: StoreRef });
message!(/// `mpm.dbatg.rotate` reply: log lines moved.
    RotateResponse { moved: u64 });

/// A dispatch check's verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchDecision {
    /// The prompt names an active goal.
    Allow,
    /// It does not; warn mode.
    Warn,
    /// It does not; block mode.
    Deny,
}

/// One batch op, tagged by `op`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Create a record.
    Add(AddOp),
    /// Change a record.
    Update(UpdateOp),
    /// Close with a verified outcome (archives D, A, T).
    Close(CloseOp),
    /// Rewrite fields of the session record.
    Session(Box<SessionOp>),
    /// Archive done records and old answered ones.
    Prune(PruneOp),
}

message!(/// `add`: `kind` is the id letter.
    AddOp { kind: Prefix, title: String,
        #[serde(default)] id: Option<String>, #[serde(default)] goal: Option<String>,
        #[serde(default)] rank: Option<u32>, #[serde(default)] finish: Option<String>,
        #[serde(default)] maps: Option<String>, #[serde(default)] status: Option<String>,
        #[serde(default)] to: Option<String>, #[serde(default)] opts: Option<Vec<OptInput>> });
message!(/// `{l, rec}`; any other key is refused.
    OptInput { l: String, #[serde(default)] rec: bool });
message!(/// `update`.
    UpdateOp { id: String,
        #[serde(default)] title: Option<String>, #[serde(default)] rank: Option<u32>,
        #[serde(default)] esc: Option<String>, #[serde(default)] answer: Option<String>,
        #[serde(default)] to: Option<String>, #[serde(default)] outcome: Option<String>,
        #[serde(default)] status: Option<String>, #[serde(default)] note: Option<String> });
message!(/// `close`.
    CloseOp { id: String, outcome: String });
message!(/// `session`: a list field replaces the stored list.
    SessionOp {
        #[serde(default)] task: Option<String>, #[serde(default)] next: Option<String>,
        #[serde(default)] waiting_on: Option<Vec<WaitingOnInput>>,
        #[serde(default)] owed: Option<Vec<OwedInput>>,
        #[serde(default)] agents: Option<Vec<AgentInput>>,
        #[serde(default)] crons: Option<Vec<CronInput>>,
        #[serde(default)] refs: Option<Vec<RefInput>>,
        #[serde(default)] boundary: bool, #[serde(default)] ctx_use: Option<CtxUseInput> });
message!(/// `prune`.
    PruneOp { #[serde(default)] dry_run: bool, #[serde(default)] answered_hours: Option<u32>,
        #[serde(default)] require_outcome: bool });
message!(/// `waiting_on` input row.
    WaitingOnInput { who: String, what: String, #[serde(default)] since: Option<String> });
message!(/// `owed` input row.
    OwedInput { check: String, expect: String, #[serde(default, rename = "ref")] reference: String });
message!(/// `agents` input row.
    AgentInput { id: String, #[serde(default)] goal: String, #[serde(default)] what: String });
message!(/// `crons` input row.
    CronInput { id: String, schedule: String, prompt: String });
message!(/// `refs` input row.
    RefInput { key: String, value: String });
message!(/// `ctx_use` input.
    CtxUseInput { active: u8, stale: u8, ctx: u64, #[serde(default)] keep: String });

impl StoreRef {
    /// The store's files, `project` normalised to its launch directory.
    ///
    /// # Errors
    /// As [`super::store::resolve_project`] and [`StorePaths::new`].
    pub fn paths(&self) -> Result<StorePaths, LedgerError> {
        StorePaths::new(&resolve_project(&self.project)?, &self.session)
    }
}

impl Op {
    /// Every limit an op carries on its own, run before it touches a store:
    /// title, paragraphs, `finish`, options, and the session record's rows,
    /// row caps, `who`, `owed.expect`, refs and `ctx_use` (#9758).
    ///
    /// # Errors
    /// [`LedgerError::Invalid`] naming the field.
    pub fn check(&self) -> Result<(), LedgerError> {
        let para = |name: &str, v: Option<&str>| -> Result<(), LedgerError> {
            v.map_or(Ok(()), |t| {
                check_paragraph(name, t, PARAGRAPH_MAX).map(drop)
            })
        };
        match self {
            Self::Add(a) => {
                check_title(&a.title)?;
                super::ops::add_fields(a).map(drop)
            }
            Self::Update(u) => {
                u.title.as_deref().map(check_title).transpose()?;
                para("answer", u.answer.as_deref())?;
                para("outcome", u.outcome.as_deref())
            }
            Self::Close(c) if c.outcome.trim().is_empty() => {
                Err(LedgerError::Invalid("close needs an outcome".into()))
            }
            Self::Close(c) => para("outcome", Some(&c.outcome)),
            Self::Session(s) => super::ops::session_fields(s, "1970-01-01T00:00Z").map(drop),
            Self::Prune(_) => Ok(()),
        }
    }
}
