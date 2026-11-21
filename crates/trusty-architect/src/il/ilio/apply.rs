//! `ilio apply`, the ledger part: an IL block to one batch of ops.
//!
//! Why: the model ends a turn with a block; one commit must record its session
//! `next`, task note and progress, `close:` ids, `act:` lines, reminders and
//! `new:` items, or none of it. The Architect ruled that Rust owns the ledger files
//! only: the dashboard store, the steering log and `last-apply.json` stay in
//! Python, which can call `datg batch` with the ops this builds.
//! What: [`plan`] builds the ops from the block and a snapshot of the state;
//! [`apply_block`] runs them through [`crate::il::Ledger::write_ops`]. A `new:`
//! D or A needs a dashboard item first, so it is refused unless `no_dashboard`
//! says to write the ledger item alone.
//! Test: `golden_steps_match_python_byte_for_byte` (apply, pct-unchanged and missing-done_when steps),
//! `apply_new_decision_and_action_need_the_dashboard_flag`, `apply_blocks_write_the_same_ledger_as_python`.

use super::newline::{lists_lettered_choices, parse_d_opts, parse_new, parse_subs, sentence_count};
use super::{parent_goal, parse_block, task_id};
use crate::il::Ledger;
use crate::il::error::{IlResult, invalid};
use crate::il::json::{Json, obj};
use crate::il::limits::TITLE_MAX;
use crate::il::ops::Ctx;
use crate::il::time::fmt_sec;
use crate::pystr::{py_repr, strip};

fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn get<'a>(rows: &'a [(String, String)], k: &str) -> &'a str {
    rows.iter()
        .rev()
        .find(|(n, _)| n == k)
        .map_or("", |(_, v)| v.as_str())
}

fn or_null(s: &str) -> Json {
    if s.is_empty() { Json::Null } else { s.into() }
}

fn act_op(raw: &str, rows: &[(String, String)], goal: &str, who: &str) -> IlResult<Json> {
    let mut parts = raw.split(" | ").map(|x| strip(x).to_owned());
    let what = parts.next().unwrap_or_default();
    let mut kv: Vec<(String, String)> = Vec::new();
    for x in parts {
        let known = x
            .split_once(':')
            .filter(|(k, _)| matches!(strip(k), "owner" | "ev" | "goal" | "refs"));
        let Some((k, v)) = known else {
            return invalid(format!(
                "act: unknown part {}; use | owner: | ev: | goal: | refs:",
                py_repr(&cut(&x, 40))
            ));
        };
        let v = strip(v).trim_matches('"').to_owned();
        kv.retain(|(n, _)| n != strip(k));
        kv.push((strip(k).to_owned(), v));
    }
    let pick = |k: &str| {
        kv.iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
            .filter(|v| !v.is_empty())
    };
    Ok(obj([
        ("op", "act".into()),
        ("who", who.into()),
        ("what", what.into()),
        ("owner_words", pick("owner").unwrap_or_default().into()),
        (
            "evidence",
            pick("ev")
                .unwrap_or_else(|| get(rows, "evidence").to_owned())
                .into(),
        ),
        (
            "goal",
            pick("goal").unwrap_or_else(|| goal.to_owned()).into(),
        ),
        (
            "refs",
            pick("refs")
                .unwrap_or_else(|| get(rows, "refs").to_owned())
                .into(),
        ),
    ]))
}

