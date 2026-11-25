//! The `trusty-architect-il` command line: `datg <verb> ...` and `ilio <verb> ...`.
//!
//! Why: hooks, skills and the poller call a per-turn command, so process start is
//! the cost that matters. The verbs, flags, stdout, stderr and exit codes are
//! those of `datg.py` and `ilio.py` (exit 2 for a refused write, files untouched),
//! so a caller switches by changing the program name.
//! What: [`run`] takes argv, a stdin reader, an [`Env`] and a clock and returns an
//! [`Out`] with the streams and exit code; the binary only forwards them. This
//! file holds the dispatch, global flags and the write verbs; `cli_read.rs` the
//! read verbs; `ilio/` the ilio verbs. `hook`, `check-dispatch` and `stamp` are
//! not ported (they stay in Python until the Architect approves a cutover).
//! Test: `golden_steps_match_python_byte_for_byte`, `session_flag_and_state_dir_pick_the_directory`,
//! `binary_round_trip`, `check_matches_python_on_more_blocks`.

use super::args::{Parsed, Spec, parse};
use super::error::{IlError, IlResult, invalid};
use super::json::{Json, obj};
use super::ledger::Ledger;
use super::ops::{Ctx, Ev};
use super::store::{Where, per_pm_session, resolve};
use crate::Clock;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// Environment variables and the working directory a run reads.
#[derive(Debug, Clone, Default)]
pub struct Env {
    /// The working directory (`--project` default).
    pub cwd: PathBuf,
    vars: HashMap<String, String>,
}

impl Env {
    /// The real process environment.
    pub fn from_process() -> Self {
        Self {
            cwd: std::env::current_dir().unwrap_or_default(),
            vars: std::env::vars().collect(),
        }
    }

    /// A test environment with `cwd` and no variables.
    pub fn bare(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            vars: HashMap::new(),
        }
    }

    /// Sets a variable (builder form, for tests).
    #[must_use]
    pub fn with(mut self, k: &str, v: &str) -> Self {
        self.vars.insert(k.to_owned(), v.to_owned());
        self
    }

    /// A variable, `None` when unset or empty.
    pub fn get(&self, k: &str) -> Option<&str> {
        self.vars
            .get(k)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }
}

/// What a run printed and how it ended.
#[derive(Debug, Default, Clone)]
pub struct Out {
    /// Standard output.
    pub stdout: String,
    /// Standard error.
    pub stderr: String,
    /// Process exit code.
    pub code: i32,
}

impl Out {
    pub(super) fn println(&mut self, s: &str) {
        self.stdout.push_str(s);
        self.stdout.push('\n');
    }

    pub(super) fn eprintln(&mut self, s: &str) {
        self.stderr.push_str(s);
        self.stderr.push('\n');
    }
}

/// One run: its inputs and its output.
pub struct Cx<'a> {
    /// Environment.
    pub env: &'a Env,
    /// Wall time.
    pub clock: &'a dyn Clock,
    /// Reads all of stdin (called at most once).
    pub stdin: &'a dyn Fn() -> String,
    /// What the run printed.
    pub out: Out,
}

impl<'a> Cx<'a> {
    pub(super) fn ctx(&self) -> Ctx<'a> {
        Ctx {
            clock: self.clock,
            env_session: self.env.get("DATG_SESSION").unwrap_or("").to_owned(),
            warnings: Vec::new(),
        }
    }

    pub(super) fn ledger(&self, w: &Where) -> IlResult<Ledger> {
        let mut l = Ledger::new(resolve(w)?);
        if let Some(ms) = self
            .env
            .get("TRUSTY_IL_LOCK_WAIT_MS")
            .and_then(|v| v.parse().ok())
        {
            l.wait = Duration::from_millis(ms);
        }
        Ok(l)
    }

    /// `WARNINGS`: `datg: <msg>` on stderr after a successful write.
    pub(super) fn flush_warnings(&mut self, ctx: &Ctx, prefix: &str) {
        for w in &ctx.warnings {
            self.out.eprintln(&format!("{prefix}: {w}"));
        }
    }
}

/// Runs `args` (the first being `datg` or `ilio`).
pub fn run(args: &[String], stdin: &dyn Fn() -> String, env: &Env, clock: &dyn Clock) -> Out {
    let mut cx = Cx {
        env,
        clock,
        stdin,
        out: Out::default(),
    };
    let (prog, rest) = match args.split_first() {
        Some((p, r)) if p == "datg" || p == "ilio" => (p.as_str(), r),
        _ => {
            cx.out
                .eprintln("usage: trusty-architect-il <datg|ilio> <verb> [args]");
            cx.out.code = 2;
            return cx.out;
        }
    };
    let result = if prog == "datg" {
        run_datg(&mut cx, rest)
    } else {
        super::ilio::cli::run_ilio(&mut cx, rest)
    };
    match result {
        Ok(code) => cx.out.code = code,
        Err(e) => {
            cx.out.eprintln(&format!("{prog}: {e}"));
            cx.out.code = 2;
        }
    }
    cx.out
}

const ADD: Spec = Spec {
    values: &[
        "title",
        "goal",
        "rank",
        "finish",
        "maps",
        "opts",
        "to",
        "done-when",
        "id",
    ],
    flags: &["multistep"],
    repeat: &[],
};
const UPDATE: Spec = Spec {
    values: &[
        "title",
        "status",
        "answer",
        "outcome",
        "esc",
        "to",
        "note",
        "done-when",
        "rank",
        "maps",
        "priority",
    ],
    flags: &[],
    repeat: &[],
};

