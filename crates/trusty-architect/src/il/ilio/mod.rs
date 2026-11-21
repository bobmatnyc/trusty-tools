//! `ilio`: the IL response model (blocks, the id table, relay text).
//!
//! Why: a session ends each turn with one fenced `il` block; scripts, not the
//! model, validate it and write the ledger. `ilio.py` holds the block grammar,
//! the id table, the R reference library and the verbs on top. This is its port,
//! limited to the ledger side: `check`, `apply`, `collapse`, `table`, `relay`,
//! `parse` and `expand`. The dashboard store, `last-apply.json` and the steering
//! log stay in Python (Architect ruling), which calls this for the ledger part.
//! What: [`parse_block`], [`Lib`] (the R library), [`ledger_ids`], [`table_rows`],
//! [`fit_rows`], [`relay_text`], [`expand`], [`task_id`] and [`parent_goal`].
//! Test: `tests/il_ilio.rs`, plus the unit tests below.

pub mod apply;
pub mod check;
pub mod cli;
pub mod collapse;
pub mod idre;
pub mod newline;
pub mod secret;

use crate::il::json::Json;
use crate::pystr::{is_space, splitlines, strip};
use idre::{id_full, ids_in};

/// Fields of an IL block in the order they must appear.
pub const FIELDS: [&str; 20] = [
    "goal",
    "task",
    "done",
    "pct",
    "est",
    "remaining",
    "open",
    "new",
    "remind",
    "close",
    "next",
    "delta",
    "need",
    "decided",
    "act",
    "evidence",
    "refs",
    "error",
    "feedback",
    "merge",
];
/// Fields every block carries.
pub const REQUIRED: [&str; 4] = ["task", "done", "pct", "next"];

/// The R reference library: `{"R01": {"title": .., "options": {"a": "text with {slot}"}}}`.
pub struct Lib(pub Json);

impl Lib {
    /// An empty library.
    pub fn empty() -> Self {
        Lib(Json::Obj(Vec::new()))
    }

    /// Reads `<dir>/r-library.json`; a missing or unreadable file is an empty library.
    pub fn load(dir: Option<&std::path::Path>) -> Self {
        let json = dir
            .and_then(|d| std::fs::read_to_string(d.join("r-library.json")).ok())
            .and_then(|t| Json::parse(&t).ok())
            .filter(|j| matches!(j, Json::Obj(_)));
        json.map_or_else(Self::empty, Lib)
    }

    fn option(&self, rid: &str, opt: &str) -> Option<&str> {
        self.0.get(rid)?.get("options")?.get(opt)?.as_str()
    }
}

/// `parse_block`: the fields of the LAST ` ```il ` fence as `(field, value)`, `None` without a fence.
pub fn parse_block(text: &str) -> Option<Vec<(String, String)>> {
    let t: Vec<char> = text.chars().collect();
    let lit = |at: usize, s: &str| {
        t.get(at..at + s.chars().count())
            .is_some_and(|w| w.iter().copied().eq(s.chars()))
    };
    let (mut last, mut i) = (None, 0);
    while i < t.len() {
        if lit(i, "```il") {
            let mut j = i + 5;
            while matches!(t.get(j), Some(' ' | '\t')) {
                j += 1;
            }
            if t.get(j) == Some(&'\n')
                && let Some(close) = (j + 1..t.len()).find(|&k| lit(k, "```"))
            {
                last = Some(t[j + 1..close].iter().collect::<String>());
                i = close + 3;
                continue;
            }
        }
        i += 1;
    }
    let body = last?;
    let mut rows = Vec::new();
    for ln in splitlines(&body) {
        if strip(ln).is_empty() {
            continue;
        }
        rows.push(match ln.split_once(':') {
            Some((k, v)) => (strip(k).to_owned(), strip(v).to_owned()),
            None => ("?".to_owned(), strip(ln).to_owned()),
        });
    }
    Some(rows)
}

/// `(id, kind, status, created stamp)` in goals, tasks, items order.
pub type LedgerIds = Vec<(String, char, String, Option<String>)>;

/// `_mmdd`: MMDD of a task's creation date.
pub fn mmdd(t: &Json) -> Option<String> {
    let c: Vec<char> = t
        .get("created")
        .filter(|v| v.truthy())
        .map_or_else(String::new, Json::py_str)
        .chars()
        .collect();
    let date_like = c.len() >= 10
        && c[..4].iter().all(char::is_ascii_digit)
        && c[4] == '-'
        && c[5..7].iter().all(char::is_ascii_digit)
        && c[7] == '-'
        && c[8..10].iter().all(char::is_ascii_digit);
    date_like.then(|| c[5..7].iter().chain(&c[8..10]).collect())
}

