//! Port of `il_agent.py`: read-only Intent Ledger access for dispatched agents (#12028 P1f).
//!
//! Why: an agent a PM dispatches loads one task row, not the whole ledger, and
//! hands back one `il` block the PM validates before anything is written. The
//! script is the only reader that does this; the port is its drop-in.
//! What: [`render_brief`] (the task, its goal, the cited D/A items and R
//! replies, owed rows, the rules and a handback template), [`render_dispatch`]
//! (the header lines of an Agent prompt) and [`check_handback`] (validate the
//! agent's block, then [`pulse_line`]). A refusal is a [`Reject`], printed as
//! one `IL-REJECT <code> <field>: <expected>` line. No function writes.
//! [`cli::run`] is the command line.
//! Test: `il_agent_recordings_replay_clean_against_the_port`,
//! `shipped_il_agent_handback_validates_stdin`.

pub mod cli;
pub mod pulse;

use crate::il::ilio::check::check;
use crate::il::ilio::idre::{id_full, lookalike_full};
use crate::il::ilio::{FIELDS, Lib, REQUIRED, mmdd, parse_block, relay_text};
use crate::il::json::Json;
use crate::pystr::{is_space, is_word, py_repr, strip};

/// Store writes: the PM's only; an agent proposes them in `need:`.
const WRITE_FIELDS: [&str; 4] = ["new", "remind", "close", "decided"];
/// How many other open items on the task's goal a brief lists.
const MAX_ITEMS: usize = 5;
const RULES: &str = "You read the ledger; you never write it (no datg.py, no ilio.py apply). Never create a task: your work stacks on the task above. No secret values. Propose a new decision or task in `need:`; the PM files it.";

/// A refused input: `IL-REJECT <code> <field>: <expected>`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("IL-REJECT {code} {field}: {expected}")]
pub struct Reject {
    code: &'static str,
    field: String,
    expected: String,
}

fn reject(code: &'static str, field: &str, expected: String) -> Reject {
    Reject {
        code,
        field: field.to_owned(),
        expected,
    }
}

/// The first `n` characters of `s` (a Python slice).
fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `f"{value}"` of a state field: `str()` of the value, `None` when missing.
fn text(v: Option<&Json>) -> String {
    v.map_or_else(|| "None".to_owned(), Json::py_str)
}

/// `_task`: the open task a full task id names, and its creation stamp.
fn task<'a>(st: &'a Json, tok: &str, field: &str) -> Result<(&'a Json, Option<String>), Reject> {
    let tok = strip(tok);
    let m = id_full(tok).filter(|m| m.code.is_some() && m.pre == 'T');
    let Some(m) = m else {
        let code = if lookalike_full(tok) && id_full(tok).is_none() {
            "BADID"
        } else {
            "FIELD"
        };
        let why = format!(
            "{}: a full task id like ar-T01@MMDD (base 34: no o, no l)",
            py_repr(tok)
        );
        return Err(reject(code, field, why));
    };
    let tid = format!("{}-T{}", m.code.as_deref().unwrap_or_default(), m.n);
    let Some(t) = st.arr("tasks").iter().find(|x| x.s("id") == tid) else {
        let why = format!("{tid} is not in the ledger (unknown, or closed and archived)");
        return Err(reject("UNKNOWN-ID", field, why));
    };
    let mm = mmdd(t);
    if let (Some(stamp), Some(mm)) = (m.stamp.as_deref().filter(|s| !s.is_empty()), &mm)
        && stamp != mm
    {
        let why = format!("{tok} was re-issued; the holder was created @{mm}");
        return Err(reject("STALE-ID", field, why));
    }
    if t.s("status") == "done" {
        let why = format!("{tid} is done; ask the PM which task your work stacks on");
        return Err(reject("CLOSED", field, why));
    }
    Ok((t, mm))
}

/// `tid@MMDD`, or the bare id when the task has no creation day.
fn tag(t: &Json, mm: Option<&str>) -> String {
    match mm {
        Some(mm) => format!("{}@{mm}", t.s("id")),
        None => t.s("id").to_owned(),
    }
}

/// One cited extra: an R reply (expanded) or a D/A item.
struct Extra {
    reply: bool,
    label: String,
    text: String,
}

