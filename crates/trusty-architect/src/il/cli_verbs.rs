//! The `datg` verbs: argv to ops, ops to the ledger, results to stdout.
//!
//! Why: each verb of `datg.py` is a thin shell over the same ops; keeping the
//! shell here keeps `cli.rs` to dispatch and the op bodies pure.
//! What: [`dispatch`] runs one verb. Write verbs build `Json` ops exactly as
//! `datg.py main` does and call [`crate::il::Ledger::write_ops`]; read verbs live
//! in `cli_read.rs`. `update`/`batch` with `esc=architect` then run
//! `$DATG_NOTIFY_CMD` (a failure is logged and printed, never fatal).
//! Test: `golden_steps_match_python_byte_for_byte`, `session_set_merges_and_validates`,
//! `usage_errors_exit_two_and_write_nothing`.

use super::args::Spec;
use super::cli::{Cx, add_op, parse_args, update_op, write_ops_print};
use super::error::{IlError, IlResult, invalid};
use super::json::{Json, obj};
use super::ops::Ev;
use super::store::Where;
use crate::pystr::py_repr;

const REMIND: Spec = Spec {
    values: &["at", "every", "note"],
    flags: &[],
    repeat: &[],
};
const DONE: Spec = Spec {
    values: &[],
    flags: &["end"],
    repeat: &[],
};
const ACT: Spec = Spec {
    values: &["who", "what", "owner-words", "evidence", "goal", "refs"],
    flags: &[],
    repeat: &[],
};
const SESSION: Spec = Spec {
    values: &["task", "next", "ctx-use"],
    flags: &["boundary"],
    repeat: &["wait", "owe", "agent", "cron", "ref"],
};
const PRUNE: Spec = Spec {
    values: &["answered-hours"],
    flags: &["dry-run", "require-outcome"],
    repeat: &[],
};

fn require<'a>(p: &'a super::args::Parsed, k: &str) -> IlResult<&'a str> {
    p.val(k)
        .ok_or_else(|| IlError::Invalid(format!("the following arguments are required: --{k}")))
}

fn positional(p: &super::args::Parsed, n: usize, what: &str) -> IlResult<String> {
    p.pos
        .get(n)
        .cloned()
        .ok_or_else(|| IlError::Invalid(format!("the following arguments are required: {what}")))
}

/// `none` states an empty list; otherwise each value is parsed.
fn list(vals: &[String], parse: impl Fn(&str) -> IlResult<Json>) -> IlResult<Json> {
    if vals == ["none"] {
        return Ok(Json::Arr(Vec::new()));
    }
    Ok(Json::Arr(
        vals.iter().map(|v| parse(v)).collect::<IlResult<_>>()?,
    ))
}

fn need(v: &str, sep: char, n: usize, fmt: &str) -> IlResult<Vec<String>> {
    let parts: Vec<String> = v.splitn(n, sep).map(str::to_owned).collect();
    if parts.len() != n {
        return invalid(format!("expected {fmt}, got {}", py_repr(v)));
    }
    Ok(parts)
}

fn zip(keys: &[&str], parts: Vec<String>) -> Json {
    Json::Obj(
        keys.iter()
            .zip(parts)
            .map(|(k, v)| ((*k).to_owned(), Json::from(v)))
            .collect(),
    )
}

