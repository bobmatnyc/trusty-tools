//! `ta handoff`: hand one task from an assistant instance to tm or tc
//! (#11650, slice 3c).
//!
//! Why: slice 3 of the Trusty Assistants plan. An instance can pass work to
//! trusty-mpm (`tm`) or trusty-code (`tc`). These are stubs: no approval
//! flow, no result polling, no callbacks. The command returns the handle.
//! What:
//! - [`parse_args`] — `<instance> --to tm|tc [--key <key>] [--] <task…>`;
//!   the target is always explicit, the task text is passed through.
//! - [`handoff`] — the flow over a [`Ledger`] and the two clients:
//!   key check, intent, dispatch, completion, all under the ledger lock.
//! - tm: the first handoff of an instance creates one durable session and
//!   the ledger records its id; later handoffs send into it. A session tm
//!   reports gone is REPLACED by a new one, recorded as
//!   [`TmSessionUse::Replaced`] and printed. A stopped or errored session is
//!   REPORTED as a failure, because it is resumable and replacing it would
//!   orphan it.
//! - tc: `task.run`; the returned `session_id` is the handle.
//! - `ledger` — the jsonl ledger; `clients` — the client boundary.
//!
//! Test: `tests` — `ledger_tests`, `flow_tests`, `client_tests`, `args_tests`.

pub mod clients;
pub mod ledger;

#[cfg(test)]
mod tests;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use clients::{CallError, StdioTm, TcClient, TmClient, TmSessionState, UdsTc};
pub use ledger::{LEDGER_FILE, Ledger, LedgerEntry, LedgerRecord, Outcome, TmSessionUse};

use crate::assistants::AssistantHome;
use crate::assistants::project::read_home_config;

/// Longest accepted idempotency key, in characters.
pub const MAX_KEY_CHARS: usize = 128;

/// Where a handoff goes. There is no default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    /// trusty-mpm, through its durable managed session.
    Tm,
    /// trusty-code, through `task.run`.
    Tc,
}

impl Target {
    /// Parse `tm` or `tc`; anything else is [`HandoffError::UnknownTarget`].
    /// Test: `tests::args_tests::the_target_is_required_and_checked`.
    pub fn parse(raw: &str) -> Result<Self, HandoffError> {
        match raw {
            "tm" => Ok(Self::Tm),
            "tc" => Ok(Self::Tc),
            other => Err(HandoffError::UnknownTarget(other.to_string())),
        }
    }

    /// `tm` or `tc`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tm => "tm",
            Self::Tc => "tc",
        }
    }
}

/// A parsed `ta handoff` command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffArgs {
    /// The instance id, as typed.
    pub instance: String,
    /// The explicit target.
    pub target: Target,
    /// The idempotency key; `None` mints a fresh one.
    pub key: Option<String>,
    /// The task text, never interpreted.
    pub task: String,
}

/// Parse the words after `handoff`.
///
/// What: the first word is the instance. Then `--to <t>`, `--to=<t>`,
/// `--key <k>` and `--key=<k>` in any order, until `--` or the first other
/// word. The remaining words, joined with single spaces, are the task, byte
/// for byte; flags after that point are task text. `--to` is required.
/// Test: `tests::args_tests::the_target_is_required_and_checked`,
/// `tests::args_tests::task_words_after_the_flags_are_kept_verbatim`.
pub fn parse_args(rest: &[String]) -> Result<HandoffArgs, HandoffError> {
    let Some((instance, mut words)) = rest.split_first() else {
        return Err(HandoffError::Usage);
    };
    let (mut target, mut key) = (None, None);
    loop {
        let (flag, value, used) = match words {
            [w, ..] if w == "--" => {
                words = &words[1..];
                break;
            }
            [w, ..] if w.starts_with("--to=") => ("--to", Some(&w[5..]), 1),
            [w, ..] if w.starts_with("--key=") => ("--key", Some(&w[6..]), 1),
            [w, v, ..] if w == "--to" || w == "--key" => (flag_name(w), Some(v.as_str()), 2),
            [w] if w == "--to" || w == "--key" => (flag_name(w), None, 1),
            _ => break,
        };
        let Some(value) = value else {
            return Err(HandoffError::MissingValue(flag));
        };
        if flag == "--to" {
            target = Some(Target::parse(value)?);
        } else {
            key = Some(value.to_string());
        }
        words = &words[used..];
    }
    let target = target.ok_or(HandoffError::NoTarget)?;
    Ok(HandoffArgs {
        instance: instance.clone(),
        target,
        key,
        task: words.join(" "),
    })
}