/// `ledger_ids`: every goal, task and D/A in the ledger.
pub fn ledger_ids(st: &Json) -> LedgerIds {
    let mut out = LedgerIds::new();
    out.extend(
        st.arr("goals")
            .iter()
            .map(|g| (g.s("id").to_owned(), 'G', g.s("status").to_owned(), None)),
    );
    out.extend(
        st.arr("tasks")
            .iter()
            .map(|t| (t.s("id").to_owned(), 'T', t.s("status").to_owned(), mmdd(t))),
    );
    for i in st.arr("items") {
        let kind = i
            .s("id")
            .split('-')
            .nth(1)
            .and_then(|p| p.chars().next())
            .unwrap_or('?');
        out.push((i.s("id").to_owned(), kind, i.s("status").to_owned(), None));
    }
    out
}

fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `table_rows`: goals by rank, live tasks by id, D then A, owed, then the R library.
pub fn table_rows(st: &Json, lib: &Lib) -> Vec<String> {
    let mut goals: Vec<&Json> = st.arr("goals").iter().collect();
    goals.sort_by_key(|g| g.i("rank").unwrap_or(0));
    let mut rows: Vec<String> = goals
        .iter()
        .map(|g| {
            format!(
                "{} r{} {} {}",
                g.s("id"),
                g.i("rank").unwrap_or(0),
                g.s("status"),
                cut(g.s("title"), 60)
            )
        })
        .collect();
    let mut tasks: Vec<&Json> = st
        .arr("tasks")
        .iter()
        .filter(|t| t.s("status") != "done")
        .collect();
    tasks.sort_by(|a, b| a.s("id").cmp(b.s("id")));
    rows.extend(tasks.iter().map(|t| {
        format!(
            "{}@{} {} {} {}",
            t.s("id"),
            mmdd(t).unwrap_or_else(|| "----".into()),
            t.s("status"),
            t.s("goal"),
            cut(t.s("title"), 60)
        )
    }));
    for pre in ['D', 'A'] {
        let mut items: Vec<&Json> = st
            .arr("items")
            .iter()
            .filter(|i| {
                i.s("id")
                    .split('-')
                    .nth(1)
                    .is_some_and(|p| p.starts_with(pre))
                    && i.s("status") != "done"
            })
            .collect();
        items.sort_by(|a, b| a.s("id").cmp(b.s("id")));
        for i in items {
            let opts: Vec<String> = i
                .arr("opts")
                .iter()
                .map(|o| {
                    format!(
                        "{}{}",
                        if o.get("rec").is_some_and(Json::truthy) {
                            "*"
                        } else {
                            ""
                        },
                        cut(o.s("l"), 40)
                    )
                })
                .collect();
            let mut row = format!("{} {} {}", i.s("id"), i.s("status"), cut(i.s("title"), 60));
            if !opts.is_empty() {
                row.push_str(&format!(" opts {}", opts.join("|")));
            }
            if let Some(a) = i.get("answer").filter(|a| a.truthy()) {
                row.push_str(&format!(" ans {}", cut(&a.py_str(), 60)));
            }
            rows.push(row);
        }
    }
    for o in st.get("session").map_or(&[][..], |s| s.arr("owed")) {
        rows.push(format!(
            "owed {} = {} @ {}",
            o.s("check"),
            o.s("expect"),
            o.s("ref")
        ));
    }
    let mut rids: Vec<&str> = lib.0.entries().iter().map(|(k, _)| k.as_str()).collect();
    rids.sort_unstable();
    for rid in rids {
        let entry = lib.0.get(rid);
        let mut opts: Vec<&(String, Json)> = entry
            .and_then(|e| e.get("options"))
            .map_or(&[][..], |o| o.entries())
            .iter()
            .collect();
        opts.sort_by(|a, b| a.0.cmp(&b.0));
        for (k, v) in opts {
            rows.push(format!(
                "{rid}-{k} {}: {}",
                entry.map_or("", |e| e.s("title")),
                v.py_str()
            ));
        }
    }
    rows
}

/// `fit_rows`: whole rows within `budget` characters; a cut ends with one `... N rows withheld` line.
pub fn fit_rows(rows: &[String], budget: usize) -> Vec<String> {
    let (mut out, mut used) = (Vec::new(), 0);
    for (i, r) in rows.iter().enumerate() {
        let n = r.chars().count();
        if used + n + 1 > budget {
            out.push(format!(
                "... {} rows withheld (raise --max-tokens)",
                rows.len() - i
            ));
            return out;
        }
        out.push(r.clone());
        used += n + 1;
    }
    out
}

fn is_slot_word(c: char) -> bool {
    crate::pystr::is_word(c)
}