/// `^R[0-9a-z]{2}-[a-z]\b`.
fn looks_like_rref(t: &[char]) -> bool {
    let alnum = |c: &char| c.is_ascii_lowercase() || c.is_ascii_digit();
    t.len() >= 5
        && t[0] == 'R'
        && t[1..3].iter().all(alnum)
        && t[3] == '-'
        && t[4].is_ascii_lowercase()
        && t.get(5).is_none_or(|c| !is_word(*c))
}

/// `re.search(r"\{\w+\}", text)`.
fn has_open_slot(text: &str) -> bool {
    let t: Vec<char> = text.chars().collect();
    (0..t.len()).any(|i| {
        let n = t[i + 1..].iter().take_while(|c| is_word(**c)).count();
        t[i] == '{' && n > 0 && t.get(i + 1 + n) == Some(&'}')
    })
}

/// `_extra`: one extra argument, refused unless it is a known R reply or D/A id.
fn extra(st: &Json, lib: &Lib, tok: &str) -> Result<Extra, Reject> {
    let tok = strip(tok);
    let t: Vec<char> = tok.chars().collect();
    if looks_like_rref(&t) {
        let first = tok.split(is_space).next().unwrap_or_default().to_owned();
        let Some(text) = relay_text(tok, &[], lib) else {
            return Err(reject(
                "RREF",
                "ref",
                format!("{first} is not in the R library"),
            ));
        };
        if has_open_slot(&text) {
            let why = format!("{first} needs its slots filled (k=v after the ref)");
            return Err(reject("RREF", "ref", why));
        }
        return Ok(Extra {
            reply: true,
            label: first,
            text,
        });
    }
    let m = id_full(tok).filter(|m| m.code.is_some() && matches!(m.pre, 'D' | 'A'));
    let Some(m) = m else {
        let why = format!(
            "{}: only a full D/A id (ar-D01) or an R reply (\"R02-a sha=abc\") rides here",
            py_repr(tok)
        );
        return Err(reject("FIELD", "ref", why));
    };
    let iid = format!("{}-{}{}", m.code.as_deref().unwrap_or_default(), m.pre, m.n);
    let Some(it) = st.arr("items").iter().find(|x| x.s("id") == iid) else {
        return Err(reject(
            "UNKNOWN-ID",
            "ref",
            format!("{iid} is not in the ledger"),
        ));
    };
    Ok(Extra {
        reply: false,
        label: iid,
        text: item_line(it),
    })
}

/// `_item_line`: `id status title [opts a|*b] [ans text]`.
fn item_line(it: &Json) -> String {
    let opts: Vec<String> = it
        .arr("opts")
        .iter()
        .map(|o| {
            let rec = if o.get("rec").is_some_and(Json::truthy) {
                "*"
            } else {
                ""
            };
            format!("{rec}{}", cut(o.s("l"), 40))
        })
        .collect();
    let mut line = format!(
        "{} {} {}",
        it.s("id"),
        text(it.get("status")),
        cut(it.s("title"), 120)
    );
    if !opts.is_empty() {
        line += &format!(" opts {}", opts.join("|"));
    }
    if it.get("answer").is_some_and(Json::truthy) {
        line += &format!(" ans {}", cut(it.s("answer"), 120));
    }
    line
}

/// `_template`: the closing `il` block an agent fills in.
fn template(tag: &str) -> String {
    format!(
        "```il\ntask: {tag}\ndone: <what you did, with a sha or path>\npct: <0-100>\nnext: <what the PM should do next>\n```"
    )
}

/// `\b(?:name|...)\b` searched in `hay`.
fn names_any(hay: &str, names: &[String]) -> bool {
    let t: Vec<char> = hay.chars().collect();
    let edge = |i: usize| (i > 0 && is_word(t[i - 1])) != (i < t.len() && is_word(t[i]));
    names.iter().any(|n| {
        let p: Vec<char> = n.chars().collect();
        (0..=t.len()).any(|i| t[i..].starts_with(&p) && edge(i) && edge(i + p.len()))
    })
}

/// The part of an id after its `code-`.
fn bare(id: &str) -> String {
    id.split_once('-')
        .map_or_else(String::new, |(_, b)| b.to_owned())
}

