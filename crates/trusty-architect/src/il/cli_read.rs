//! The read verbs of `datg`: show, digest, fresh, acts, reminders, archive-show,
//! and the poller's `remind-fire`.
//!
//! Why: these run on every resume and every poll. They take no lock (a reader
//! sees `state.json` whole because writers rename it into place) and tolerate a
//! torn last log line.
//! What: [`read_verb`] and [`remind_fire`]. `digest` appends one `digest` log
//! line unless `--no-log`; a failure of that one append is printed and does not
//! fail the verb, as in `datg.py` (no ledger state changes). `hook`,
//! `check-dispatch` and `stamp` are refused with a pointer to `datg.py`.
//! Test: `golden_steps_match_python_byte_for_byte`, `fresh_exit_codes_and_messages`,
//! `since_accepts_iso_and_epoch`.

use super::act::{fmt_act, parse_dur, read_acts};
use super::args::Spec;
use super::cli::{Cx, parse_args, session_name};
use super::digest::build_digest;
use super::error::{IlError, IlResult, invalid};
use super::freshness::fresh;
use super::json::{Json, obj};
use super::remind::{due, fmt_row, listing, row};
use super::store::{Where, load_state};

const DIGEST: Spec = Spec {
    values: &["max-tokens"],
    flags: &["no-log"],
    repeat: &[],
};
const FRESH: Spec = Spec {
    values: &["since", "max-age"],
    flags: &[],
    repeat: &[],
};
const ACTS: Spec = Spec {
    values: &["since", "goal", "limit"],
    flags: &["json"],
    repeat: &[],
};
const REMINDERS: Spec = Spec {
    values: &[],
    flags: &["due", "all", "json"],
    repeat: &[],
};
const SHOW: Spec = Spec {
    values: &[],
    flags: &["brief"],
    repeat: &[],
};

/// `summary`: `TOP <goal>; OPEN ids; ANSWERED ids`.
fn summary(st: &Json) -> String {
    let mut goals: Vec<&Json> = st
        .arr("goals")
        .iter()
        .filter(|g| g.s("status") == "active")
        .collect();
    goals.sort_by_key(|g| g.i("rank").unwrap_or(0));
    let ids = |status: &str| -> Vec<&str> {
        st.arr("items")
            .iter()
            .filter(|i| i.s("status") == status)
            .map(|i| i.s("id"))
            .take(5)
            .collect()
    };
    let mut parts = Vec::new();
    if let Some(g) = goals.first() {
        let title: String = g.s("title").chars().take(60).collect();
        parts.push(format!("TOP {} {title}", g.s("id")));
    }
    for (label, status) in [("OPEN", "open"), ("ANSWERED", "answered")] {
        let v = ids(status);
        if !v.is_empty() {
            parts.push(format!("{label} {}", v.join(",")));
        }
    }
    parts.join("; ")
}

fn int_opt(p: &super::args::Parsed, k: &str) -> IlResult<Option<i64>> {
    p.int(k).map_err(IlError::Invalid)
}

