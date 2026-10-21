//! The `session` op: field limits and the merge into `state.json` (#9758).
//!
//! Why: the session record carries the most per-field limits of any op, and
//! the writer must apply them exactly as the prototype's `apply_session`.
//! What: [`session_fields`] checks and trims every field once, for
//! [`Op::check`](crate::ledger::rpc::Op) and for [`apply_session`];
//! [`apply_session`] merges them into the stored session and refuses a
//! record over [`SESSION_MAX`].
//! Test: `ledger_writer_session_limits_refuse_and_leave_files`.

use super::{Now, bad, event};
use crate::error::LedgerError;
use crate::ledger::limits::{SESSION_MAX, check_line};
use crate::ledger::model::{Agent, Cron, CtxUse, Owed, SessionRef, State, WaitingOn};
use crate::ledger::rpc::{
    AgentInput, BatchEvent, CronInput, OwedInput, RefInput, SessionOp, WaitingOnInput,
};

const WHO: [&str; 5] = ["owner", "architect", "ci", "agent", "other"];
const VAGUE: [&str; 11] = [
    "latest", "main", "master", "head", "current", "tbd", "unknown", "?", "n/a", "none", "-",
];
const REFS_MAX: usize = 12;

/// The session op's fields, checked and trimmed; `now` fills `since` and
/// `ctx_use.at` (the prototype's `apply_session` limits).
pub(crate) fn session_fields(s: &SessionOp, now: &str) -> Result<SessionFields, LedgerError> {
    let caps = |n: usize, max: usize, name: &str| {
        (n <= max)
            .then_some(())
            .ok_or_else(|| bad(format!("{name}: a list of at most {max} objects")))
    };
    let line = |v: &Option<String>, name: &str, max: usize| {
        v.as_deref()
            .map(|t| check_line(name, t, max, true))
            .transpose()
    };
    let mut f = SessionFields {
        task: line(&s.task, "task", 160)?,
        next: line(&s.next, "next", 200)?,
        ..SessionFields::default()
    };
    if let Some(rows) = &s.waiting_on {
        caps(rows.len(), 5, "waiting_on")?;
        let row = |r: &WaitingOnInput| -> Result<WaitingOn, LedgerError> {
            if !WHO.contains(&r.who.as_str()) {
                return Err(bad(format!("waiting_on.who must be one of {WHO:?}")));
            }
            let what = check_line("waiting_on.what", &r.what, 120, true)?;
            let since = check_line(
                "waiting_on.since",
                r.since.as_deref().unwrap_or(now),
                20,
                false,
            )?;
            Ok(WaitingOn {
                who: r.who.clone(),
                what,
                since,
            })
        };
        f.waiting_on = Some(rows.iter().map(row).collect::<Result<_, _>>()?);
    }
    if let Some(rows) = &s.owed {
        caps(rows.len(), 8, "owed")?;
        let row = |r: &OwedInput| -> Result<Owed, LedgerError> {
            let expect = check_line("owed.expect", &r.expect, 80, true)?;
            if VAGUE.contains(&expect.to_lowercase().as_str()) {
                return Err(bad(format!(
                    "owed.expect must be an exact value (SHA, run id, count), not {expect:?}"
                )));
            }
            let check = check_line("owed.check", &r.check, 80, true)?;
            Ok(Owed {
                check,
                expect,
                reference: check_line("owed.ref", &r.reference, 80, false)?,
            })
        };
        f.owed = Some(rows.iter().map(row).collect::<Result<_, _>>()?);
    }
    if let Some(rows) = &s.agents {
        caps(rows.len(), 6, "agents")?;
        let row = |r: &AgentInput| -> Result<Agent, LedgerError> {
            Ok(Agent {
                id: check_line("agents.id", &r.id, 40, true)?,
                goal: check_line("agents.goal", &r.goal, 40, false)?,
                what: check_line("agents.what", &r.what, 80, false)?,
            })
        };
        f.agents = Some(rows.iter().map(row).collect::<Result<_, _>>()?);
    }
    if let Some(rows) = &s.crons {
        caps(rows.len(), 4, "crons")?;
        let row = |r: &CronInput| -> Result<Cron, LedgerError> {
            Ok(Cron {
                id: check_line("crons.id", &r.id, 40, true)?,
                schedule: check_line("crons.schedule", &r.schedule, 40, true)?,
                prompt: check_line("crons.prompt", &r.prompt, 200, true)?,
            })
        };
        f.crons = Some(rows.iter().map(row).collect::<Result<_, _>>()?);
    }
    if let Some(rows) = &s.refs {
        caps(rows.len(), REFS_MAX, "refs")?;
        f.refs = Some(rows.iter().map(ref_row).collect::<Result<_, _>>()?);
    }
    if let Some(c) = &s.ctx_use {
        if c.active > 100 || c.stale > 100 || c.ctx > 1_000_000_000 {
            return Err(bad(
                "ctx_use: active and stale must be 0-100, ctx 0-1000000000",
            ));
        }
        let keep = check_line("ctx_use.keep", &c.keep, 160, false)?;
        f.ctx_use = Some(CtxUse {
            at: now.to_owned(),
            active: c.active,
            stale: c.stale,
            ctx: c.ctx,
            keep,
        });
    }
    Ok(f)
}