fn new_op(
    raw: &str,
    st: &Json,
    rows: &[(String, String)],
    made: &[Json],
) -> IlResult<Option<Json>> {
    let form = || {
        invalid(format!(
            "new: not in the form K \"title\" [goal G] [session=S] [brief=P] [task=T] [multistep] [done_when=\"acceptance criteria, T requires it\"] [detail=\"plain context, D needs 120+ chars\"] [subs=\"k: question [:: detail] [rec=yes|no] || k2: ...\" (D only)] [opts \"Label :: sentences\"*|\"Label :: sentences\" (D; A may use `opts done*`)]: {}",
            cut(raw, 80)
        ))
    };
    let Some(m) = parse_new(raw) else {
        return form();
    };
    let tlen = m.title.chars().count();
    if tlen > TITLE_MAX {
        return invalid(format!(
            "new: title is {tlen} chars, over {TITLE_MAX}: {}...",
            cut(&m.title, 60)
        ));
    }
    let dtl = strip(m.detail.as_deref().unwrap_or("")).to_owned();
    let head = cut(raw, 60);
    if m.kind == 'D' && (dtl.chars().count() < 120 || sentence_count(&dtl) < 3) {
        return invalid(format!(
            "new: D needs detail=\"...\" of at least 120 characters and 3 sentences, one paragraph in plain language; load architect-writing-style: {head}"
        ));
    }
    let goal = m
        .goal
        .clone()
        .unwrap_or_else(|| parent_goal(st, get(rows, "task")));
    let mut op = obj([
        ("op", "add".into()),
        ("kind", m.kind.to_string().into()),
        ("title", m.title.as_str().into()),
        ("goal", goal.into()),
    ]);
    match (&m.done_when, m.kind) {
        (Some(w), _) => op.set("done_when", w.as_str()),
        (None, 'T') => {
            return invalid(format!(
                "new: T needs done_when=\"acceptance criteria\" (one line, `;`-separated): {head}"
            ));
        }
        _ => {}
    }
    if m.toks.iter().any(|t| t == "multistep") {
        op.set("multistep", true);
    }
    if m.detail.is_some() {
        op.set("detail", m.detail.as_deref().unwrap_or(""));
    }
    if let Some(s) = &m.subs {
        if m.kind != 'D' {
            return invalid(format!("new: subs= is for D only: {head}"));
        }
        op.set("subs", parse_subs(s)?);
    }
    if m.kind == 'D' && lists_lettered_choices(&dtl) {
        return invalid(format!(
            "new: D detail lists lettered choices (Option a/b); move the choices into opts \"Label :: sentences\"*|\"Label :: sentences\": {head}"
        ));
    }
    if let Some(o) = &m.opts {
        let opts = if m.kind == 'D' {
            parse_d_opts(o, raw)?
        } else {
            plain_opts(&o.split('|').collect::<Vec<_>>().join(","))
        };
        op.set("opts", opts);
    }
    let kind_name = match m.kind {
        'D' => Some("question"),
        'A' => Some("action"),
        _ => None,
    };
    let dup = st.arr("items").iter().any(|i| {
        Some(i.s("kind")) == kind_name && i.s("status") == "open" && i.s("title") == m.title
    }) || (m.kind == 'T'
        && st
            .arr("tasks")
            .iter()
            .any(|t| t.s("title") == m.title && t.s("status") != "done"))
        || made
            .iter()
            .any(|o| o.s("title") == m.title && o.s("kind") == m.kind.to_string());
    Ok((!dup).then_some(op))
}

/// `datg._opts`: `"a*,b"` to `[{l, rec}]`.
fn plain_opts(s: &str) -> Json {
    Json::Arr(
        s.split(',')
            .map(|p| strip(p).to_owned())
            .filter(|p| !p.is_empty())
            .map(|p| {
                obj([
                    ("l", p.trim_end_matches('*').into()),
                    ("rec", p.ends_with('*').into()),
                ])
            })
            .collect(),
    )
}

/// What `apply` decided to write.
pub struct Plan {
    /// The ops, in `ilio.py`'s order.
    pub ops: Vec<Json>,
}

