//! `dashboard-db.py`'s command line: argparse, dispatch, output and exit codes (#12029 P2g).
//!
//! Why: the drop-in for `python3 scripts/dashboard-db.py ...` must parse the
//! same command lines, print the same JSON (`json.dump(indent=2,
//! ensure_ascii=False)`) and lines, and exit the same way.
//! What: [`run`]: the `TAIL_HOST` read (an unusable `local/architect.json`
//! is one stderr line), argparse (top `--root` / `--actor`, 18
//! sub-commands, `--if-version` as `int`), the store, the sub-command, then
//! the output. A `StoreError` prints `dashboard-db: <code>: <msg>` and exits
//! `{404: 1, 409: 3}.get(status, 2)`; `fsck` and `validate` exit 1 on a
//! problem; any other exception prints its last traceback line, exit 1.
//! Test: `dashboard_db_recordings_replay_clean_against_the_port`.

use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::help::{SUB_TEXT, TOP_HELP, TOP_USAGE};
use super::{Ctx, Fail, R, s, summary, write};
use crate::argparse::{Args, HELP, Nargs, Opt, Pos};
use crate::argparse_sub::{self, Cli, Cmd, Sub};
use crate::clock::Clock;
use crate::dashboard::pyqual;
use crate::dashboard::store::py::{Options as StoreOptions, PyStore};
use crate::pyjson_load::{PyInt, PyValue};
use crate::pyval::{dumps, get};
use crate::spec_drift::Outcome;

/// The questions kick: one in-process `questions::rc` pass. It runs on a
/// worker thread, so it owns what it reads.
pub type Kick = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

/// What the caller supplies besides argv and stdin.
pub struct Options<'a> {
    /// The script's checkout (`REPO`): `local/owner-queue`, `schemas/dashboard`,
    /// `local/architect.json`, `inbox/questions-rc.json` and `local/il` under it.
    pub checkout: PathBuf,
    /// The working directory relative paths resolve against.
    pub cwd: PathBuf,
    /// The process environment.
    pub env: &'a dyn Fn(&str) -> Option<String>,
    /// `datetime.now(timezone.utc)`.
    pub clock: Arc<dyn Clock + Send + Sync>,
    /// The `datg` program the footer's reminders read runs (`datg`: found on `PATH`).
    pub datg: PathBuf,
    /// The questions kick: one in-process `questions::rc` pass.
    pub kick: Kick,
    /// How long `update` waits for the kick ([`crate::TMUX_TIMEOUT`] in the
    /// binary and the replay).
    pub kick_timeout: Duration,
}

impl Options<'_> {
    /// `ds.DEFAULT_ROOT`: `<checkout>/local/owner-queue`.
    pub fn default_root(&self) -> PathBuf {
        self.checkout.join("local").join("owner-queue")
    }

    /// `dashboard_schema.SCHEMA_DIR`: `<checkout>/schemas/dashboard`.
    pub fn schema_dir(&self) -> PathBuf {
        self.checkout.join("schemas").join("dashboard")
    }
}

const fn opt(long: &'static str, value: bool) -> Opt {
    Opt {
        long,
        short: None,
        value,
    }
}

const fn one(name: &'static str) -> Pos {
    Pos {
        name,
        nargs: Nargs::One,
        choices: &[],
        required: true,
    }
}

const NAMES: [&str; 17] = [
    "get",
    "list",
    "set",
    "update",
    "delete",
    "pruned",
    "next",
    "create",
    "log",
    "status-brief",
    "summary",
    "requests",
    "validate",
    "fsck",
    "tally",
    "footer",
    "batch",
];
const CI: Pos = one("collection");
const ID: Pos = one("id");
const JSON: Opt = opt("--json", true);
const IF_VERSION: Opt = opt("--if-version", true);
const AS_JSON: Opt = opt("--json", false);

