//! Test stand-in for `level_lint.py`, the replay tool's first end-to-end target (#12026).
//!
//! Why: P1 (#12028) has not ported `level_lint.py` yet, and the replay tool
//! must show one recording replaying clean against both the Python script and
//! a Rust binary. This example never ships; P1's real port replaces it.
//! What: the `level_lint.py` command line (`--level`, `--strict`, `--file`,
//! TEXT, else stdin) and its `path`, `flag`, `envvar`, `tool` and
//! `identifier` rules, with the script's overlap rule (an earlier rule's span
//! wins). The `sha` and `job` rules are not here: the recorded inputs hold
//! neither. Levels at `engineer` and above print nothing.
//! Test: `level_lint_recordings_replay_clean_against_the_rust_stand_in`.

use std::io::Read;
use std::process::ExitCode;

const LEVELS: [&str; 6] = [
    "non-technical",
    "vibe-coder",
    "pm",
    "engineer",
    "senior-engineer",
    "cto",
];
const EXTS: [&str; 20] = [
    "py", "rs", "md", "json", "jsonl", "toml", "yaml", "yml", "sh", "js", "ts", "tsx", "html",
    "css", "txt", "lock", "cfg", "ini", "sql", "log",
];
const TOOLS: [&str; 11] = [
    "cargo",
    "pytest",
    "tmux",
    "clippy",
    "rustc",
    "npm",
    "pnpm",
    "launchctl",
    "ripgrep",
    "git",
    "gh",
];

fn word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Spans `(start, end)` in char indices for one rule.
fn spans(rule: &str, t: &[char]) -> Vec<(usize, usize)> {
    let prev = |i: usize| if i == 0 { None } else { Some(t[i - 1]) };
    let run = |i: usize, f: &dyn Fn(char) -> bool| t[i..].iter().take_while(|c| f(**c)).count();
    let mut out = Vec::new();
    let mut i = 0;
    while i < t.len() {
        let found = match rule {
            "path" if !prev(i).is_some_and(word) => {
                let mut end = i + run(i, &|c| word(c) || matches!(c, '.' | '~' | '/' | '-'));
                while end > i && t[end - 1] == '.' {
                    end -= 1;
                }
                let s: String = t[i..end].iter().collect();
                let ext_ok = s
                    .rsplit_once('.')
                    .is_some_and(|(stem, e)| !stem.is_empty() && EXTS.contains(&e));
                if ext_ok
                    && t.get(end) == Some(&':')
                    && t.get(end + 1).is_some_and(char::is_ascii_digit)
                {
                    end += 1 + run(end + 1, &|c| c.is_ascii_digit());
                }
                (ext_ok && !t.get(end).copied().is_some_and(word)).then_some(end)
            }
            "flag"
                if t[i..].starts_with(&['-', '-'])
                    && !prev(i).is_some_and(|c| word(c) || c == '-') =>
            {
                t.get(i + 2)
                    .filter(|c| c.is_ascii_lowercase())
                    .map(|_| i + 3 + run(i + 3, &|c| word(c) || c == '-'))
            }
            "envvar" if t[i].is_ascii_uppercase() && !prev(i).is_some_and(word) => {
                let end = i + run(i, &|c| {
                    c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'
                });
                let s: String = t[i..end].iter().collect();
                let ok = s.contains('_') && !s.ends_with('_') && !s.contains("__");
                (ok && !t.get(end).copied().is_some_and(word)).then_some(end)
            }
            "tool" if !prev(i).is_some_and(|c| word(c) || c == '-') => TOOLS.iter().find_map(|w| {
                let end = i + w.chars().count();
                let hit = t[i..].starts_with(&w.chars().collect::<Vec<_>>());
                (hit && !t.get(end).copied().is_some_and(|c| word(c) || c == '-')).then_some(end)
            }),
            "identifier" if t[i] == '`' => t[i + 1..]
                .iter()
                .position(|c| *c == '`' || *c == '\n')
                .filter(|n| *n > 0 && t[i + 1 + n] == '`')
                .map(|n| i + n + 2),
            _ => None,
        };
        match found {
            Some(end) => {
                out.push((i, end));
                i = end;
            }
            None => i += 1,
        }
    }
    out
}

fn lint(text: &str, level: &str) -> Vec<String> {
    if LEVELS.iter().position(|l| *l == level) >= Some(3) {
        return Vec::new();
    }
    let t: Vec<char> = text.chars().collect();
    let mut taken: Vec<(usize, usize, &str)> = Vec::new();
    for rule in ["path", "flag", "envvar", "tool", "identifier"] {
        for (s, e) in spans(rule, &t) {
            if !taken.iter().any(|(ts, te, _)| s < *te && *ts < e) {
                taken.push((s, e, rule));
            }
        }
    }
    taken.sort();
    taken
        .iter()
        .map(|(s, e, rule)| {
            let m: String = t[*s..*e].iter().collect();
            let line = t[..*s].iter().filter(|c| **c == '\n').count() + 1;
            format!("level_lint [{level}]: {rule} `{m}` (line {line})")
        })
        .collect()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mut level, mut strict, mut file, mut words) = (None, false, None, Vec::new());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--level" => level = it.next().cloned(),
            "--strict" => strict = true,
            "--file" => file = it.next().cloned(),
            _ => words.push(a.clone()),
        }
    }
    let text = match (file, words.is_empty()) {
        (Some(f), _) => std::fs::read_to_string(f).unwrap_or_default(),
        (None, false) => words.join(" "),
        (None, true) => {
            let mut s = String::new();
            let _ = std::io::stdin().read_to_string(&mut s);
            s
        }
    };
    let level = level.unwrap_or_else(|| "pm".into());
    let known = LEVELS.contains(&level.as_str());
    if !known {
        eprintln!("level_lint: unknown level '{level}'; using pm");
    }
    let out = lint(&text, if known { &level } else { "pm" });
    for line in &out {
        println!("{line}");
    }
    ExitCode::from(u8::from(strict && !out.is_empty()))
}