/// The `'static` spelling of a flag word already known to be one.
fn flag_name(word: &str) -> &'static str {
    if word == "--to" { "--to" } else { "--key" }
}

/// What a successful handoff reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HandoffReport {
    /// The idempotency key; pass it again with `--key` to retry safely.
    pub key: String,
    pub target: Target,
    /// The target's handle: the tm managed session id or the tc session id.
    pub handle: String,
    /// tm only: created, reused or replaced.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tm_session: Option<TmSessionUse>,
    /// True when the key was already dispatched and nothing was sent now.
    pub replayed: bool,
}

/// A handoff that did not produce a handle.
///
/// Why: the Fail-Open Check. Every way a handoff can stop short is a variant;
/// none carries a handle as if it had succeeded.
#[derive(Debug, Error)]
pub enum HandoffError {
    /// The command line had no instance.
    #[error("usage: ta handoff <instance> --to tm|tc [--key <key>] [--] <task…>")]
    Usage,
    /// `--to` was not given. There is no default target.
    #[error("ta handoff needs `--to tm` or `--to tc`; there is no default target")]
    NoTarget,
    /// `--to` named something other than `tm` or `tc`.
    #[error("unknown handoff target `{0}`; expected `tm` or `tc`")]
    UnknownTarget(String),
    /// A flag was the last word.
    #[error("`{0}` needs a value")]
    MissingValue(&'static str),
    /// The task text was empty or whitespace only.
    #[error("the task is empty")]
    EmptyTask,
    /// The key is empty, too long or holds a control character.
    #[error("invalid idempotency key: {0}")]
    InvalidKey(String),
    /// The key was used before for a different target or task.
    #[error("idempotency key `{key}` was already used for a different target or task")]
    KeyConflict { key: String },
    /// The instance's `config.toml` could not be read.
    #[error("cannot read the instance's config: {0}")]
    Config(#[source] crate::assistants::AssistantError),
    /// A ledger line is not a valid record.
    #[error("handoff ledger `{}` line {line} is corrupt: {reason}", path.display())]
    CorruptLedger {
        path: PathBuf,
        line: usize,
        reason: String,
    },
    /// The ledger could not be opened, locked, read or written.
    #[error("handoff ledger `{}` could not be used: {source}", path.display())]
    LedgerIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Nothing was dispatched; a retry with the same key may dispatch.
    #[error("handoff `{key}` to {} failed: {detail}", target.as_str())]
    Failed {
        key: String,
        target: Target,
        detail: String,
    },
    /// The work may or may not have been dispatched. Never success; the key
    /// will not dispatch again.
    #[error(
        "handoff `{key}` to {}: outcome_unknown — {detail}; check the target before retrying with a new key",
        target.as_str()
    )]
    OutcomeUnknown {
        key: String,
        target: Target,
        detail: String,
    },
    /// The target took the work but its completion could not be recorded;
    /// the ledger will read `outcome_unknown` for this key.
    #[error(
        "handoff `{key}` reached {} as `{handle}`, but the ledger could not record it: {source}",
        target.as_str()
    )]
    CompletionNotRecorded {
        key: String,
        target: Target,
        handle: String,
        #[source]
        source: Box<HandoffError>,
    },
}

/// The two clients a handoff may use.
pub struct Clients<'a> {
    pub tm: &'a dyn TmClient,
    pub tc: &'a dyn TcClient,
}