/// `render_brief`: what an agent needs for one task.
///
/// # Errors
/// A [`Reject`] for a task or extra the ledger does not hold.
pub fn render_brief(
    st: &Json,
    lib: &Lib,
    task_tok: &str,
    extras: &[String],
) -> Result<String, Reject> {
    let (t, mm) = task(st, task_tok, "task")?;
    let ex = extras
        .iter()
        .map(|e| extra(st, lib, e))
        .collect::<Result<Vec<_>, _>>()?;
    let goal = st
        .arr("goals")
        .iter()
        .find(|g| g.get("id") == t.get("goal"));
    let tag = tag(t, mm.as_deref());
    let mut out = vec![format!(
        "IL BRIEF {tag} (ledger {}, read-only)",
        text(st.get("code"))
    )];
    out.push(match goal {
        Some(g) => format!(
            "goal: {} r{} {} {}",
            text(g.get("id")),
            text(g.get("rank")),
            text(g.get("status")),
            cut(g.s("title"), 120)
        ),
        None => format!("goal: {}", text(t.get("goal"))),
    });
    if let Some(g) = goal.filter(|g| g.get("finish").is_some_and(Json::truthy)) {
        out.push(format!("done-when (goal): {}", cut(g.s("finish"), 200)));
    }
    let to = t
        .get("to")
        .filter(|v| v.truthy())
        .map_or_else(String::new, |v| format!(" (to {})", v.py_str()));
    out.push(format!(
        "task: {tag} {} {}{to}",
        text(t.get("status")),
        cut(t.s("title"), 200)
    ));
    let shown: Vec<&str> = ex
        .iter()
        .filter(|e| !e.reply)
        .map(|e| e.label.as_str())
        .collect();
    for e in &ex {
        let kind = if e.reply { "reply" } else { "cited" };
        out.push(format!("{kind} {}: {}", e.label, e.text));
    }
    let rest: Vec<&Json> = st
        .arr("items")
        .iter()
        .filter(|i| i.get("goal") == t.get("goal") && i.s("status") != "done")
        .filter(|i| !shown.contains(&i.s("id")))
        .collect();
    for i in rest.iter().take(MAX_ITEMS) {
        out.push(format!("open on this goal: {}", item_line(i)));
    }
    if rest.len() > MAX_ITEMS {
        let n = rest.len() - MAX_ITEMS;
        out.push(format!("... {n} more open items on this goal withheld"));
    }
    let mut names = vec![t.s("id").to_owned(), bare(t.s("id"))];
    names.extend(shown.iter().map(|i| bare(i)));
    let session = st.get("session").filter(|s| s.truthy());
    let owed = session.map_or(&[][..], |s| s.arr("owed"));
    for o in owed.iter().take(50) {
        let (c, e, r) = (text(o.get("check")), text(o.get("expect")), o.get("ref"));
        let r = r.map_or_else(String::new, Json::py_str);
        if names_any(&format!("{c} {e} {r}"), &names) {
            out.push(format!("owed: {c} = {e} @ {r}"));
        }
    }
    out.push(format!("rules: {RULES}"));
    let optional: Vec<&str> = FIELDS
        .iter()
        .copied()
        .filter(|f| !WRITE_FIELDS.contains(f) && !REQUIRED.contains(f))
        .collect();
    out.push(format!(
        "handback: end your final message with one il block, fields in this order. Required: task, done, pct, next (no goal line; the task's parent goal is read from the ledger). Optional: {}. One `field: text` line each, 240 characters at most.",
        optional.join(", ")
    ));
    out.push(template(&tag));
    Ok(out.join("\n"))
}

/// `render_dispatch`: the `Task:`, `IL:`, `Ask:` and `Handback:` lines.
///
/// `brief_cmd` is the command that prints a brief (the script's
/// `python3 <dir>/il_agent.py`, or the port's binary).
///
/// # Errors
/// A [`Reject`] for a task or extra the ledger does not hold.
pub fn render_dispatch(
    st: &Json,
    lib: &Lib,
    task_tok: &str,
    extras: &[String],
    ask: &str,
    project: Option<&str>,
    brief_cmd: &[String],
) -> Result<String, Reject> {
    let (t, mm) = task(st, task_tok, "task")?;
    for e in extras {
        extra(st, lib, e)?;
    }
    let tag = tag(t, mm.as_deref());
    let mut cmd: Vec<String> = brief_cmd.to_vec();
    cmd.extend(["brief".to_owned(), tag.clone()]);
    cmd.extend(extras.iter().cloned());
    if let Some(p) = project.filter(|p| !p.is_empty()) {
        cmd.extend(["--project".to_owned(), p.to_owned()]);
    }
    let quoted: Vec<String> = cmd.iter().map(|c| pulse::shell_quote(c)).collect();
    let mut lines = vec![format!("Task: {tag}"), format!("IL: {}", quoted.join(" "))];
    if !ask.is_empty() {
        lines.push(format!("Ask: {ask}"));
    }
    lines.push(
        "Handback: the il block the IL line prints, nothing else beside it. You never write the ledger."
            .to_owned(),
    );
    Ok(lines.join("\n"))
}

