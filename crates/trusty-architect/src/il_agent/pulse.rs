//! `task_tag.parse_pulse` and `shlex.quote`, as `il_agent.py` uses them (#12028 P1f).
//!
//! Why: `il_agent.py handback` derives a strict `Task:` pulse line and keeps it
//! only when `task_tag.parse_pulse` accepts it; `dispatch` prints a shell line
//! built with `shlex.quote`. The crate has no regex engine, so both are hand
//! matchers.
//! What: [`pulse_ok`] (`parse_pulse(text)[0] == "ok"`: the last non-empty line
//! matches `STRICT_RE`) and [`shell_quote`].
//! Test: `pulse_ok_follows_the_strict_grammar`, `shell_quote_matches_shlex`.

use crate::pystr::{is_digit, rstrip, splitlines, strip};

/// `[0-9a-km-np-z]`: a base-34 id digit.
fn id_digit(c: char) -> bool {
    c.is_ascii_digit() || (c.is_ascii_lowercase() && c != 'l' && c != 'o')
}

/// `[a-z][a-z0-9]{1,7}-T[0-9a-km-np-z]{2}@\d{4}` at the start of `t`: its length.
fn il_id(t: &[char]) -> Option<usize> {
    let code = t
        .iter()
        .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        .count();
    let ok = t.first().is_some_and(char::is_ascii_lowercase)
        && (2..=8).contains(&code)
        && t.get(code) == Some(&'-')
        && t.get(code + 1) == Some(&'T')
        && t.get(code + 2..code + 4)
            .is_some_and(|p| p.iter().all(|c| id_digit(*c)))
        && t.get(code + 4) == Some(&'@')
        && t.get(code + 5..code + 9)
            .is_some_and(|p| p.iter().all(|c| is_digit(*c)));
    ok.then_some(code + 9)
}

const FIELDS: [(&str, usize); 3] = [("done", 200), ("est", 60), ("remaining", 200)];

/// The optional ` | key: value` groups of `STRICT_RE` from group `from`, then `$`.
fn tail(t: &[char], from: usize) -> bool {
    if t.is_empty() {
        return true;
    }
    let starts = |s: &str| {
        let p: Vec<char> = s.chars().collect();
        t.starts_with(&p).then_some(p.len())
    };
    for (i, (name, max)) in FIELDS.iter().enumerate().skip(from) {
        let Some(at) = starts(&format!(" | {name}: ")) else {
            continue;
        };
        let run = t[at..]
            .iter()
            .take_while(|c| **c != '|' && **c != '\n')
            .count();
        if (1..=run.min(*max)).any(|n| tail(&t[at + n..], i + 1)) {
            return true;
        }
    }
    let Some(at) = starts(" | pct: ") else {
        return false;
    };
    let digits = t[at..].iter().take_while(|c| is_digit(**c)).count();
    let rest = &t[at + digits.min(3)..];
    (1..=3).contains(&digits) && (rest.is_empty() || rest == ['%'])
}

/// `STRICT_RE.match(line)`.
fn strict(line: &str) -> bool {
    let t: Vec<char> = line.chars().collect();
    let Some(body) = t.strip_prefix(&['T', 'a', 's', 'k', ':', ' ']) else {
        return false;
    };
    if let Some(n) = il_id(body)
        && tail(&body[n..], 0)
    {
        return true;
    }
    // `T[0-9a-km-np-z]{2,4}`, greedy, then backtracking.
    let run = body.iter().skip(1).take_while(|c| id_digit(**c)).count();
    body.first() == Some(&'T') && (2..=run.min(4)).rev().any(|n| tail(&body[1 + n..], 0))
}

/// `task_tag.parse_pulse(text)[0] == "ok"`.
pub fn pulse_ok(text: &str) -> bool {
    let lines: Vec<&str> = splitlines(text)
        .into_iter()
        .filter(|l| !strip(l).is_empty())
        .map(rstrip)
        .collect();
    lines
        .last()
        .is_some_and(|last| last.starts_with("Task:") && strict(last))
}

/// `shlex.quote(s)`.
pub fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_owned();
    }
    let safe = s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if safe {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\"'\"'"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulse_ok_follows_the_strict_grammar() {
        assert!(pulse_ok("Task: qx-T01@1115 | done: x | pct: 40"));
        assert!(pulse_ok(
            "Task: qx-T01@1115 | done: a b | est: 1h | remaining: r | pct: 100%"
        ));
        assert!(pulse_ok("Task: T93"));
        assert!(!pulse_ok("Task: qx-T01 | done: x | pct: 4"));
        assert!(!pulse_ok("Task: qx-T01@1115 | done:  | pct: 4"));
        assert!(!pulse_ok("Task: qx-T01@1115 | pct: 1000"));
        assert!(!pulse_ok(&format!(
            "Task: qx-T01@1115 | done: {} | pct: 4",
            "x".repeat(201)
        )));
        assert!(!pulse_ok("Task: qx-Tl1@1115 | pct: 4"));
        assert!(!pulse_ok(""));
    }

    #[test]
    fn shell_quote_matches_shlex() {
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("qx-T01@1115"), "qx-T01@1115");
        assert_eq!(shell_quote("R02-a sha=abc"), "'R02-a sha=abc'");
        assert_eq!(shell_quote("it's"), "'it'\"'\"'s'");
    }
}
