//! The IL packet: the fenced ```` ```il ```` block a turn ends with (G0b).
//!
//! Why: Bob: "all ledger updates should happen automatically on receiving an
//! IL structured response with an IL packet". The harness must read and
//! check the block itself, in process, with the rules `ilio.py` applies
//! before it writes through `datg.py`.
//! What: [`parse_block`] (ilio `parse_block`: the LAST fence, one
//! `field: value` row per non-blank line, a line with no colon is field `?`),
//! [`check`] (ilio `check`: required fields, order, known fields, line caps,
//! secret shapes, `pct`, ids in the ledger, `new:` kind, `act:` parts and
//! the datg `ACT_LIMITS`) and [`packet_id`] (the idempotency key). [`apply`]
//! turns a checked packet into one writer batch plus the `act` log lines.
//! Test: `tests/il_packet.rs`.

mod apply;
mod route;
mod secret;

pub use apply::{Applied, IlError, apply};
pub use route::{
    APPLIED_FILE, APPLY_LOCK, PENDING_DIR, Route, clear_pending, hold, ilio_fields, mark_applied,
    mark_pending, pending_unknown, route,
};

use crate::ledger::model::State;
use crate::secret::SecretScan;

/// ilio `FIELDS`, in the order a block must keep.
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
/// ilio `REQUIRED`.
pub const REQUIRED: [&str; 4] = ["task", "done", "pct", "next"];
/// ilio `LINE_MAX`: one `field: value` line, in characters.
pub const LINE_MAX: usize = 240;
/// ilio `NEW_LINE_MAX`: a `new:` line.
pub const NEW_LINE_MAX: usize = 1200;
/// datg `ACT_LIMITS`: (key, max chars, required).
pub const ACT_LIMITS: [(&str, usize, bool); 6] = [
    ("who", 60, true),
    ("what", 240, true),
    ("owner_words", 400, false),
    ("evidence", 240, false),
    ("goal", 24, false),
    ("refs", 240, false),
];

/// One `field: value` row.
pub type Row = (String, String);

/// A refused block: `IL-REJECT <code> <field>: <expected>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reject {
    /// The rule broken (`NO-BLOCK`, `LONG`, `FIELD`, `ACT`, ...).
    pub code: &'static str,
    /// The field it broke on.
    pub field: String,
    /// What was expected.
    pub expected: String,
}

impl std::fmt::Display for Reject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = format!("IL-REJECT {} {}: {}", self.code, self.field, self.expected);
        f.write_str(&text.chars().take(200).collect::<String>())
    }
}

fn reject(code: &'static str, field: &str, expected: impl Into<String>) -> Reject {
    Reject {
        code,
        field: field.to_owned(),
        expected: expected.into(),
    }
}

/// The value of field `k` (ilio `dict(rows)`: the last row of a field wins).
pub(crate) fn get<'a>(rows: &'a [Row], k: &str) -> &'a str {
    rows.iter()
        .rev()
        .find(|(f, _)| f == k)
        .map_or("", |(_, v)| v.as_str())
}

/// The body of the LAST ```` ```il ```` fence in `text` (ilio `FENCE`,
/// ```` ```il[ \t]*\n(.*?)``` ```` with `re.S`), or `None`.
pub fn fence_body(text: &str) -> Option<&str> {
    let (mut pos, mut last) = (0, None);
    while let Some(at) = text[pos..].find("```il").map(|i| pos + i) {
        let after = &text[at + 5..];
        let pad = after.len() - after.trim_start_matches([' ', '\t']).len();
        if !after[pad..].starts_with('\n') {
            pos = at + 1;
            continue;
        }
        let start = at + 5 + pad + 1;
        let Some(len) = text[start..].find("```") else {
            break;
        };
        last = Some(&text[start..start + len]);
        pos = start + len + 3;
    }
    last
}

/// Python `str.splitlines` boundaries.
fn line_break(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r'
            | '\x0b'
            | '\x0c'
            | '\x1c'
            | '\x1d'
            | '\x1e'
            | '\u{85}'
            | '\u{2028}'
            | '\u{2029}'
    )
}

/// ilio `parse_block`: the rows of the last fence; `None` with no fence.
pub fn parse_block(text: &str) -> Option<Vec<Row>> {
    let body = fence_body(text)?;
    let rows = body
        .split(line_break)
        .filter(|l| !l.trim().is_empty())
        .map(|l| match l.split_once(':') {
            Some((k, v)) => (k.trim().to_owned(), v.trim().to_owned()),
            None => ("?".to_owned(), l.trim().to_owned()),
        })
        .collect();
    Some(rows)
}

/// The packet's idempotency key: `il-` and 32 hex digits of the SHA-256 of
/// the fence body, so a replayed block maps to the same batch `request_id`.
pub fn packet_id(text: &str) -> Option<String> {
    let body = fence_body(text)?;
    Some(format!(
        "il-{}",
        &crate::sha256::sha256_hex(body.as_bytes())[..32]
    ))
}

