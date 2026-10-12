//! Hand-written equivalents of `fleet-poll.py`'s module-level regexes.
//!
//! Why: the crate takes no `regex` dependency (#9171 leaf rule), and two of the
//! patterns use look-around the `regex` crate lacks anyway. Each function here
//! names the Python pattern it replaces and returns what `re.match`,
//! `re.search` or `re.sub` returns for it on one screen row.
//! What: pure predicates and rewriters over `&str`, built on [`crate::pystr`].
//! Test: `ansi_strip_matches_python_alternatives`, `box_top_rule_accepts_a_short_label`,
//! `timers_sub_removes_each_shape` (in `classify/tests.rs`).

use crate::pystr::{contains_ci, is_digit, is_space, is_word, lstrip, rstrip, strip};

const ESC: char = '\u{1b}';
const RULE_CHARS: [char; 2] = ['─', '━'];

fn is_rule_char(c: char) -> bool {
    RULE_CHARS.contains(&c)
}

/// Length of one ANSI escape starting at `s[0] == ESC`, or `None`.
///
/// `ANSI = \x1b\[[0-9;?]*[ -/]*[@-~] | \x1b\][^\x07\x1b]*(?:\x07|\x1b\\) | \x1b[()][0-9A-Za-z]`
pub(crate) fn ansi_len(s: &[char]) -> Option<usize> {
    match s.get(1)? {
        '[' => {
            let mut i = 2;
            while s
                .get(i)
                .is_some_and(|c| c.is_ascii_digit() || *c == ';' || *c == '?')
            {
                i += 1;
            }
            while s.get(i).is_some_and(|c| (' '..='/').contains(c)) {
                i += 1;
            }
            s.get(i).filter(|c| ('@'..='~').contains(*c)).map(|_| i + 1)
        }
        ']' => {
            let mut i = 2;
            while s.get(i).is_some_and(|c| *c != '\u{7}' && *c != ESC) {
                i += 1;
            }
            match (s.get(i), s.get(i + 1)) {
                (Some('\u{7}'), _) => Some(i + 1),
                (Some(&ESC), Some('\\')) => Some(i + 2),
                _ => None,
            }
        }
        '(' | ')' => s.get(2).filter(|c| c.is_ascii_alphanumeric()).map(|_| 3),
        _ => None,
    }
}