/// `session_op`: the batch-style `session` op from the CLI flags.
fn session_op(rest: &[String]) -> IlResult<Json> {
    let p = parse_args(rest, &SESSION)?;
    if p.pos.first().map(String::as_str) != Some("set") {
        return invalid("argument action: invalid choice (choose from 'set')");
    }
    let mut op = obj([
        ("op", "session".into()),
        ("task", p.val("task").map_or(Json::Null, Json::from)),
        ("next", p.val("next").map_or(Json::Null, Json::from)),
        ("boundary", p.flag("boundary").into()),
    ]);
    if let Some(v) = p.repeated("wait") {
        op.set(
            "waiting_on",
            list(v, |x| {
                Ok(zip(&["who", "what"], need(x, ':', 2, "WHO:WHAT")?))
            })?,
        );
    }
    if let Some(v) = p.repeated("owe") {
        let owe = |x: &str| {
            let parts = need(x, '=', 2, "CHECK=EXPECT@REF")?;
            let (expect, r) = parts[1].split_once('@').unwrap_or((parts[1].as_str(), ""));
            Ok(obj([
                ("check", parts[0].as_str().into()),
                ("expect", expect.into()),
                ("ref", r.into()),
            ]))
        };
        op.set("owed", list(v, owe)?);
    }
    if let Some(v) = p.repeated("agent") {
        op.set(
            "agents",
            list(v, |x| {
                Ok(zip(
                    &["id", "goal", "what"],
                    need(x, ':', 3, "ID:GOAL:WHAT")?,
                ))
            })?,
        );
    }
    if let Some(v) = p.repeated("cron") {
        op.set(
            "crons",
            list(v, |x| {
                Ok(zip(
                    &["id", "schedule", "prompt"],
                    need(x, '=', 3, "ID=SCHEDULE=PROMPT")?,
                ))
            })?,
        );
    }
    if let Some(v) = p.repeated("ref") {
        let r = |x: &str| {
            let parts = need(x, '=', 2, "KEY=VALUE")?;
            let value = if parts[1] == "none" {
                Json::Null
            } else {
                parts[1].as_str().into()
            };
            Ok(obj([("key", parts[0].as_str().into()), ("value", value)]))
        };
        op.set("refs", list(v, r)?);
    }
    if let Some(c) = p.val("ctx-use").filter(|c| !c.is_empty()) {
        let parts = need(c, ':', 4, "ACTIVE:STALE:CTX:KEEP")?;
        let n = |s: &str| {
            s.trim().parse::<i64>().map_err(|_| {
                IlError::Invalid("ctx-use: ACTIVE, STALE, CTX must be integers".into())
            })
        };
        op.set(
            "ctx_use",
            obj([
                ("active", n(&parts[0])?.into()),
                ("stale", n(&parts[1])?.into()),
                ("ctx", n(&parts[2])?.into()),
                ("keep", parts[3].as_str().into()),
            ]),
        );
    }
    Ok(op)
}

fn act_op(rest: &[String]) -> IlResult<Json> {
    let p = parse_args(rest, &ACT)?;
    let get = |k: &str| p.val(k).unwrap_or("");
    Ok(obj([
        ("op", "act".into()),
        ("who", require(&p, "who")?.into()),
        ("what", require(&p, "what")?.into()),
        ("owner_words", get("owner-words").into()),
        ("evidence", get("evidence").into()),
        ("goal", get("goal").into()),
        ("refs", get("refs").into()),
    ]))
}

/// Queues each `escalated` event through `$DATG_NOTIFY_CMD`; a failure is logged and printed.
fn notify_escalations(cx: &mut Cx, w: &Where, evs: &[Ev], st: &Json) -> IlResult<()> {
    let cmd = cx.env.get("DATG_NOTIFY_CMD").map(str::to_owned);
    for e in evs.iter().filter(|e| e.ev == "escalated") {
        let item = st
            .arr("items")
            .iter()
            .find(|i| i.s("id") == e.id)
            .cloned()
            .unwrap_or(Json::Obj(Vec::new()));
        let opts: Vec<String> = item
            .arr("opts")
            .iter()
            .map(|o| {
                format!(
                    "{}{}",
                    o.s("l"),
                    if o.get("rec").is_some_and(Json::truthy) {
                        "*"
                    } else {
                        ""
                    }
                )
            })
            .collect();
        let tail = if opts.is_empty() {
            String::new()
        } else {
            format!(" [options: {}]", opts.join(", "))
        };
        let text = format!("{} {}{tail}", e.id, item.s("title"));
        let sess = super::cli::session_name(cx, w, st.s("code"))?;
        // An unset command is a failure to notify like any other: logged as `notify_failure`.
        let outcome = match &cmd {
            Some(c) => {
                super::notify::run(c, &sess, &text).map_err(|err| (format!("failed: {err}"), err))
            }
            None => Err((
                "skipped: DATG_NOTIFY_CMD unset".to_owned(),
                "DATG_NOTIFY_CMD is not set; datg.py queues it from its own scripts dir".to_owned(),
            )),
        };
        if let Err((logged, err)) = outcome {
            cx.out
                .eprintln(&format!("datg: pm-notify failed for {} ({err})", e.id));
            let ledger = cx.ledger(w)?;
            let ctx = cx.ctx();
            let rec = obj([
                ("ts", ctx.now_iso().into()),
                ("session", sess.as_str().into()),
                ("kind", "notify_failure".into()),
                ("text", format!("{} pm-notify {logged}", e.id).into()),
            ]);
            // The escalation is already in the state; a log line that cannot be written is an error
            // (exit 2), not a note, so a silent gap in the record cannot go unseen.
            ledger.append_log(format!("{}\n", rec.dump()).as_bytes())?;
        }
    }
    Ok(())
}