fn text_or_null(p: &Parsed, k: &str) -> Json {
    p.val(k).map_or(Json::Null, Json::from)
}

fn int_or_null(p: &Parsed, k: &str) -> IlResult<Json> {
    Ok(p.int(k)
        .map_err(IlError::Invalid)?
        .map_or(Json::Null, Json::Int))
}

/// `_opts`: `"a*,b"` to `[{l, rec}]`.
fn opts(s: Option<&str>) -> Json {
    let list = s
        .unwrap_or("")
        .split(',')
        .map(|p| crate::pystr::strip(p).to_owned())
        .filter(|p| !p.is_empty())
        .map(|p| {
            obj([
                ("l", p.trim_end_matches('*').into()),
                ("rec", p.ends_with('*').into()),
            ])
        })
        .collect();
    Json::Arr(list)
}

fn ids(evs: &[Ev]) -> String {
    evs.iter()
        .map(|e| e.id.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Runs the ops, prints the ids, then the notes. Returns the events and the committed state.
pub(super) fn write_ops_print(cx: &mut Cx, w: &Where, ops: &[Json]) -> IlResult<(Vec<Ev>, Json)> {
    let ledger = cx.ledger(w)?;
    let mut ctx = cx.ctx();
    let (evs, st) = ledger.write_ops(&mut ctx, ops)?;
    cx.out.println(&ids(&evs));
    cx.flush_warnings(&ctx, "datg");
    Ok((evs, st))
}

fn run_datg(cx: &mut Cx, args: &[String]) -> IlResult<i32> {
    let mut w = Where {
        project: cx.env.cwd.clone(),
        env_session: cx.env.get("DATG_SESSION").map(str::to_owned),
        tmux_pane: cx.env.get("TMUX_PANE").map(str::to_owned),
        state_dir: cx.env.get("DATG_STATE_DIR").map(PathBuf::from),
        ..Where::default()
    };
    let mut i = 0;
    while i < args.len() && args[i].starts_with("--") {
        let (flag, inline) = match args[i].split_once('=') {
            Some((f, v)) => (f, Some(v.to_owned())),
            None => (args[i].as_str(), None),
        };
        i += 1;
        let value = match inline {
            Some(v) => v,
            None => {
                let v = args.get(i).cloned();
                i += 1;
                v.ok_or_else(|| {
                    IlError::Invalid(format!("argument {flag}: expected one argument"))
                })?
            }
        };
        match flag {
            "--project" => w.project = PathBuf::from(value),
            "--session" => w.session = Some(value),
            "--state-dir" => w.state_dir = Some(PathBuf::from(value)),
            other => return invalid(format!("unrecognized arguments: {other}")),
        }
    }
    let Some(verb) = args.get(i) else {
        return invalid("a verb is required (init, add, update, close, act, digest, fresh, ...)");
    };
    let rest = &args[i + 1..];
    super::cli_verbs::dispatch(cx, &w, verb, rest)
}

pub(super) fn parse_args(rest: &[String], spec: &Spec) -> IlResult<Parsed> {
    parse(rest, spec).map_err(IlError::Invalid)
}

pub(super) fn add_op(rest: &[String]) -> IlResult<Json> {
    let p = parse_args(rest, &ADD)?;
    let kind = match p.pos.first().map(String::as_str) {
        Some(k @ ("G" | "D" | "A" | "T")) => k.to_owned(),
        _ => return invalid("argument kind: invalid choice (choose from 'G', 'D', 'A', 'T')"),
    };
    Ok(obj([
        ("op", "add".into()),
        ("kind", kind.into()),
        ("title", text_or_null(&p, "title")),
        ("goal", text_or_null(&p, "goal")),
        ("rank", int_or_null(&p, "rank")?),
        ("finish", text_or_null(&p, "finish")),
        ("maps", text_or_null(&p, "maps")),
        ("opts", opts(p.val("opts"))),
        ("to", text_or_null(&p, "to")),
        ("id", text_or_null(&p, "id")),
        ("multistep", p.flag("multistep").into()),
        ("done_when", text_or_null(&p, "done-when")),
    ]))
}

pub(super) fn update_op(rest: &[String]) -> IlResult<(Json, String)> {
    let p = parse_args(rest, &UPDATE)?;
    let Some(id) = p.pos.first() else {
        return invalid("the following arguments are required: id");
    };
    let mut op = obj([("op", "update".into()), ("id", id.as_str().into())]);
    for k in ["title", "status", "answer", "outcome", "esc", "to", "note"] {
        op.set(k, text_or_null(&p, k));
    }
    op.set("rank", int_or_null(&p, "rank")?);
    op.set("priority", int_or_null(&p, "priority")?);
    op.set("done_when", text_or_null(&p, "done-when"));
    op.set("maps", text_or_null(&p, "maps"));
    Ok((op, id.clone()))
}

pub(super) fn session_name(cx: &Cx, w: &Where, code: &str) -> IlResult<String> {
    Ok(per_pm_session(w)?
        .or_else(|| cx.env.get("DATG_SESSION").map(str::to_owned))
        .unwrap_or_else(|| code.to_owned()))
}