/// `apply_block` planning: the ops for `text` against a snapshot `st`.
///
/// # Errors
/// `Invalid` for an unchanged `pct`, a bad `new:`/`act:` line, or a D/A without `no_dashboard`.
pub fn plan(text: &str, st: &Json, who: &str, now: i64, no_dashboard: bool) -> IlResult<Plan> {
    let parsed = parse_block(text).unwrap_or_default();
    let rows = &parsed[..];
    let mut ops = Vec::new();
    if !get(rows, "next").is_empty() {
        ops.push(obj([
            ("op", "session".into()),
            ("next", cut(get(rows, "next"), 200).into()),
        ]));
    }
    let tid = task_id(st, get(rows, "task"));
    let note: String = cut(
        &["decided", "delta"]
            .iter()
            .filter(|k| !get(rows, k).is_empty())
            .map(|k| format!("{k}: {}", get(rows, k)))
            .collect::<Vec<_>>()
            .join("; "),
        200,
    );
    if let Some(tid) = &tid
        && !note.is_empty()
    {
        ops.push(obj([
            ("op", "pulse".into()),
            ("id", tid.as_str().into()),
            ("note", note.into()),
        ]));
    }
    let pct = get(rows, "pct");
    let pct_chars: Vec<char> = pct.chars().collect();
    if let Some(tid) = &tid
        && let Some(n) = (!pct.is_empty())
            .then(|| crate::pystr::parse_digits(&pct_chars))
            .flatten()
    {
        let t0 = st.arr("tasks").iter().find(|t| t.s("id") == *tid);
        let done = strip(get(rows, "done")).to_lowercase();
        if t0.and_then(|t| t.i("pct")) == i64::try_from(n).ok()
            && !matches!(done.as_str(), "" | "none" | "nothing" | "-")
            && strip(get(rows, "remaining")).is_empty()
        {
            return invalid(format!(
                "pct {pct} is unchanged for {tid} but done: claims progress; move pct or give remaining:"
            ));
        }
        ops.push(obj([
            ("op", "pulse".into()),
            ("id", tid.as_str().into()),
            ("pct", pct.into()),
            ("progress_note", cut(get(rows, "done"), 200).into()),
            ("est", or_null(get(rows, "est"))),
            ("remaining", or_null(get(rows, "remaining"))),
            ("progress_at", fmt_sec(now).into()),
        ]));
    }
    let live_ids: Vec<String> = crate::il::ilio::ledger_ids(st)
        .into_iter()
        .filter(|(_, _, s, _)| s != "done")
        .map(|(i, ..)| i)
        .collect();
    for tok in get(rows, "close")
        .split([' ', ','])
        .filter(|t| !t.is_empty())
    {
        if let Some(full) = super::full_id_of(tok)
            && live_ids.contains(&full)
        {
            let outcome = [get(rows, "evidence"), get(rows, "done")]
                .into_iter()
                .find(|s| !s.is_empty())
                .unwrap_or("closed");
            ops.push(obj([
                ("op", "close".into()),
                ("id", full.into()),
                ("outcome", cut(outcome, 200).into()),
            ]));
        }
    }
    let goal = parent_goal(st, get(rows, "task"));
    for (k, v) in rows {
        if k == "act" {
            ops.push(act_op(v, rows, &goal, who)?);
        }
    }
    for (k, v) in rows {
        if k == "remind"
            && let Some(m) = super::check::parse_remind(v)
        {
            ops.push(obj([
                ("op", "remind".into()),
                ("id", task_id(st, &m.task).map_or(Json::Null, Json::from)),
                ("at", m.at.as_str().into()),
                ("every", m.every.as_deref().map_or(Json::Null, Json::from)),
                ("note", m.note.as_str().into()),
            ]));
        }
    }
    let mut items: Vec<Json> = Vec::new();
    for (k, raw) in rows {
        if k == "new"
            && let Some(op) = new_op(raw, st, rows, &items)?
        {
            items.push(op);
        }
    }
    if !no_dashboard && items.iter().any(|o| matches!(o.s("kind"), "D" | "A")) {
        return invalid(
            "new: D|A needs a dashboard item first (dashboard-db.py stays in Python); pass --no-dashboard to write the ledger item alone",
        );
    }
    ops.extend(items);
    Ok(Plan { ops })
}

/// `apply_block`: plans and writes; returns the summary line and the notes for stderr.
///
/// # Errors
/// Planning refusals and every `write_ops` failure.
pub fn apply_block(
    ledger: &Ledger,
    ctx: &mut Ctx,
    text: &str,
    who: &str,
    no_dashboard: bool,
) -> IlResult<String> {
    let st = ledger.load_for_write()?;
    let plan = plan(text, &st, who, ctx.epoch(), no_dashboard)?;
    if plan.ops.is_empty() {
        return Ok("nothing to write".to_owned());
    }
    let n = plan.ops.len();
    ledger.write_ops(ctx, &plan.ops)?;
    Ok(format!("wrote {n} op(s)"))
}