/// Runs one `datg` verb; returns its exit code.
pub fn dispatch(cx: &mut Cx, w: &Where, verb: &str, rest: &[String]) -> IlResult<i32> {
    match verb {
        "init" => {
            let p = parse_args(
                rest,
                &Spec {
                    values: &["code"],
                    ..Spec::default()
                },
            )?;
            let ledger = cx.ledger(w)?;
            let msg = ledger.init(&cx.ctx(), require(&p, "code")?)?;
            cx.out.println(&msg);
        }
        "add" => {
            write_ops_print(cx, w, &[add_op(rest)?])?;
        }
        "update" => {
            let (op, _) = update_op(rest)?;
            let (evs, st) = write_ops_print(cx, w, &[op])?;
            notify_escalations(cx, w, &evs, &st)?;
        }
        "close" => {
            let p = parse_args(
                rest,
                &Spec {
                    values: &["outcome"],
                    ..Spec::default()
                },
            )?;
            let op = obj([
                ("op", "close".into()),
                ("id", positional(&p, 0, "id")?.into()),
                ("outcome", require(&p, "outcome")?.into()),
            ]);
            write_ops_print(cx, w, &[op])?;
        }
        "rebalance" => {
            let ledger = cx.ledger(w)?;
            let mut ctx = cx.ctx();
            let (evs, _) = ledger.write_ops(&mut ctx, &[obj([("op", "rebalance".into())])])?;
            let lines: Vec<String> = evs
                .iter()
                .map(|e| format!("{}: {}", e.id, e.note))
                .collect();
            cx.out.println(&if lines.is_empty() {
                "rebalance: no change".to_owned()
            } else {
                lines.join("\n")
            });
            cx.flush_warnings(&ctx, "datg");
        }
        "remind" => {
            let p = parse_args(rest, &REMIND)?;
            let op = obj([
                ("op", "remind".into()),
                ("id", positional(&p, 0, "id")?.into()),
                ("at", require(&p, "at")?.into()),
                ("every", p.val("every").map_or(Json::Null, Json::from)),
                ("note", require(&p, "note")?.into()),
            ]);
            write_ops_print(cx, w, &[op])?;
        }
        "remind-done" => {
            let p = parse_args(rest, &DONE)?;
            let n = positional(&p, 1, "n")?;
            let n: i64 = n.trim().parse().map_err(|_| {
                IlError::Invalid(format!("argument n: invalid int value: {}", py_repr(&n)))
            })?;
            let op = obj([
                ("op", "remind_done".into()),
                ("id", positional(&p, 0, "id")?.into()),
                ("n", n.into()),
                ("end", p.flag("end").into()),
            ]);
            write_ops_print(cx, w, &[op])?;
        }
        "act" => {
            let ledger = cx.ledger(w)?;
            let mut ctx = cx.ctx();
            ledger.write_ops(&mut ctx, &[act_op(rest)?])?;
            cx.out.println("act recorded");
        }
        "batch" => {
            let text = (cx.stdin)();
            let ops = Json::parse(&text)
                .map_err(|e| IlError::Invalid(format!("batch input is not JSON ({e})")))?;
            let Json::Arr(ops) = ops else {
                return invalid("batch input must be a JSON list");
            };
            let (evs, st) = write_ops_print(cx, w, &ops)?;
            notify_escalations(cx, w, &evs, &st)?;
        }
        "session" => {
            write_ops_print(cx, w, &[session_op(rest)?])?;
        }
        "prune" => {
            let p = parse_args(rest, &PRUNE)?;
            let hours = p
                .float("answered-hours")
                .map_err(IlError::Invalid)?
                .unwrap_or(24.0);
            let msg = cx.ledger(w)?.prune(
                &cx.ctx(),
                p.flag("dry-run"),
                hours,
                p.flag("require-outcome"),
            )?;
            cx.out.println(&msg);
        }
        "rotate" => {
            let msg = cx.ledger(w)?.rotate(&cx.ctx())?;
            cx.out.println(&msg);
        }
        "remind-fire" => return super::cli_read::remind_fire(cx, w, rest),
        other => return super::cli_read::read_verb(cx, w, other, rest),
    }
    Ok(0)
}
