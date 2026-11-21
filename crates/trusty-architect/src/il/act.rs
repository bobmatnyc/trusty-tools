//! Actions: the `act` record, and the `acts` / digest reader.
//!
//! Why: Bob 2026-11-17 moved the action list into the ledger. An action is one
//! log line, not a state change, so it never bumps `v`; the digest shows the last
//! twenty and `acts` lists them across the rotated files.
//! What: [`act_record`] validates the fields and builds the `{ts, kind: act, ...}`
//! line; [`parse_dur`] reads `30m`/`48h`/`7d`; [`read_acts`] returns act records
//! oldest first, honouring `--since`, `--goal` and `--limit`; [`fmt_act`] renders
//! one for `acts` and the digest.
//! Test: `act_record_validates_limits_and_orders_keys`, `parse_dur_forms`,
//! `read_acts_filters_and_limits`, `fmt_act_matches_python`.

use super::error::{IlResult, invalid};
use super::json::{Json, obj};
use super::limits::{lim, short};
use super::log::{Rec, log_files_newest_first, log_rec, read_tail};
use super::store::Paths;
use super::time::{fmt_day, fmt_min};
use crate::pystr::splitlines;

/// `ACT_LIMITS`: field, maximum characters, required.
pub const ACT_LIMITS: [(&str, usize, bool); 6] = [
    ("who", 60, true),
    ("what", 240, true),
    ("owner_words", 400, false),
    ("evidence", 240, false),
    ("goal", 24, false),
    ("refs", 240, false),
];

/// Characters of an act line in the digest, and how many lines.
pub const ACT_DIGEST_W: usize = 140;
/// Acts listed in the digest.
pub const ACT_DIGEST_N: usize = 20;

/// `apply_act`: validates an `act` op and returns the log record.
///
/// # Errors
/// `Invalid` naming the field.
pub fn act_record(a: &Json, now: i64, env_session: &str) -> IlResult<Json> {
    let mut rec = obj([
        ("ts", fmt_min(now).into()),
        ("kind", "act".into()),
        ("session", env_session.into()),
    ]);
    for (k, mx, req) in ACT_LIMITS {
        rec.set(k, lim(a.get(k), &format!("act {k}"), mx, req, false)?);
    }
    Ok(rec)
}

/// `parse_dur`: `90m`, `48h`, `7d` (a bare number is hours) in seconds.
///
/// # Errors
/// `Invalid` with `datg.py`'s message.
pub fn parse_dur(s: &str) -> IlResult<f64> {
    let t = crate::pystr::strip(s);
    let (num, mult) = match t.chars().last() {
        Some('m') => (&t[..t.len() - 1], 60.0),
        Some('h') => (&t[..t.len() - 1], 3600.0),
        Some('d') => (&t[..t.len() - 1], 86_400.0),
        _ => (t, 3600.0),
    };
    let ok = !num.is_empty()
        && num.split('.').count() <= 2
        && num
            .split('.')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    match num.parse::<f64>() {
        Ok(v) if ok => Ok(v * mult),
        _ => invalid(format!(
            "--since must look like 30m, 48h or 7d, got {}",
            crate::pystr::py_repr(s)
        )),
    }
}

/// `read_acts`: act records, oldest first. `since_s` keeps the last N seconds, `limit` the newest N.
pub fn read_acts(
    paths: &Paths,
    since_s: Option<f64>,
    goal: Option<&str>,
    limit: Option<usize>,
    now: f64,
) -> Vec<Json> {
    let cutoff = since_s.map(|s| now - s);
    let cut_day = cutoff.map(|c| fmt_day(c.floor() as i64));
    let mut out: Vec<Json> = Vec::new();
    for (path, day) in log_files_newest_first(paths) {
        if let (Some(d), Some(c)) = (&day, &cut_day)
            && d < c
        {
            break; // a rotated file's day is >= every line's day: the rest hold only older lines
        }
        let text = read_tail(&path, None);
        let mut got: Vec<Json> = Vec::new();
        for ln in splitlines(&text)
            .into_iter()
            .filter(|l| l.contains("\"act\""))
        {
            let Some(Rec { t, kind, rec }) = log_rec(ln) else {
                continue;
            };
            if kind != "act" {
                continue;
            }
            if let Some(c) = cutoff
                && t.is_none_or(|t| (t as f64) < c)
            {
                continue;
            }
            if let Some(g) = goal.filter(|g| !g.is_empty())
                && rec.get("goal").and_then(Json::as_str) != Some(g)
            {
                continue;
            }
            got.push(rec);
        }
        got.append(&mut out);
        out = got;
        if limit.is_some_and(|l| l > 0 && out.len() >= l) {
            break; // older rotation days cannot hold lines newer than these
        }
    }
    out.sort_by(|a, b| a.s("ts").cmp(b.s("ts")));
    match limit.filter(|l| *l > 0) {
        Some(l) if out.len() > l => out.split_off(out.len() - l),
        _ => out,
    }
}