/// `relay_text`: the text of an R reference such as `R01-a sha=abc` with `{slot}`s filled.
pub fn relay_text(reference: &str, extra: &[(String, String)], lib: &Lib) -> Option<String> {
    let s = strip(reference);
    let t: Vec<char> = s.chars().collect();
    if t.first() != Some(&'R') || t.len() < 5 || t[3] != '-' || !t[4].is_ascii_lowercase() {
        return None;
    }
    let n: String = t[1..3].iter().collect();
    crate::il::ids::dec(&n)?;
    let mut slots: Vec<(String, String)> = Vec::new();
    let rest: String = t[5..].iter().collect();
    if !rest.is_empty() {
        // `(?: [a-z_]+=\S+)*` to the end: single-space separated, no other characters.
        for part in rest.strip_prefix(' ')?.split(' ') {
            let (k, v) = part.split_once('=')?;
            let ok = !k.is_empty()
                && k.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                && !v.is_empty()
                && !v.chars().any(is_space);
            if !ok {
                return None;
            }
            slots.push((k.to_owned(), v.to_owned()));
        }
    }
    let txt = lib.option(&format!("R{n}"), &t[4].to_string())?;
    // `{**dict(from the ref), **slots}`: the explicit slots win.
    for (k, v) in extra {
        slots.retain(|(sk, _)| sk != k);
        slots.push((k.clone(), v.clone()));
    }
    let chars: Vec<char> = txt.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '{'
            && let Some(len) = chars[i + 1..].iter().position(|c| *c == '}')
            && len > 0
            && chars[i + 1..i + 1 + len].iter().all(|c| is_slot_word(*c))
        {
            let name: String = chars[i + 1..i + 1 + len].iter().collect();
            match slots.iter().rev().find(|(k, _)| *k == name) {
                Some((_, v)) => out.push_str(v),
                None => out.extend(&chars[i..i + len + 2]),
            }
            i += len + 2;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    Some(out)
}

/// `\bR[0-9a-km-np-z]{2}-[a-z](?: [a-z_]+=\S+)*`: the spans of R references (with slots) in `v`.
fn rref_spans(t: &[char]) -> Vec<(usize, usize)> {
    let (mut out, mut i) = (Vec::new(), 0);
    while i + 5 <= t.len() {
        let pair: String = t[i + 1..i + 3].iter().collect();
        let head = t[i] == 'R'
            && (i == 0 || !crate::pystr::is_word(t[i - 1]))
            && crate::il::ids::dec(&pair).is_some()
            && t[i + 3] == '-'
            && t[i + 4].is_ascii_lowercase();
        if !head {
            i += 1;
            continue;
        }
        let mut end = i + 5;
        loop {
            let name = t[end.min(t.len())..]
                .iter()
                .skip(1)
                .take_while(|c| c.is_ascii_lowercase() || **c == '_')
                .count();
            let ok = t.get(end) == Some(&' ')
                && name > 0
                && t.get(end + 1 + name) == Some(&'=')
                && t.get(end + 2 + name).is_some_and(|c| !is_space(*c));
            if !ok {
                break;
            }
            let val = t[end + 2 + name..]
                .iter()
                .take_while(|c| !is_space(**c))
                .count();
            end += 2 + name + val;
        }
        out.push((i, end));
        i = end;
    }
    out
}

/// `expand`: the block with every R reference and id expanded to text, one `field: value` per line.
pub fn expand(text: &str, st: &Json, lib: &Lib) -> String {
    let rows = parse_block(text).unwrap_or_default();
    let mut titles: Vec<(&str, &str)> = Vec::new();
    for bucket in ["goals", "tasks", "items"] {
        titles.extend(st.arr(bucket).iter().map(|o| (o.s("id"), o.s("title"))));
    }
    let mut out = Vec::new();
    for (k, v) in rows {
        let t: Vec<char> = v.chars().collect();
        let mut s = String::new();
        let mut last = 0;
        for (a, b) in rref_spans(&t) {
            s.extend(&t[last..a]);
            let whole: String = t[a..b].iter().collect();
            s.push_str(&relay_text(&whole, &[], lib).unwrap_or(whole));
            last = b;
        }
        s.extend(&t[last..]);
        let t: Vec<char> = s.chars().collect();
        let mut o = String::new();
        let mut last = 0;
        for m in ids_in(&t) {
            o.extend(&t[last..m.start]);
            let matched: String = t[m.start..m.end].iter().collect();
            let full = m.code.as_ref().map(|c| format!("{c}-{}{}", m.pre, m.n));
            match full.and_then(|f| titles.iter().find(|(id, _)| *id == f).map(|(_, ti)| *ti)) {
                Some(title) => o.push_str(&format!("{matched} ({})", cut(title, 50))),
                None => o.push_str(&matched),
            }
            last = m.end;
        }
        o.extend(&t[last..]);
        out.push(format!("{k}: {o}"));
    }
    out.join("\n")
}

fn task_match<'a>(st: &'a Json, tok: &str) -> Option<&'a Json> {
    let t: Vec<char> = tok.chars().collect();
    let m = ids_in(&t).into_iter().next()?;
    if m.pre != 'T' {
        return None;
    }
    let suffix = format!("-T{}", m.n);
    st.arr("tasks").iter().find(|t| {
        t.s("id").ends_with(&suffix)
            && m.code
                .as_ref()
                .is_none_or(|c| t.s("id").starts_with(&format!("{c}-")))
    })
}