/// Runs a read verb; returns its exit code.
pub fn read_verb(cx: &mut Cx, w: &Where, verb: &str, rest: &[String]) -> IlResult<i32> {
    let now = cx.clock.now_unix();
    match verb {
        "show" => {
            let p = parse_args(rest, &SHOW)?;
            let st = load_state(&cx.ledger(w)?.paths)?;
            if p.flag("brief") {
                let s = summary(&st);
                cx.out
                    .println(if s.is_empty() { "no open items" } else { &s });
            } else {
                cx.out.println(&st.dump_pretty());
            }
        }
        "digest" => {
            let p = parse_args(rest, &DIGEST)?;
            let max = int_opt(&p, "max-tokens")?.unwrap_or(3000);
            let ledger = cx.ledger(w)?;
            let st = load_state(&ledger.paths)?;
            let name = session_name(cx, w, st.s("code"))?;
            let text = build_digest(&ledger.paths, &name, &st, max, now);
            cx.out.println(&text);
            if !p.flag("no-log") {
                let ctx = cx.ctx();
                let rec = obj([
                    ("ts", ctx.now_iso().into()),
                    ("session", ctx.env_session.as_str().into()),
                    ("kind", "digest".into()),
                ]);
                if let Err(e) = ledger.append_log(format!("{}\n", rec.dump()).as_bytes()) {
                    cx.out.eprintln(&format!("datg: log write failed ({e})"));
                }
            }
        }
        "fresh" => {
            let p = parse_args(rest, &FRESH)?;
            let max_age = p.float("max-age").map_err(IlError::Invalid)?;
            let (code, msg) = fresh(&cx.ledger(w)?.paths, p.val("since"), max_age, now);
            cx.out.println(&msg);
            return Ok(code);
        }
        "acts" => {
            let p = parse_args(rest, &ACTS)?;
            let since = p
                .val("since")
                .filter(|s| !s.is_empty())
                .map(parse_dur)
                .transpose()?;
            let limit = int_opt(&p, "limit")?.and_then(|l| usize::try_from(l).ok());
            let rows = read_acts(&cx.ledger(w)?.paths, since, p.val("goal"), limit, now);
            let lines: Vec<String> = rows
                .iter()
                .map(|r| {
                    if p.flag("json") {
                        r.dump_spaced()
                    } else {
                        fmt_act(r, None)
                    }
                })
                .collect();
            cx.out.println(&lines.join("\n"));
        }
        "reminders" => {
            let p = parse_args(rest, &REMINDERS)?;
            let st = load_state(&cx.ledger(w)?.paths)?;
            let rows = listing(&st, now.floor() as i64, p.flag("due"), p.flag("all"));
            let lines: Vec<String> = rows
                .iter()
                .map(|r| {
                    if p.flag("json") {
                        r.dump_spaced()
                    } else {
                        fmt_row(r)
                    }
                })
                .collect();
            cx.out.println(&lines.join("\n"));
        }
        "archive-show" => {
            let p = parse_args(rest, &Spec::default())?;
            let Some(id) = p.pos.first() else {
                return invalid("the following arguments are required: id");
            };
            let line = cx.ledger(w)?.archive_show(id)?;
            cx.out.println(&line);
        }
        "hook" | "check-dispatch" | "stamp" => {
            return invalid(format!(
                "{verb} is not ported to trusty-architect-il; keep calling datg.py {verb}"
            ));
        }
        other => return invalid(format!("argument verb: invalid choice: {other:?}")),
    }
    Ok(0)
}

/// `remind-fire [--dry-run]`: fires every due reminder once; one line per firing.
pub fn remind_fire(cx: &mut Cx, w: &Where, rest: &[String]) -> IlResult<i32> {
    let p = parse_args(
        rest,
        &Spec {
            values: &[],
            flags: &["dry-run"],
            repeat: &[],
        },
    )?;
    let ledger = cx.ledger(w)?;
    let now = cx.clock.now_unix().floor() as i64;
    let st = load_state(&ledger.paths)?;
    let pairs = due(&st, now);
    let prefix = if p.flag("dry-run") {
        "would fire "
    } else {
        "fired "
    };
    if p.flag("dry-run") || pairs.is_empty() {
        if p.flag("dry-run") {
            for (ti, ri) in pairs {
                let t = &st.arr("tasks")[ti];
                cx.out.println(&format!(
                    "{prefix}{}",
                    fmt_row(&row(t, &t.arr("reminders")[ri]))
                ));
            }
        }
        return Ok(0);
    }
    let keys: Vec<Json> = pairs
        .iter()
        .map(|&(ti, ri)| {
            let t = &st.arr("tasks")[ti];
            Json::Arr(vec![
                t.s("id").into(),
                t.arr("reminders")[ri]
                    .get("n")
                    .cloned()
                    .unwrap_or(Json::Null),
                t.arr("reminders")[ri].s("at").into(),
            ])
        })
        .collect();
    let op = obj([
        ("op", "remind_fire".into()),
        ("keys", Json::Arr(keys)),
        ("now", super::time::fmt_min(now).into()),
    ]);
    let mut ctx = cx.ctx();
    let (evs, _) = ledger.write_ops(&mut ctx, &[op])?;
    for e in evs {
        if let (Some(title), Some(rem)) = (e.task, e.reminder) {
            let mut r = row(
                &obj([("id", e.id.as_str().into()), ("title", title.into())]),
                &rem,
            );
            r.set("at", rem.s("fired_for"));
            cx.out.println(&format!("{prefix}{}", fmt_row(&r)));
        }
    }
    cx.flush_warnings(&ctx, "datg");
    Ok(0)
}
