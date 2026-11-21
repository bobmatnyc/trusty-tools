//! The bounded resume digest.
//!
//! Why: a session resumes from one text block. The digest must be a pure function
//! of the ledger files plus one wall-clock read (`prog Nh ago`, `STALE`), and
//! byte-identical to `datg.py digest` so the two can run side by side.
//! What: [`build_digest`] renders HEAD, SESSION, GOALS, ITEMS, TASKS, RECENT (the
//! last ten escalated/answered/done/drift lines of the log tail), RECENT ACTIONS
//! and POINTERS, then cuts to `max_tokens * 4 - 60` characters in `datg.py`'s
//! order: RECENT, oldest tasks, briefs and archive pointers, trailing items, a
//! hard cut, ending `TRUNCATED: N bytes dropped`. Lengths count characters.
//! Test: `golden_steps_match_python_byte_for_byte` (digest at fourteen budgets),
//! `digest_is_byte_equal_to_python`, `token_label_matches_python`, `progress_ages_and_stale_flag`.

use super::act::{ACT_DIGEST_N, ACT_DIGEST_W, fmt_act, read_acts};
use super::briefs::find_brief_paths;
use super::freshness::fresh;
use super::json::Json;
use super::limits::short;
use super::log::recent;
use super::remind;
use super::session::OWED_MAX;
use super::size::SIZE_WARN;
use super::store::{Paths, size_of};
use super::time::parse_ts;
use super::wip::task_priority;

const STALE_HOURS: f64 = 24.0;
const RECENT_TAIL: u64 = 262_144;

fn is_active(t: &Json) -> bool {
    matches!(t.s("status"), "open" | "running")
}