/// The last value of `key` in `rows` (`dict(rows)[key]`).
fn field<'a>(rows: &'a [(String, String)], key: &str) -> Option<&'a str> {
    rows.iter()
        .rev()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// `check_handback`: the agent's block rows, or the reject line to print.
///
/// # Errors
/// The `IL-REJECT ...` line.
pub fn check_handback(
    text: &str,
    st: &Json,
    lib: &Lib,
    expect: Option<&str>,
) -> Result<Vec<(String, String)>, String> {
    let Some(rows) = parse_block(text) else {
        return Err(
            "IL-REJECT NO-BLOCK il: end your final message with one ```il fenced block".to_owned(),
        );
    };
    if let Some((k, _)) = rows
        .iter()
        .find(|(k, _)| WRITE_FIELDS.contains(&k.as_str()))
    {
        return Err(format!(
            "IL-REJECT WRITE-FIELD {k}: agents never write the ledger; propose it in need:"
        ));
    }
    if let Some(err) = check(text, st, lib) {
        return Err(err);
    }
    let given = field(&rows, "task").unwrap_or_default();
    let (t, mm) = task(st, given, "task").map_err(|e| e.to_string())?;
    if let Some(want) = expect.filter(|e| !e.is_empty()) {
        let (w, _) = task(st, want, "dispatched task").map_err(|e| e.to_string())?;
        if w.s("id") != t.s("id") {
            let why = format!("{given} is not the dispatched task {}", w.s("id"));
            return Err(reject("TASK-MISMATCH", "task", why).to_string());
        }
    }
    if let Some(mm) = mm
        && !has_stamp(given, &mm)
    {
        let id = t.s("id");
        return Err(format!(
            "IL-REJECT STAMP task: {id} needs its stamp, write {id}@{mm}"
        ));
    }
    Ok(rows)
}

/// `re.search(rf"@{mm}\b", text)`.
fn has_stamp(text: &str, mm: &str) -> bool {
    let needle = format!("@{mm}");
    text.match_indices(&needle).any(|(i, _)| {
        text[i + needle.len()..]
            .chars()
            .next()
            .is_none_or(|c| !is_word(c))
    })
}

/// `pulse_line`: the strict `Task:` pulse for older consumers, or why there is none.
///
/// # Errors
/// The reason no pulse was derived.
pub fn pulse_line(
    rows: &[(String, String)],
    t_id: &str,
    mm: Option<&str>,
) -> Result<String, String> {
    let Some(mm) = mm else {
        return Err(format!(
            "{t_id} has no creation day, so no strict pulse (an IL id needs @MMDD)"
        ));
    };
    let get = |k: &str| field(rows, k).unwrap_or_default();
    let cut_field = |k: &str, n: usize| strip(&cut(&get(k).replace('|', "/"), n)).to_owned();
    let mut parts = vec![
        format!("Task: {t_id}@{mm}"),
        format!("done: {}", cut_field("done", 200)),
    ];
    for (k, n) in [("est", 60), ("remaining", 200)] {
        if !get(k).is_empty() {
            parts.push(format!("{k}: {}", cut_field(k, n)));
        }
    }
    parts.push(format!("pct: {}", get("pct")));
    let line = parts.join(" | ");
    if pulse::pulse_ok(&line) {
        Ok(line)
    } else {
        Err("derived pulse failed the strict grammar".to_owned())
    }
}

/// `check_handback` then the script's printout: `OK`, the expanded block, the pulse.
///
/// # Errors
/// The `IL-REJECT ...` line.
pub fn handback(text: &str, st: &Json, lib: &Lib, expect: Option<&str>) -> Result<String, String> {
    let rows = check_handback(text, st, lib, expect)?;
    let (t, mm) =
        task(st, field(&rows, "task").unwrap_or_default(), "task").map_err(|e| e.to_string())?;
    let pulse = match pulse_line(&rows, t.s("id"), mm.as_deref()) {
        Ok(line) => format!("pulse: {line}"),
        Err(why) => format!("pulse: none ({why})"),
    };
    let expanded = crate::il::ilio::expand(text, st, lib);
    Ok(format!("OK\n{expanded}\n{pulse}"))
}