fn ref_row(r: &RefInput) -> Result<SessionRef, LedgerError> {
    let mut chars = r.key.chars();
    let lead = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let rest = chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_.-".contains(c));
    if !(lead && rest && r.key.len() <= 32) {
        return Err(bad("refs.key must match ^[a-z0-9][a-z0-9_.-]{0,31}$"));
    }
    let value = check_line("refs.value", &r.value, 200, true)?;
    Ok(SessionRef {
        key: r.key.clone(),
        value,
    })
}

/// [`session_fields`]' result.
#[derive(Debug, Default)]
pub(crate) struct SessionFields {
    task: Option<String>,
    next: Option<String>,
    waiting_on: Option<Vec<WaitingOn>>,
    owed: Option<Vec<Owed>>,
    agents: Option<Vec<Agent>>,
    crons: Option<Vec<Cron>>,
    refs: Option<Vec<SessionRef>>,
    ctx_use: Option<CtxUse>,
}

pub(super) fn apply_session(
    st: &mut State,
    s: &SessionOp,
    now: &Now,
) -> Result<BatchEvent, LedgerError> {
    let f = session_fields(s, &now.minute)?;
    let mut ses = st.session.clone().unwrap_or_default();
    ses.task = f.task.or(ses.task);
    ses.next = f.next.or(ses.next);
    ses.waiting_on = f.waiting_on.or(ses.waiting_on);
    if let Some(owed) = f.owed {
        ses.owed_none = Some(owed.is_empty());
        ses.owed = Some(owed);
    }
    ses.agents = f.agents.or(ses.agents);
    ses.crons = f.crons.or(ses.crons);
    if let Some(new) = f.refs {
        let mut refs = if new.is_empty() {
            Vec::new()
        } else {
            ses.refs.take().unwrap_or_default()
        };
        for r in new {
            match refs.iter_mut().find(|o| o.key == r.key) {
                Some(old) => old.value = r.value,
                None => refs.push(r),
            }
        }
        if refs.len() > REFS_MAX {
            return Err(bad(format!("refs: at most {REFS_MAX} refs")));
        }
        ses.refs = Some(refs);
    }
    if s.boundary {
        ses.boundary_at = Some(now.minute.clone());
    }
    ses.ctx_use = f.ctx_use.or(ses.ctx_use);
    let mut probe = ses.clone();
    probe.written_at = Some(now.minute.clone());
    probe.written_epoch = Some(now.epoch);
    let n = serde_json::to_string(&probe)?.len();
    if n > SESSION_MAX {
        return Err(bad(format!(
            "session record would be {n} B (> {SESSION_MAX} B limit)"
        )));
    }
    let note: String = ses
        .task
        .as_deref()
        .unwrap_or("")
        .chars()
        .take(120)
        .collect();
    st.session = Some(ses);
    Ok(event(
        &format!("{}-session", st.code.as_str()),
        "update",
        &note,
    ))
}