/// `_k`: `12k`, `3.2k`, or the bare number.
fn k(n: i64) -> String {
    if n >= 10_000 {
        format!("{}k", (n as f64 / 1000.0).round_ties_even() as i64)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// `token_label`: ` tok act 12k/3.2k est 20k/5k x0.6/0.6 [model]`, empty without estimate or actual.
pub fn token_label(t: &Json) -> String {
    let [ei, eo, ai, ao] =
        ["est_in", "est_out", "act_in", "act_out"].map(|f| t.opt(f).and_then(Json::as_i64));
    if [ei, eo, ai, ao].iter().all(Option::is_none) {
        return String::new();
    }
    let mut out = format!(" tok act {}/{}", k(ai.unwrap_or(0)), k(ao.unwrap_or(0)));
    if ei.is_some() || eo.is_some() {
        let r = |a: i64, e: Option<i64>| match e {
            Some(e) if e != 0 => format!("{:.1}", a as f64 / e as f64),
            _ => "-".to_owned(),
        };
        out.push_str(&format!(
            " est {}/{} x{}/{}",
            k(ei.unwrap_or(0)),
            k(eo.unwrap_or(0)),
            r(ai.unwrap_or(0), ei),
            r(ao.unwrap_or(0), eo)
        ));
    } else {
        out.push_str(" est -");
    }
    let m = t.s("act_models");
    if !m.is_empty() {
        out.push_str(&format!(" [{m}]"));
    }
    out
}

fn or_dash(s: &str) -> &str {
    if s.is_empty() { "-" } else { s }
}

/// `_progress`: ` | pct 25 est X rem Y prog 3h ago`, plus ` STALE` after 24 h; empty unless active.
fn progress(t: &Json, now: f64) -> String {
    if !is_active(t) {
        return String::new();
    }
    let stamp = [t.s("progress_at"), t.s("created")]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or("");
    let age = parse_ts(stamp).map(|ts| (now - ts as f64) / 3600.0);
    let pct = match t.opt("pct") {
        Some(p) => p.py_str(),
        None => "-".to_owned(),
    };
    let mut out = format!(
        " | pct {pct} est {} rem {}{}",
        or_dash(t.s("est")),
        or_dash(t.s("remaining")),
        token_label(t)
    );
    let when = match age {
        Some(a) if a >= 1.0 => format!("{}h", a as i64),
        Some(_) => "<1h".to_owned(),
        None => "?".to_owned(),
    };
    out.push_str(&format!(" prog {when} ago"));
    if age.is_none_or(|a| a > STALE_HOURS) {
        out.push_str(" STALE");
    }
    out
}

struct Sections {
    head: Vec<String>,
    sess: Vec<String>,
    goals: Vec<String>,
    items: Vec<String>,
    tasks: Vec<String>,
    log: Vec<String>,
    ptr: Vec<String>,
}

fn session_lines(ses: &Json) -> Vec<String> {
    let mut sess = vec!["SESSION".to_owned()];
    if !ses.get("task").is_some_and(Json::truthy) && !ses.get("next").is_some_and(Json::truthy) {
        sess.push("  none recorded".to_owned());
    }
    for key in ["task", "next"] {
        if let Some(v) = ses.get(key).filter(|v| v.truthy()) {
            sess.push(format!("  {key}: {}", v.py_str()));
        }
    }
    for w in ses.arr("waiting_on") {
        sess.push(format!(
            "  waiting_on {}: {} (since {})",
            w.s("who"),
            w.s("what"),
            w.s("since")
        ));
    }
    match ses.get("owed") {
        Some(owed) => {
            let rows = owed.as_arr().unwrap_or(&[]);
            if !rows.is_empty() {
                sess.push(format!("  owed ({}/{OWED_MAX}):", rows.len()));
            }
            for o in rows {
                sess.push(format!(
                    "  owed: {} = {} @ {}",
                    o.s("check"),
                    o.s("expect"),
                    o.s("ref")
                ));
            }
            if rows.is_empty() {
                sess.push("  owed: none (stated)".to_owned());
            }
        }
        None => sess.push("  owed: NOT RECORDED".to_owned()),
    }
    if let Some(agents) = ses.get("agents") {
        let list: Vec<String> = agents
            .as_arr()
            .unwrap_or(&[])
            .iter()
            .map(|a| format!("{} [{}] {}", a.s("id"), a.s("goal"), a.s("what")))
            .collect();
        sess.push(format!(
            "  agents: {}",
            if list.is_empty() {
                "none".to_owned()
            } else {
                list.join("; ")
            }
        ));
    }
    if let Some(crons) = ses.get("crons") {
        let rows = crons.as_arr().unwrap_or(&[]);
        sess.extend(rows.iter().map(|c| {
            format!(
                "  cron {} [{}] {}",
                c.s("id"),
                c.s("schedule"),
                c.s("prompt")
            )
        }));
        if rows.is_empty() {
            sess.push("  crons: none".to_owned());
        }
    }
    if let Some(b) = ses.get("boundary_at").filter(|b| b.truthy()) {
        sess.push(format!("  boundary_at: {}", b.py_str()));
    }
    for r in ses.arr("refs") {
        sess.push(format!("  ref {}: {}", r.s("key"), r.s("value")));
    }
    if let Some(cu) = ses.get("ctx_use").filter(|c| c.truthy()) {
        let f = |key: &str| cu.get(key).map_or_else(String::new, Json::py_str);
        sess.push(format!(
            "  ctx_use: active {}% stale {}% ctx {} at {} keep: {}",
            f("active"),
            f("stale"),
            f("ctx"),
            f("at"),
            f("keep")
        ));
    }
    sess
}

fn sections(paths: &Paths, name: &str, st: &Json, now: f64) -> Sections {
    let ses = st.get("session").cloned().unwrap_or(Json::Obj(Vec::new()));
    let (code, _) = fresh(paths, None, None, now);
    let fresh_word = match code {
        0 => "fresh",
        1 => "STALE",
        _ => "unknown",
    };
    let ctx = ses.get("ctx_use").and_then(|c| c.i("ctx"));
    let written = ses
        .get("written_at")
        .map_or_else(|| "never".to_owned(), Json::py_str);
    let mut head = vec![format!(
        "IL DIGEST {name} ({}) v{} written_at={written} IL={fresh_word}{}",
        st.s("code"),
        st.i("v").unwrap_or(0),
        ctx.map_or_else(String::new, |c| format!(
            " ctx_before_clear={}k",
            c.div_euclid(1000)
        ))
    )];
    let size = size_of(&paths.state) as usize;
    if size > SIZE_WARN {
        head.push(format!(
            "SIZE: state.json {size} B over the {SIZE_WARN} B bar; run datg.py prune"
        ));
    }
    let mut active: Vec<&Json> = st
        .arr("goals")
        .iter()
        .filter(|g| g.s("status") == "active")
        .collect();
    active.sort_by_key(|g| g.i("rank").unwrap_or(0));
    let mut goals = vec!["GOALS".to_owned()];
    for g in active {
        let maps = g
            .get("maps")
            .filter(|m| m.truthy())
            .map_or_else(String::new, |m| format!(" -> {}", m.py_str()));
        let finish = g
            .get("finish")
            .filter(|m| m.truthy())
            .map_or_else(String::new, |m| format!(" -> {}", m.py_str()));
        goals.push(format!(
            "  {}{maps} r{} {}{finish}",
            g.s("id"),
            g.i("rank").unwrap_or(0),
            g.s("title")
        ));
    }
    let mut items = vec!["ITEMS (open/answered)".to_owned()];
    for i in st
        .arr("items")
        .iter()
        .filter(|i| matches!(i.s("status"), "open" | "answered"))
    {
        let rec = i
            .arr("opts")
            .iter()
            .find(|o| o.get("rec").is_some_and(Json::truthy))
            .map_or("", |o| o.s("l"));
        let mut line = format!(
            "  {} [{}]{} {}",
            i.s("id"),
            i.s("status"),
            if i.get("esc").is_some_and(Json::truthy) {
                " esc"
            } else {
                ""
            },
            i.s("title")
        );
        if let Some(a) = i.get("answer").filter(|a| a.truthy()) {
            line.push_str(&format!(" | answer: {}", a.py_str()));
        }
        if !rec.is_empty() && i.s("status") == "open" {
            line.push_str(&format!(" | rec: {rec}"));
        }
        items.push(line);
    }
    let tasks = st
        .arr("tasks")
        .iter()
        .filter(|t| t.s("status") != "done")
        .map(|t| {
            let overflow = if t.get("overflow").is_some_and(Json::truthy) {
                " overflow"
            } else {
                ""
            };
            let mut lines = vec![format!(
                "  {} [{}]{overflow} P{} {} {}{}",
                t.s("id"),
                t.s("status"),
                task_priority(st, t),
                t.s("goal"),
                t.s("title"),
                progress(t, now)
            )];
            if let Some(dw) = t.get("done_when").filter(|d| d.truthy()) {
                lines.push(format!("    done_when: {}", short(&dw.py_str(), 200)));
            }
            lines.extend(remind::lines(t));
            lines.join("\n")
        })
        .collect();
    let mut recent_rows = recent(
        &paths.log,
        Some(RECENT_TAIL),
        &["escalated", "answered", "done", "drift"],
    );
    let from = recent_rows.len().saturating_sub(10);
    let mut log = vec!["RECENT".to_owned()];
    for r in recent_rows.drain(from..) {
        let when = ["t", "ts"]
            .iter()
            .find_map(|k| r.rec.get(k).filter(|v| v.truthy()))
            .map_or_else(String::new, Json::py_str);
        let note = ["note", "text"]
            .iter()
            .find_map(|k| r.rec.get(k).filter(|v| v.truthy()))
            .map_or_else(String::new, Json::py_str);
        log.push(format!(
            "  {when} {} {} {}",
            r.rec.s("id"),
            r.kind,
            short(&note, 80)
        ));
    }
    let acts = read_acts(paths, None, None, Some(ACT_DIGEST_N), now);
    if !acts.is_empty() {
        log.push(format!("RECENT ACTIONS (last {ACT_DIGEST_N}, newest last)"));
        log.extend(
            acts.iter()
                .map(|r| format!("  {}", fmt_act(r, Some(ACT_DIGEST_W)))),
        );
    }
    let mut briefs = find_brief_paths(&st.dump_spaced());
    briefs.sort();
    briefs.dedup();
    let mut ptr = vec![
        "POINTERS".to_owned(),
        format!("  archive: {}", paths.archive.display()),
    ];
    ptr.extend(briefs.into_iter().take(8).map(|b| format!("  brief: {b}")));
    Sections {
        head,
        sess: session_lines(&ses),
        goals,
        items,
        tasks,
        log,
        ptr,
    }
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

/// `build_digest`: the digest text for `st`, `name` being the session or code shown in HEAD.
pub fn build_digest(paths: &Paths, name: &str, st: &Json, max_tokens: i64, now: f64) -> String {
    let s = sections(paths, name, st, now);
    let limit = (max_tokens.saturating_mul(4).max(200) - 60) as usize;
    let render = |tk: &[String], with_log: bool, with_ptr: bool, it: &[String]| -> String {
        let tasks_head = ["TASKS (open/running)".to_owned()];
        let tasks: &[String] = if tk.is_empty() { &[] } else { &tasks_head };
        [
            &s.head[..],
            &s.sess,
            &s.goals,
            it,
            tasks,
            tk,
            if with_log { &s.log } else { &[] },
            if with_ptr { &s.ptr } else { &[] },
        ]
        .concat()
        .join("\n")
    };
    let full = render(&s.tasks, true, true, &s.items);
    if chars(&full) <= limit + 60 {
        return full;
    }
    let (mut tk, mut it) = (s.tasks.clone(), s.items.clone());
    let mut out = String::new();
    for step in ["log", "tasks", "ptr", "items", "cut"] {
        match step {
            "log" => out = render(&tk, false, true, &it),
            "tasks" => {
                while !tk.is_empty() && chars(&render(&tk, false, true, &it)) > limit {
                    tk.remove(0); // oldest first
                }
                out = render(&tk, false, true, &it);
            }
            "ptr" => out = render(&tk, false, false, &it), // briefs and archive go; refs pointers are always empty
            "items" => {
                while it.len() > 1 && chars(&render(&tk, false, false, &it)) > limit {
                    it.pop();
                }
                out = render(&tk, false, false, &it);
            }
            _ => out = out.chars().take(limit).collect(),
        }
        if chars(&out) <= limit {
            break;
        }
    }
    format!(
        "{out}\nTRUNCATED: {} bytes dropped",
        chars(&full) - chars(&out)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::json::obj;

    #[test]
    fn token_label_matches_python() {
        let mut t = obj([
            ("act_in", 12_500.into()),
            ("act_out", 3200.into()),
            ("est_in", 20_000.into()),
            ("est_out", 5000.into()),
        ]);
        assert_eq!(token_label(&t), " tok act 12k/3.2k est 20k/5.0k x0.6/0.6");
        t.set("act_models", "opus");
        assert!(token_label(&t).ends_with(" [opus]"));
        assert_eq!(
            token_label(&obj([("act_in", 5.into())])),
            " tok act 5/0 est -"
        );
        assert_eq!(token_label(&obj([("title", "x".into())])), "");
        assert_eq!(
            token_label(&obj([("est_in", 0.into())])),
            " tok act 0/0 est 0/0 x-/-"
        );
    }

    #[test]
    fn progress_ages_and_stale_flag() {
        let now = parse_ts("2026-11-20T12:00Z").unwrap() as f64;
        let t = obj([
            ("status", "open".into()),
            ("progress_at", "2026-11-20T10:30Z".into()),
            ("pct", 0.into()),
        ]);
        assert_eq!(progress(&t, now), " | pct 0 est - rem - prog 1h ago");
        let old = obj([
            ("status", "running".into()),
            ("created", "2026-11-18T12:00Z".into()),
        ]);
        assert_eq!(
            progress(&old, now),
            " | pct - est - rem - prog 48h ago STALE"
        );
        let none = obj([("status", "open".into())]);
        assert_eq!(
            progress(&none, now),
            " | pct - est - rem - prog ? ago STALE"
        );
        assert_eq!(progress(&obj([("status", "parked".into())]), now), "");
    }
}