/// Each sub-command's options and positionals, in `NAMES` order.
const SHAPES: [(&[Opt], &[Pos]); 17] = [
    (&[HELP], &[CI, ID]),
    (&[HELP, opt("--status", true)], &[CI]),
    (
        &[HELP, JSON, IF_VERSION, opt("--allow-id", false)],
        &[CI, ID],
    ),
    (&[HELP, JSON, IF_VERSION], &[CI, ID]),
    (&[HELP, IF_VERSION], &[CI, ID]),
    (&[HELP, opt("--id", true), opt("--collection", true)], &[]),
    (&[HELP], &[one("prefix")]),
    (&[HELP, JSON], &[CI, one("prefix")]),
    (&[HELP], &[CI, ID, one("text")]),
    (&[HELP], &[ID, one("brief_id"), one("brief")]),
    (&[HELP], &[]),
    (&[HELP, AS_JSON], &[]),
    (&[HELP], &[]),
    (&[HELP], &[]),
    (&[HELP, AS_JSON], &[]),
    (&[HELP, AS_JSON], &[]),
    (&[HELP], &[one("file")]),
];

const fn sub(i: usize) -> Sub {
    Sub {
        name: NAMES[i],
        opts: SHAPES[i].0,
        pos: SHAPES[i].1,
        usage: SUB_TEXT[i].0,
        help: SUB_TEXT[i].1,
    }
}

const SUBS: [Sub; 17] = [
    sub(0),
    sub(1),
    sub(2),
    sub(3),
    sub(4),
    sub(5),
    sub(6),
    sub(7),
    sub(8),
    sub(9),
    sub(10),
    sub(11),
    sub(12),
    sub(13),
    sub(14),
    sub(15),
    sub(16),
];

const CLI: Cli = Cli {
    prog: "dashboard-db.py",
    usage: TOP_USAGE,
    help: TOP_HELP,
    top: &[HELP, opt("--root", true), opt("--actor", true)],
    dest: "cmd",
    names: &NAMES,
    required: true,
    subs: &SUBS,
};

/// The `required=True` options of each sub-command (`--json` of set, update, create).
fn required(cmd: usize) -> &'static [usize] {
    match NAMES[cmd] {
        "set" | "update" | "create" => &[1],
        _ => &[],
    }
}

/// `int(text)`: whitespace, a sign, digits with single `_` between them (any Unicode digit).
fn py_int(text: &str) -> Option<PyInt> {
    let t = crate::pystr::strip(text);
    let (neg, body) = match t.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    if body.is_empty() || body.starts_with('_') || body.ends_with('_') || body.contains("__") {
        return None;
    }
    let mut digits = String::new();
    for c in body.chars().filter(|c| *c != '_') {
        digits.push(char::from_digit(crate::pystr::digit_value(c)?, 10)?);
    }
    let digits = digits.trim_start_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let text = if neg && digits != "0" {
        format!("-{digits}")
    } else {
        digits.to_owned()
    };
    Some(text.parse::<i64>().map_or(PyInt::Big(text), PyInt::Small))
}

/// argparse's `type=int` for `--if-version`.
fn check(cmd: usize, opt: usize, value: &str) -> Result<(), String> {
    let is_version = SHAPES[cmd]
        .0
        .get(opt)
        .is_some_and(|o| o.long == "--if-version");
    if is_version && py_int(value).is_none() {
        return Err(format!(
            "argument --if-version: invalid int value: {}",
            crate::pystr::py_repr(value)
        ));
    }
    Ok(())
}

/// The value of option `long` of the sub-command's parse.
fn val<'a>(cmd: usize, a: &'a Args, long: &str) -> Option<&'a str> {
    let i = SHAPES[cmd]
        .0
        .iter()
        .position(|o| o.long == long && o.value)?;
    a.values[i].as_deref()
}

/// Whether flag `long` was given.
fn flag(cmd: usize, a: &Args, long: &str) -> bool {
    SHAPES[cmd]
        .0
        .iter()
        .position(|o| o.long == long && !o.value)
        .is_some_and(|i| a.flags[i])
}

/// Positional `i` of the sub-command's parse.
fn arg(a: &Args, i: usize) -> String {
    a.pos
        .get(i)
        .cloned()
        .flatten()
        .and_then(|v| v.into_iter().next())
        .unwrap_or_default()
}