/// ilio `check` (and the `act_op`/`apply_act` limits datg raises on), run
/// before anything is written. `None` when the block may be applied.
pub fn check(text: &str, st: &State, scan: SecretScan<'_>) -> Result<Vec<Row>, Reject> {
    let rows = parse_block(text)
        .ok_or_else(|| reject("NO-BLOCK", "il", "end the turn with one ```il fenced block"))?;
    let names: Vec<&str> = rows.iter().map(|(k, _)| k.as_str()).collect();
    if let Some(want) = REQUIRED.iter().find(|w| !names.contains(w)) {
        let all = REQUIRED.join(",");
        return Err(reject("MISSING", want, format!("required fields {all}")));
    }
    let order: Vec<usize> = names
        .iter()
        .filter_map(|n| FIELDS.iter().position(|f| f == n))
        .collect();
    if order.windows(2).any(|w| w[0] > w[1]) {
        let first = FIELDS[..10].join(",");
        return Err(reject(
            "ORDER",
            names[0],
            format!("fields in the order {first}..."),
        ));
    }
    for (k, v) in &rows {
        check_row(k, v, st, scan)?;
    }
    // G0b gap ruling: ilio's apply-time rule, so no path writes past it.
    if let Some(e) = route::pct_unchanged(&rows, st) {
        return Err(reject("PCT-UNCHANGED", "pct", e));
    }
    Ok(rows)
}

fn check_row(k: &str, v: &str, st: &State, scan: SecretScan<'_>) -> Result<(), Reject> {
    if !FIELDS.contains(&k) {
        let all = FIELDS.join(",");
        return Err(reject("FIELD", k, format!("unknown field; allowed {all}")));
    }
    let cap = if k == "new" { NEW_LINE_MAX } else { LINE_MAX };
    if k.chars().count() + 2 + v.chars().count() > cap {
        return Err(reject("LONG", k, format!("line over {cap} characters")));
    }
    // ilio `SECRET` as well as the injected scan (G0b critic).
    if scan(v).is_some() || secret::ilio_secret(v) {
        return Err(reject(
            "SECRET",
            k,
            "a secret-shaped value; record its location only",
        ));
    }
    if k == "pct"
        && !v
            .parse::<u8>()
            .is_ok_and(|n| n <= 100 && !v.starts_with('+'))
    {
        return Err(reject("PCT", "pct", "an integer 0-100"));
    }
    if k == "remind" {
        // G0b gap ruling: ilio `remind_problem`; `at`/`every` parse in ilio.
        return match route::remind_problem(v, st) {
            Some(e) => Err(reject("REMIND", "remind", e)),
            None => Ok(()),
        };
    }
    if matches!(k, "task" | "open" | "close") && !v.starts_with("free:") {
        if let Some(tok) = route::badid(v) {
            let why = format!("{tok} is not base 34 (no o, no l; did you mean 0 or 1)");
            return Err(reject("BADID", k, why));
        }
        for tok in v.split([' ', ',']).filter(|t| !t.is_empty()) {
            let bare = tok.split_once('@').map_or(tok, |(id, _)| id);
            if id_like(bare) && !in_ledger(st, bare) {
                return Err(reject("STALE-ID", k, format!("{tok} is not in the ledger")));
            }
        }
    }
    if k == "new" {
        let kind_ok = matches!(v.as_bytes(), [b'D' | b'A' | b'T', b' ', ..]);
        if !kind_ok {
            return Err(reject("KIND", "new", "K \"title\" with K one of D, A, T"));
        }
    }
    if k == "act" {
        act_parts(v).map_err(|e| reject("ACT", "act", e))?;
    }
    Ok(())
}

/// `[code-]<G|D|A|T><2 digits>`: a token the block uses as a ledger id.
fn id_like(tok: &str) -> bool {
    let rest = match tok.split_once('-') {
        Some((code, rest)) if crate::ledger::ids::Code::parse(code).is_ok() => rest,
        Some(_) => return false,
        None => tok,
    };
    let b = rest.as_bytes();
    b.len() == 3
        && matches!(b[0], b'G' | b'D' | b'A' | b'T')
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase())
}

/// Every goal, item and task id in `st`.
pub(crate) fn ledger_ids(st: &State) -> impl Iterator<Item = &str> {
    let goals = st.goals.iter().map(|g| g.id.as_str());
    let items = st.items.iter().map(|i| i.id.as_str());
    goals
        .chain(items)
        .chain(st.tasks.iter().map(|t| t.id.as_str()))
}

fn in_ledger(st: &State, tok: &str) -> bool {
    if tok.contains('-') {
        ledger_ids(st).any(|id| id == tok)
    } else {
        ledger_ids(st).any(|id| id.ends_with(&format!("-{tok}")))
    }
}

/// ilio `act_op` parts: `<what> [| owner: W] [| ev: E] [| goal: G] [| refs: R]`,
/// as `(what, [(key, value)])`.
pub(crate) fn act_parts(raw: &str) -> Result<(String, Vec<(String, String)>), String> {
    let mut parts = raw.split(" | ").map(str::trim);
    let what = parts.next().unwrap_or("").to_owned();
    let mut kv = Vec::new();
    for x in parts {
        match x.split_once(':') {
            Some((k, v)) if matches!(k.trim(), "owner" | "ev" | "goal" | "refs") => {
                kv.push((k.trim().to_owned(), v.trim().trim_matches('"').to_owned()));
            }
            _ => {
                let head: String = x.chars().take(40).collect();
                return Err(format!(
                    "unknown part {head:?}; use | owner: | ev: | goal: | refs:"
                ));
            }
        }
    }
    let what_len = what.chars().count();
    if what.is_empty() || what_len > ACT_LIMITS[1].1 {
        return Err(format!(
            "act what must be one line, <= {} chars, non-empty",
            ACT_LIMITS[1].1
        ));
    }
    Ok((what, kv))
}