/// `fmt_act`: `ts who: what | owner: "..." | ev: ... | goal: ... | refs: ...`, cut to `width` when given.
pub fn fmt_act(r: &Json, width: Option<usize>) -> String {
    let mut parts = vec![format!("{} {}: {}", r.s("ts"), r.s("who"), r.s("what"))];
    for (k, lab) in [
        ("owner_words", "owner"),
        ("evidence", "ev"),
        ("goal", "goal"),
        ("refs", "refs"),
    ] {
        let v = r.s(k);
        if !v.is_empty() {
            parts.push(if k == "owner_words" {
                format!("{lab}: \"{v}\"")
            } else {
                format!("{lab}: {v}")
            });
        }
    }
    let line = parts.join(" | ");
    match width {
        Some(w) => short(&line, w),
        None => line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn line(ts: &str, goal: &str, what: &str) -> String {
        format!(
            "{{\"ts\":\"{ts}\",\"kind\":\"act\",\"session\":\"\",\"who\":\"w\",\"what\":\"{what}\",\"goal\":\"{goal}\"}}\n"
        )
    }

    #[test]
    fn act_record_validates_limits_and_orders_keys() {
        let a = obj([
            ("who", "me".into()),
            ("what", " did it ".into()),
            ("refs", "PR #1".into()),
        ]);
        let r = act_record(&a, 0, "").unwrap();
        assert_eq!(
            r.dump(),
            r##"{"ts":"1970-01-01T00:00Z","kind":"act","session":"","who":"me","what":"did it","owner_words":"","evidence":"","goal":"","refs":"PR #1"}"##
        );
        let bad = obj([("who", "me".into())]);
        assert_eq!(
            act_record(&bad, 0, "").unwrap_err().to_string(),
            "act what must be one line, <= 240 chars, non-empty"
        );
        let long = obj([("who", "m".into()), ("what", "x".repeat(241).into())]);
        assert!(act_record(&long, 0, "").is_err());
    }

    #[test]
    fn parse_dur_forms() {
        assert_eq!(parse_dur("30m").unwrap(), 1800.0);
        assert_eq!(parse_dur("48h").unwrap(), 172_800.0);
        assert_eq!(parse_dur("7d").unwrap(), 604_800.0);
        assert_eq!(parse_dur("2").unwrap(), 7200.0);
        assert_eq!(parse_dur("1.5h").unwrap(), 5400.0);
        assert_eq!(
            parse_dur("x").unwrap_err().to_string(),
            "--since must look like 30m, 48h or 7d, got 'x'"
        );
        assert!(parse_dur("").is_err());
        assert!(parse_dur("1.h").is_err());
    }

    #[test]
    fn read_acts_filters_and_limits() {
        let t = tempfile::tempdir().unwrap();
        let p = Paths::at(t.path().to_path_buf());
        fs::write(
            t.path().join("log-20261101.jsonl"),
            line("2026-11-01T01:00Z", "g", "old"),
        )
        .unwrap();
        let body = [
            line("2026-11-10T01:00Z", "g", "a"),
            line("2026-11-10T02:00Z", "h", "b"),
            "{\"ts\":\"2026-11-10T03:00Z\",\"kind\":\"digest\",\"x\":\"act\"}\n".to_owned(),
            line("2026-11-10T04:00Z", "g", "c"),
        ]
        .concat();
        fs::write(&p.log, body).unwrap();
        let now = super::super::time::parse_ts("2026-11-10T05:00Z").unwrap() as f64;
        let all = read_acts(&p, None, None, None, now);
        assert_eq!(
            all.iter().map(|r| r.s("what")).collect::<Vec<_>>(),
            ["old", "a", "b", "c"]
        );
        let g = read_acts(&p, None, Some("g"), Some(2), now);
        assert_eq!(
            g.iter().map(|r| r.s("what")).collect::<Vec<_>>(),
            ["a", "c"]
        );
        let recent = read_acts(&p, Some(3.0 * 3600.0), None, None, now);
        assert_eq!(
            recent.iter().map(|r| r.s("what")).collect::<Vec<_>>(),
            ["b", "c"]
        );
    }

    #[test]
    fn fmt_act_matches_python() {
        let r = Json::parse(r#"{"ts":"T","who":"w","what":"x","owner_words":"q","evidence":"e","goal":"","refs":"r"}"#).unwrap();
        assert_eq!(fmt_act(&r, None), "T w: x | owner: \"q\" | ev: e | refs: r");
        assert_eq!(fmt_act(&r, Some(8)), "T w: x …");
    }
}