/// `TAIL_HOST`: `tailnet_url` of the instance config; an unusable config is
/// one stderr line and `$DASHBOARD_TAILNET_URL` alone (#10161).
fn tail_host(o: &Options, err: &mut String) -> String {
    match crate::architect_config::load(&o.checkout, o.env) {
        Ok(c) => c.tailnet_url,
        Err(e) => {
            err.push_str(&format!(
                "dashboard-db: tailnet link from DASHBOARD_TAILNET_URL only: {e}\n"
            ));
            (o.env)("DASHBOARD_TAILNET_URL").unwrap_or_default()
        }
    }
}

/// What a sub-command produced, before printing.
enum Out {
    Json(PyValue),
    Lines(Vec<String>),
    Text(String),
    /// `fsck` / `validate`: printed, then exit 1 on a problem.
    Report(PyValue),
}

fn emit(v: &PyValue) -> String {
    dumps(v, Some(2), false) + "\n"
}

fn lines(v: Vec<String>) -> String {
    v.into_iter().map(|l| l + "\n").collect()
}

/// The sub-command's body (`run(args)` of the script).
fn dispatch(ctx: &Ctx, cmd: usize, a: &Args, stdin: &mut dyn Read) -> R<Out> {
    let name = NAMES[cmd];
    let ver = |a: &Args| {
        val(cmd, a, "--if-version")
            .and_then(py_int)
            .map(PyValue::Int)
    };
    let root = ctx.default_root();
    Ok(match name {
        "fsck" => Out::Report(summary::fsck(ctx)?),
        "validate" => Out::Report(summary::validate(ctx)?),
        "pruned" => Out::Lines(summary::pruned_lines(
            ctx,
            val(cmd, a, "--id"),
            val(cmd, a, "--collection"),
        )?),
        "next" => Out::Json(s(&write::next_id(ctx, &arg(a, 0))?)),
        "create" => Out::Json(write::create(
            ctx,
            &arg(a, 0),
            &arg(a, 1),
            val(cmd, a, "--json").unwrap_or(""),
            stdin,
        )?),
        "get" => {
            let (coll, asked) = (arg(a, 0), arg(a, 1));
            let local = pyqual::resolve(&ctx.store, &coll, &asked, &root)?;
            let d = ctx.get(&coll, &s(&local))?;
            let doc = super::dict(vec![
                ("id", s(&d.id)),
                ("version", PyValue::Int(d.version)),
                ("data", d.data),
            ]);
            let doc = pyqual::with_qualifier(&ctx.store, &coll, doc, &root)?;
            let bare = pyqual::split(&asked).1;
            Out::Json(if bare != local {
                super::set(&doc, "alias_of", s(&bare))
            } else {
                doc
            })
        }
        "list" => {
            let coll = arg(a, 0);
            let status = val(cmd, a, "--status").map(s);
            let docs = ctx
                .store
                .list_docs(&s(&coll), status.as_ref(), None, false, None)?;
            let docs = docs
                .into_iter()
                .map(|d| {
                    super::dict(vec![
                        ("id", s(&d.id)),
                        ("version", PyValue::Int(d.version)),
                        ("data", d.data),
                    ])
                })
                .collect();
            Out::Json(pyqual::with_qualifier(
                &ctx.store,
                &coll,
                PyValue::List(docs),
                &root,
            )?)
        }
        "set" | "update" => Out::Json(write::set_or_update(
            ctx,
            name,
            &arg(a, 0),
            &arg(a, 1),
            val(cmd, a, "--json").unwrap_or(""),
            ver(a).as_ref(),
            flag(cmd, a, "--allow-id"),
            stdin,
        )?),
        "log" => Out::Json(write::append_log(ctx, &arg(a, 0), &arg(a, 1), &arg(a, 2))?),
        "status-brief" => Out::Json(write::append_status_brief(
            ctx,
            &arg(a, 0),
            &arg(a, 1),
            &arg(a, 2),
        )?),
        "delete" => Out::Json(write::delete(ctx, &arg(a, 0), &arg(a, 1), ver(a).as_ref())?),
        "tally" => {
            let t = summary::tally(ctx)?;
            if flag(cmd, a, "--json") {
                Out::Json(t)
            } else {
                Out::Text(summary::tally_line(&t) + "\n")
            }
        }
        "footer" => {
            let f = summary::footer(ctx)?;
            if flag(cmd, a, "--json") {
                Out::Json(f)
            } else {
                let text = |k: &str| get(&f, k).map_or(String::new(), crate::pyval::py_str);
                let rem = text("reminders");
                let tail = if rem.is_empty() {
                    String::new()
                } else {
                    format!("{rem}\n")
                };
                Out::Text(format!("{}\n{tail}", text("footer")))
            }
        }
        "summary" => Out::Lines(summary::summary_lines(ctx)?),
        "requests" => {
            let q = summary::requests(ctx)?;
            if flag(cmd, a, "--json") {
                Out::Json(PyValue::List(q))
            } else {
                Out::Text(q.iter().map(summary::request_line).collect())
            }
        }
        _ => Out::Json(write::batch(ctx, &arg(a, 0), stdin)?),
    })
}