/// Run one handoff for the instance at `home`.
///
/// What, in order, under the ledger lock: refuse an empty task or bad key;
/// if the key is known, refuse a different target or task, return a
/// dispatched key's handle without dispatching (`replayed`), refuse an open
/// or unknown key as [`HandoffError::OutcomeUnknown`], and retry a failed
/// one; append and sync the intent (no dispatch if that fails); dispatch;
/// append the completion. Only a recorded [`Outcome::Dispatched`] returns
/// `Ok`.
/// Test: `tests::flow_tests` — the whole module.
pub async fn handoff(
    home: &AssistantHome,
    args: &HandoffArgs,
    clients: Clients<'_>,
) -> Result<HandoffReport, HandoffError> {
    if args.task.trim().is_empty() {
        return Err(HandoffError::EmptyTask);
    }
    let key = match &args.key {
        Some(key) => validate_key(key)?,
        None => uuid::Uuid::new_v4().to_string(),
    };
    let target = args.target;
    let mut ledger = Ledger::open(&Ledger::path_for(home)).await?;
    if let Some(entry) = ledger.entries().remove(&key) {
        if entry.target != target || entry.task != args.task {
            return Err(HandoffError::KeyConflict { key });
        }
        match entry.outcome {
            Outcome::Dispatched { handle, tm_session } => {
                return Ok(HandoffReport {
                    key,
                    target,
                    handle,
                    tm_session,
                    replayed: true,
                });
            }
            Outcome::OutcomeUnknown { error } => {
                return Err(HandoffError::OutcomeUnknown {
                    key,
                    target,
                    detail: error,
                });
            }
            Outcome::Failed { .. } => {}
        }
    }
    ledger.append(LedgerRecord::Intent {
        v: ledger::LEDGER_RECORD_VERSION,
        key: key.clone(),
        at: ledger::now(),
        target,
        task: args.task.clone(),
    })?;
    let result = match target {
        Target::Tm => dispatch_tm(home, &ledger, clients.tm, &args.task).await,
        Target::Tc => {
            let project = project_dir(home);
            match project {
                Ok(project) => clients
                    .tc
                    .task_run(&args.task, project.as_deref())
                    .await
                    .map(|handle| (handle, None)),
                Err(e) => Err(CallError::NotSent(e.to_string())),
            }
        }
    };
    let outcome = match result {
        Ok((handle, tm_session)) => Outcome::Dispatched { handle, tm_session },
        Err(CallError::NotSent(error)) => Outcome::Failed { error },
        Err(CallError::Unknown(error)) => Outcome::OutcomeUnknown { error },
    };
    let recorded = ledger.append(LedgerRecord::Done {
        v: ledger::LEDGER_RECORD_VERSION,
        key: key.clone(),
        at: ledger::now(),
        result: outcome.clone(),
    });
    match (outcome, recorded) {
        (Outcome::Dispatched { handle, tm_session }, Ok(())) => Ok(HandoffReport {
            key,
            target,
            handle,
            tm_session,
            replayed: false,
        }),
        (Outcome::Dispatched { handle, .. }, Err(e)) => Err(HandoffError::CompletionNotRecorded {
            key,
            target,
            handle,
            source: Box::new(e),
        }),
        (Outcome::Failed { error }, _) => Err(HandoffError::Failed {
            key,
            target,
            detail: error,
        }),
        (Outcome::OutcomeUnknown { error }, _) => Err(HandoffError::OutcomeUnknown {
            key,
            target,
            detail: error,
        }),
    }
}

/// The tm leg: create, reuse or replace the instance's durable session.
async fn dispatch_tm(
    home: &AssistantHome,
    ledger: &Ledger,
    tm: &dyn TmClient,
    task: &str,
) -> Result<(String, Option<TmSessionUse>), CallError> {
    let repo = || -> Result<PathBuf, CallError> {
        project_dir(home)
            .map(|p| p.unwrap_or_else(|| home.path().to_path_buf()))
            .map_err(|e| CallError::NotSent(e.to_string()))
    };
    let Some(current) = ledger.tm_session() else {
        let id = tm.session_new(&repo()?, task).await?;
        return Ok((id, Some(TmSessionUse::Created)));
    };
    match tm.session_state(&current).await? {
        TmSessionState::Live => {
            tm.session_send(&current, task).await?;
            Ok((current, Some(TmSessionUse::Reused)))
        }
        TmSessionState::Gone { reason } => {
            let id = tm.session_new(&repo()?, task).await?;
            Ok((
                id,
                Some(TmSessionUse::Replaced {
                    previous: current,
                    reason,
                }),
            ))
        }
        TmSessionState::Parked { state } => Err(CallError::NotSent(format!(
            "the instance's tm session `{current}` is {state}; resume it with \
             `tm session resume {current}`, then retry"
        ))),
    }
}

/// The instance's configured project directory, if any.
fn project_dir(home: &AssistantHome) -> Result<Option<PathBuf>, HandoffError> {
    Ok(read_home_config(home)
        .map_err(HandoffError::Config)?
        .and_then(|c| c.project_dir))
}

/// A key must be 1..=[`MAX_KEY_CHARS`] characters with no control character.
/// Test: `tests::flow_tests::keys_are_validated`.
fn validate_key(key: &str) -> Result<String, HandoffError> {
    let chars = key.chars().count();
    if chars == 0 || chars > MAX_KEY_CHARS || key.chars().any(char::is_control) {
        return Err(HandoffError::InvalidKey(format!(
            "it must be 1 to {MAX_KEY_CHARS} characters with no control character"
        )));
    }
    Ok(key.to_string())
}