/// `ANSI.sub("", line)`: every escape sequence removed.
pub(crate) fn strip_ansi(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ESC
            && let Some(n) = ansi_len(&chars[i..])
        {
            i += n;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// `SGR.sub("", text)` with `SGR = \x1b\[[0-9;]*m` (input-state.py).
pub(crate) fn strip_sgr(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ESC && chars.get(i + 1) == Some(&'[') {
            let mut j = i + 2;
            while chars
                .get(j)
                .is_some_and(|c| c.is_ascii_digit() || *c == ';')
            {
                j += 1;
            }
            if chars.get(j) == Some(&'m') {
                i = j + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// `RULE.match(line)`: `^\s*[─━]{10,}\s*$`.
pub(crate) fn is_rule(line: &str) -> bool {
    let body = strip(line);
    body.chars().all(is_rule_char) && body.chars().count() >= 10
}

/// `BOX_TOP_RULE.match(line)`: `^\s*[─━]{10,}(?:\s.{1,30}[─━]+)?\s*$` — a
/// rule, optionally followed by a short label and a closing rule run.
pub(crate) fn is_box_top_rule(line: &str) -> bool {
    let t: Vec<char> = lstrip(line).chars().collect();
    let run = t.iter().take_while(|c| is_rule_char(**c)).count();
    if run < 10 {
        return false;
    }
    let rest = &t[run..];
    if rest.iter().all(|c| is_space(*c)) {
        return true;
    }
    if !is_space(rest[0]) {
        return false;
    }
    // After the one whitespace: 1..=30 of anything, a rule run, then spaces.
    let label: Vec<char> = rest[1..].to_vec();
    let end = label.len() - label.iter().rev().take_while(|c| is_space(**c)).count();
    let tail_rule = label[..end]
        .iter()
        .rev()
        .take_while(|c| is_rule_char(**c))
        .count();
    if tail_rule == 0 || label[..end].contains(&'\n') {
        return false;
    }
    // The rule part takes j chars (1..=tail_rule); the label part keeps end - j.
    (1..=tail_rule).any(|j| (1..=30).contains(&(end - j)))
}

/// `OPTION.match(line)`: `^\s*❯\s*\d+\.` — a dialog cursor on a numbered option.
pub(crate) fn is_option(line: &str) -> bool {
    let Some(rest) = lstrip(line).strip_prefix('❯') else {
        return false;
    };
    let rest = lstrip(rest);
    let digits = rest.chars().take_while(|c| is_digit(*c)).count();
    digits > 0 && rest.chars().nth(digits) == Some('.')
}

/// `SPINNER.match(line)`: `^[·✢✳✶✻✽*∗] `.
pub(crate) fn is_spinner(line: &str) -> bool {
    let mut it = line.chars();
    matches!(
        it.next(),
        Some('·' | '✢' | '✳' | '✶' | '✻' | '✽' | '*' | '∗')
    ) && it.next() == Some(' ')
}

/// `ACTIVE.search(status)`: `…|esc to interrupt` — a turn still running.
pub(crate) fn is_active(status: &str) -> bool {
    status.contains('…') || status.contains("esc to interrupt")
}

/// `WAITING.search(status)`: `Waiting for \d+ background agent`.
pub(crate) fn has_waiting(status: &str) -> bool {
    status.match_indices("Waiting for ").any(|(i, m)| {
        let rest = &status[i + m.len()..];
        let digits = rest.chars().take_while(|c| is_digit(*c)).count();
        digits > 0 && rest[digits..].starts_with(" background agent")
    })
}

/// `AGENT_ROW.match(line)`: `^\s*◯ \S.*\b\d+(?:m \d+)?s\b` — a running
/// background agent row, which always carries an elapsed timer.
// #9171: fleet-poll.py requires the timer; fleet-classify.py took any `◯ ` row.
pub(crate) fn is_agent_row(line: &str) -> bool {
    let Some(rest) = lstrip(line).strip_prefix("◯ ") else {
        return false;
    };
    let s: Vec<char> = rest.chars().collect();
    if s.first().is_none_or(|c| is_space(*c)) {
        return false;
    }
    (1..s.len()).any(|p| boundary(&s, p) && agent_timer_at(&s, p))
}

/// `\d+(?:m \d+)?s\b` at `p`.
fn agent_timer_at(s: &[char], p: usize) -> bool {
    let after = p + digits_at(s, p);
    if after == p {
        return false;
    }
    let at = |k: usize, c: char| s.get(k) == Some(&c);
    if at(after, 'm') && at(after + 1, ' ') {
        let d2 = digits_at(s, after + 2);
        if d2 > 0 && at(after + 2 + d2, 's') && boundary(s, after + 3 + d2) {
            return true;
        }
    }
    at(after, 's') && boundary(s, after + 1)
}

/// `COMPLETED.search(line)`: `^[·✢✳✶✻✽*∗] .*\bfor \d+\s*[smh]\b|· done ` —
/// a finished turn's summary (`✻ Brewed for 42s · done 1:13 AM`).
pub(crate) fn is_completed(line: &str) -> bool {
    if line.contains("· done ") {
        return true;
    }
    if !is_spinner(line) {
        return false;
    }
    let s: Vec<char> = line.chars().collect();
    let prefix: Vec<char> = "for ".chars().collect();
    (2..s.len()).any(|p| {
        if !boundary(&s, p) || !s[p..].starts_with(&prefix) {
            return false;
        }
        let d = digits_at(&s, p + 4);
        let k = p + 4 + d + s[p + 4 + d..].iter().take_while(|c| is_space(**c)).count();
        d > 0 && matches!(s.get(k), Some('s' | 'm' | 'h')) && boundary(&s, k + 1)
    })
}

/// `ACCOUNT.search(text)[1]`: `\|\s*✻(\S+)\s+\|` — the status line's
/// `✻<account>` field. The closing `|` is required, so a truncated line never
/// matches. `\s` spans rows, as it does in the Python over the joined footer.
pub(crate) fn account_field(text: &str) -> Option<String> {
    let s: Vec<char> = text.chars().collect();
    (0..s.len()).filter(|&i| s[i] == '|').find_map(|i| {
        let glyph = i + 1 + s[i + 1..].iter().take_while(|c| is_space(**c)).count();
        if s.get(glyph) != Some(&'✻') {
            return None;
        }
        let start = glyph + 1;
        let n = s[start..].iter().take_while(|c| !is_space(**c)).count();
        let ws = s[start + n..].iter().take_while(|c| is_space(**c)).count();
        (n > 0 && ws > 0 && s.get(start + n + ws) == Some(&'|'))
            .then(|| s[start..start + n].iter().collect())
    })
}

/// `RC_FAILED.search(line)`: the status line after Remote Control drops.
pub(crate) fn is_rc_failed(line: &str) -> bool {
    line.contains("/rc failed")
}

/// Any of `DIALOG_MARKERS` in `line`.
pub(crate) fn has_dialog_marker(line: &str) -> bool {
    [
        "Do you want to proceed?",
        "Esc to cancel",
        "Enter to select",
    ]
    .iter()
    .any(|m| line.contains(m))
}

/// `FOOTER_NOISE.search(line)`:
/// `^\s*TM \d|bypass permissions|/rc\s*$|Update installed|^\s*[◯⏺] `.
pub(crate) fn is_footer_noise(line: &str) -> bool {
    let left = lstrip(line);
    let tm = left
        .strip_prefix("TM ")
        .is_some_and(|r| r.chars().next().is_some_and(is_digit));
    tm || line.contains("bypass permissions")
        || rstrip(line).ends_with("/rc")
        || line.contains("Update installed")
        || left.starts_with("◯ ")
        || left.starts_with("⏺ ")
}

/// `LIMIT.search(line)`, case-insensitive: a usage-limit notice.
pub(crate) fn is_limit(line: &str) -> bool {
    let chars: Vec<char> = line.chars().collect();
    [
        "usage limit reached",
        "limit reached",
        "hit your usage limit",
        "hit your limit",
        "resets at",
        "limit will reset",
    ]
    .iter()
    .any(|p| contains_ci(&chars, p))
}

/// `ERROR.search(line)`: `Traceback|API Error|Error: Exit code`.
pub(crate) fn is_error(line: &str) -> bool {
    ["Traceback", "API Error", "Error: Exit code"]
        .iter()
        .any(|m| line.contains(m))
}

fn digits_at(s: &[char], i: usize) -> usize {
    s[i..].iter().take_while(|c| is_digit(**c)).count()
}

/// Word boundary (`\b`) before index `i`.
fn boundary(s: &[char], i: usize) -> bool {
    let before = i > 0 && is_word(s[i - 1]);
    let after = s.get(i).is_some_and(|c| is_word(*c));
    before != after
}

/// End of the `\w` run that starts at a word boundary `i` (`\b\w+`), or
/// `None` when `i` is not the first character of a word.
pub(crate) fn boundary_word_run(s: &[char], i: usize) -> Option<usize> {
    if !s.get(i).is_some_and(|c| is_word(*c)) || !boundary(s, i) {
        return None;
    }
    Some(i + s[i..].iter().take_while(|c| is_word(**c)).count())
}

/// Length of a `TIMERS` match at `i`, trying the alternatives in order:
/// `\d+m \d+s | \b\d+s\b | ↓ [\d.]+k tokens | \d+:\d\d(?: [AP]M)?`.
fn timer_len(s: &[char], i: usize) -> Option<usize> {
    let at = |k: usize, c: char| s.get(k) == Some(&c);
    let d = digits_at(s, i);
    if d > 0 && at(i + d, 'm') && at(i + d + 1, ' ') {
        let d2 = digits_at(s, i + d + 2);
        if d2 > 0 && at(i + d + 2 + d2, 's') {
            return Some(d + 2 + d2 + 1);
        }
    }
    if d > 0 && boundary(s, i) && at(i + d, 's') && boundary(s, i + d + 1) {
        return Some(d + 1);
    }
    if at(i, '↓') && at(i + 1, ' ') {
        let n = s[i + 2..]
            .iter()
            .take_while(|c| is_digit(**c) || **c == '.')
            .count();
        let tail: Vec<char> = "k tokens".chars().collect();
        if n > 0 && s[i + 2 + n..].starts_with(&tail) {
            return Some(2 + n + tail.len());
        }
    }
    if d > 0 && at(i + d, ':') && digits_at(s, i + d + 1) >= 2 {
        let end = i + d + 3;
        let meridiem = at(end, ' ') && (at(end + 1, 'A') || at(end + 1, 'P')) && at(end + 2, 'M');
        return Some(end - i + if meridiem { 3 } else { 0 });
    }
    None
}

/// `TIMERS.sub("", line)`: elapsed times, token counts and clock times removed.
pub(crate) fn strip_timers(line: &str) -> String {
    let s: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < s.len() {
        if let Some(n) = timer_len(&s, i) {
            i += n;
            continue;
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

/// `re.search(r"\bctx (\d+)%", text)` and friends: the integer of the first
/// `<prefix>\d+%` match. `word_start` adds a leading `\b`; `space_after`
/// allows `\s*` between the prefix and the digits. A value past `u64` reads as
/// `None` (Python would return a big integer).
pub(crate) fn find_pct(
    text: &str,
    prefix: &str,
    word_start: bool,
    space_after: bool,
) -> Option<u64> {
    let s: Vec<char> = text.chars().collect();
    let p: Vec<char> = prefix.chars().collect();
    (0..s.len()).find_map(|i| {
        if !s[i..].starts_with(&p) || (word_start && !boundary(&s, i)) {
            return None;
        }
        let mut j = i + p.len();
        if space_after {
            j += s[j..].iter().take_while(|c| is_space(**c)).count();
        }
        let d = digits_at(&s, j);
        if d == 0 || s.get(j + d) != Some(&'%') {
            return None;
        }
        s[j..j + d].iter().collect::<String>().parse().ok()
    })
}