/// The exit and stderr line of a failure.
fn failure(f: Fail) -> (String, i32) {
    match f {
        Fail::Store(e) => match e.http() {
            Some((status, code)) => {
                let exit = match status {
                    404 => 1,
                    409 => 3,
                    _ => 2,
                };
                (format!("dashboard-db: {code}: {e}\n"), exit)
            }
            None => (format!("{}: {e}\n", e.class()), 1),
        },
        Fail::Raise(line) => (format!("{line}\n"), 1),
    }
}

fn out(stdout: String, stderr: String, code: i32) -> Outcome {
    Outcome {
        stdout,
        stderr,
        code,
    }
}

/// Why: the drop-in for the script's command line.
/// What: see the module doc; `stdin` is read only for `--json -` and `batch -`.
/// Test: `dashboard_db_recordings_replay_clean_against_the_port`.
pub fn run(args: &[String], stdin: &mut dyn Read, o: &Options) -> Outcome {
    let mut err = String::new();
    let tail = tail_host(o, &mut err);
    let (parsed, top) = argparse_sub::parse_top_required(&CLI, args, &check, &required);
    let (cmd, a) = match parsed {
        Cmd::Run(i, a) => (i, a),
        Cmd::Help(text) => return out(text.into(), err, 0),
        Cmd::Error(e) => return out(String::new(), err + &e, 2),
        Cmd::None => return out(String::new(), err, 0),
    };
    let root = top.values[1]
        .clone()
        .or_else(|| (o.env)("DASHBOARD_ROOT"))
        .unwrap_or_else(|| o.default_root().to_string_lossy().into_owned());
    let actor = top.values[2]
        .clone()
        .or_else(|| (o.env)("DASHBOARD_ACTOR"))
        .unwrap_or_else(|| "architect".into());
    let opts = StoreOptions {
        actor: Some(actor),
        ..StoreOptions::default()
    };
    let store = match PyStore::new(&root, opts, o.env, &o.cwd, o.schema_dir(), o.clock.clone()) {
        Ok(st) => st,
        Err(e) => {
            let (line, code) = failure(Fail::Store(e));
            return out(String::new(), err + &line, code);
        }
    };
    let ctx = Ctx::new(store, o, tail, err);
    let result = dispatch(&ctx, cmd, &a, stdin);
    let stderr = ctx.stderr();
    match result {
        Ok(Out::Report(v)) => out(emit(&v), stderr, summary::report_exit(&v)),
        Ok(Out::Json(v)) => out(emit(&v), stderr, 0),
        Ok(Out::Lines(v)) => out(lines(v), stderr, 0),
        Ok(Out::Text(t)) => out(t, stderr, 0),
        Err(f) => {
            let (line, code) = failure(f);
            out(String::new(), stderr + &line, code)
        }
    }
}