/// `task_id`: the full ledger id of the block's task (stamp dropped); `None` when it is not live.
pub fn task_id(st: &Json, tok: &str) -> Option<String> {
    task_match(st, tok).map(|t| t.s("id").to_owned())
}

/// The id of the ledger's active ad-hoc goal, else `ar-G01`.
fn adhoc_or_default(st: &Json) -> String {
    st.arr("goals")
        .iter()
        .find(|g| g.s("status") == "active" && g.s("title").starts_with("Handle ad-hoc tasks"))
        .map_or_else(|| "ar-G01".to_owned(), |g| g.s("id").to_owned())
}

/// `parent_goal`: the goal of the block's task; the ad-hoc default goal when it cannot be resolved.
pub fn parent_goal(st: &Json, tok: &str) -> String {
    match task_match(st, tok) {
        Some(t) => {
            let first = strip(t.s("goal").split(',').next().unwrap_or("")).to_owned();
            if first.is_empty() {
                adhoc_or_default(st)
            } else {
                first
            }
        }
        None => adhoc_or_default(st),
    }
}

/// Whether `tok` is a whole id with a code, for `close:`.
pub fn full_id_of(tok: &str) -> Option<String> {
    let m = id_full(tok)?;
    m.code.map(|c| format!("{c}-{}{}", m.pre, m.n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_block_takes_the_last_fence() {
        let text = "x\n```il\ntask: a\n```\nmid\n```il  \nTask: ar-T01\n\nnote without colon\ndone: x: y\n```\n";
        let rows = parse_block(text).unwrap();
        assert_eq!(
            rows,
            [
                ("Task".into(), "ar-T01".into()),
                ("?".into(), "note without colon".into()),
                ("done".into(), "x: y".into())
            ]
        );
        assert!(parse_block("```il\nunclosed").is_none());
        assert!(parse_block("no fence").is_none());
        assert!(parse_block("```ilx\na: b\n```").is_none());
    }

    #[test]
    fn relay_text_fills_slots() {
        let lib = Lib(Json::parse(
            r#"{"R01":{"title":"T","options":{"a":"merge {sha} on {where} {missing}"}}}"#,
        )
        .unwrap());
        assert_eq!(
            relay_text("R01-a sha=abc where=main", &[], &lib).as_deref(),
            Some("merge abc on main {missing}")
        );
        assert_eq!(
            relay_text("R01-a sha=abc", &[("sha".into(), "zzz".into())], &lib).as_deref(),
            Some("merge zzz on {where} {missing}")
        );
        assert!(relay_text("R01-b", &[], &lib).is_none());
        assert!(relay_text("R02-a", &[], &lib).is_none());
        assert!(relay_text("R01-a bad", &[], &lib).is_none());
        assert!(relay_text("R01-ax", &[], &lib).is_none());
    }

    #[test]
    fn fit_rows_stops_at_a_whole_row() {
        let rows: Vec<String> = ["aaaa", "bbbb", "cccc"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        assert_eq!(fit_rows(&rows, 100), rows);
        assert_eq!(
            fit_rows(&rows, 10),
            ["aaaa", "bbbb", "... 1 rows withheld (raise --max-tokens)"]
        );
        assert_eq!(
            fit_rows(&rows, 4)[0],
            "... 3 rows withheld (raise --max-tokens)"
        );
    }

    #[test]
    fn task_and_goal_resolution() {
        let st = Json::parse(r#"{"goals":[{"id":"ar-G01","status":"active","title":"Handle ad-hoc tasks (standing)"}],"tasks":[{"id":"ar-T0f","goal":"ar-G02, ar-G03","status":"open"}]}"#).unwrap();
        assert_eq!(task_id(&st, "ar-T0f@1120").as_deref(), Some("ar-T0f"));
        assert_eq!(task_id(&st, "T0f").as_deref(), Some("ar-T0f"));
        assert_eq!(task_id(&st, "xx-T0f"), None);
        assert_eq!(parent_goal(&st, "ar-T0f"), "ar-G02");
        assert_eq!(parent_goal(&st, "free: nothing"), "ar-G01");
    }
}
