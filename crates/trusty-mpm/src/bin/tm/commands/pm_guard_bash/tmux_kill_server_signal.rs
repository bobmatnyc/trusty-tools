//! The process-signal half of the whole-server kill floor (#10702).
//!
//! Why: the #10665 floor reads tmux verbs, so `pkill -x tmux`, `killall tmux`
//! and `kill $(tmux display -p '#{pid}')` killed the fleet's server with no
//! verb to read.
//! What: [`signal_kill`] reads the command, every `$(…)` and backtick body in
//! it, and every `eval` / `-c` string, as units. Each unit is split on `;`,
//! `&&`, `||`, `|`, `&` and newline outside quotes and substitutions, and each segment
//! is judged alone. A kill word counts only in command position: first in its
//! segment, after a compound head (`(`, `{`, `do`, `then`, `if`, `while`, `!`,
//! `find -exec`), or after a wrapper (`timeout 5`, `sudo -u root`,
//! `env -u X`, `nice -n 10`, `xargs`). Elsewhere it counts unless the segment
//! opens with a data program ([`DATA`]), so `grep kill tmux.log` is data.
//! A `pkill`, `killall` or `kill` word is judged on its operands, one
//! group per word or substitution: a group naming `tmux` is refused unless it
//! carries its own scratch server (`-L <label>` / `-S <path>` that
//! [`scratch_server`] accepts); a `pkill -f 'a|b'` pattern is judged per
//! alternative. A kill fed by `xargs`, or a `kill $var` beside a substitution
//! that names `tmux`, is refused when another part of the command names an
//! unscratched `tmux`. `pgrep`, `ps` and quoted text (`echo '…'`, a `gh`
//! body) do not signal and are allowed. This only adds refusals: a command it
//! cannot read falls through to the #10665 verb floor unchanged.
//! Residual (// See #10702): a pid passed through a file or an earlier
//! command (`tmux display … > f; kill $(cat f)`), an obfuscated name
//! (`tm[u]x`, `$T`, `printf`), and a literal pid or process group.
//! Test: `a_tmux_process_kill_is_refused`,
//! `a_scratch_or_unrelated_process_kill_is_allowed`,
//! `a_kill_word_outside_command_position_is_allowed`,
//! `each_compound_segment_is_judged_alone`,
//! `pipe_and_background_segments_are_judged_alone`,
//! `a_kill_in_a_loop_or_if_head_is_refused`,
//! `an_unlisted_wrapper_or_leading_redirect_fails_closed`.

use super::super::super::split_shell_segments;
use super::{ServerEnv, scratch_server};

/// Words after which the next word runs as a command.
const HEADS: &[&str] = &[
    "(", "{", "do", "then", "else", "elif", "if", "while", "until", "!", "|", "&", "&&", "||", ";",
    "-exec", "-execdir", "-ok",
];

/// #10702 r4: programs whose operands are data, so a kill word after one in
/// a segment naming `tmux` is text; any other opener fails closed.
const DATA: &[&str] = &[
    "grep", "egrep", "fgrep", "rg", "ag", "git", "gh", "echo", "printf", "cat", "less", "more",
    "head", "tail", "sed", "awk", "jq", "wc", "sort", "uniq", "diff", "man", "which", "type", "ls",
];

/// Programs that run their operand as a command (`sudo pkill`).
const WRAPPERS: &[&str] = &[
    "sudo", "doas", "env", "timeout", "nice", "nohup", "setsid", "stdbuf", "ionice", "exec",
    "command", "builtin", "time", "xargs",
];

/// Shells whose `-c` operand is a command string.
const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh"];

/// How deep nested `$(…)` / `eval` / `-c` bodies are read.
const MAX_DEPTH: usize = 4;

/// Words that end a kill word's operands at nesting depth zero.
const STOPS: &[&str] = &[
    ";", "&&", "||", "|", "&", "do", "done", "then", "fi", "else", "}", ")",
];

/// Why `command` signals the fleet's tmux server, or `None`.
pub(super) fn signal_kill(command: &str, env: &ServerEnv) -> Option<String> {
    let mut units = Vec::new();
    collect_units(command, 0, &mut units);
    let substitutions_name_tmux = units[1..].iter().any(|u| unscratched_tmux(u, env));
    units.iter().find_map(|unit| {
        // #10702 r3: each `;`/`&&`/`||`/newline segment is judged alone.
        compound_segments(unit).iter().find_map(|text| {
            judge_text(text, env, substitutions_name_tmux)
                .or_else(|| xargs_kill(text, command, env))
        })
    })
}

/// `command` and, recursively, its substitution bodies and `eval`/`-c` strings.
fn collect_units(command: &str, depth: usize, out: &mut Vec<String>) {
    out.push(command.to_string());
    if depth >= MAX_DEPTH {
        return;
    }
    let words = shell_words(command);
    let mut nested = substitutions(command);
    for pair in words.windows(2) {
        let head = base(&pair[0]);
        if (head == "eval" || pair[0] == "-c" || pair[0] == "-lc") && pair[1].contains(' ') {
            nested.push(pair[1].clone());
        }
    }
    for body in nested {
        collect_units(&body, depth + 1, out);
    }
}

/// The bodies of the outermost `$(…)` and backtick spans of `text`.
fn substitutions(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && bytes.get(i + 1) == Some(&b'(') {
            let mut level = 0usize;
            let start = i + 2;
            let mut j = i + 1;
            while j < bytes.len() {
                match bytes[j] {
                    b'(' => level += 1,
                    b')' => {
                        level -= 1;
                        if level == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            out.push(text[start..j.min(bytes.len())].to_string());
            i = j + 1;
        } else if bytes[i] == b'`' {
            let end = text[i + 1..].find('`').map_or(bytes.len(), |k| i + 1 + k);
            out.push(text[i + 1..end].to_string());
            i = end + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// Why a `pkill`/`killall`/`kill` word in `text` signals the fleet's server.
fn judge_text(text: &str, env: &ServerEnv, substitutions_name_tmux: bool) -> Option<String> {
    let words = shell_words(text);
    (0..words.len()).find_map(|i| {
        let positioned = runs_kill(&words, i);
        // #10702 r4: an unlisted wrapper or leading redirect fails closed.
        let program =
            positioned.or_else(|| kill_word(&words[i]).filter(|_| !data_opener(&words)))?;
        let groups = operand_groups(&words[i + 1..]);
        let named = groups.iter().any(|g| unscratched_tmux(g, env));
        let variable = program == "kill" && groups.iter().any(|g| is_variable(g));
        (named || (variable && substitutions_name_tmux)).then(|| refusal(program))
    })
}

/// A kill fed through `xargs` by another part of `command` naming `tmux`.
fn xargs_kill(segment: &str, command: &str, env: &ServerEnv) -> Option<String> {
    let words = shell_words(segment);
    let has_xargs = words.iter().any(|w| base(w) == "xargs");
    let kills = words
        .iter()
        .flat_map(|w| w.split_whitespace().map(str::to_string).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let kills = (0..kills.len()).any(|i| runs_kill(&kills, i).is_some());
    if !(has_xargs && kills) {
        return None;
    }
    split_shell_segments(command)
        .iter()
        .filter(|s| s.trim() != segment.trim())
        .any(|s| shell_words(s).iter().any(|w| unscratched_tmux(w, env)))
        .then(|| refusal("xargs kill"))
}

/// `text` split on `;`, `&&`, `||` and newline outside quotes, `$(…)` and
/// backticks (#10702 r3), so `kill 1\necho tmux` is two commands.
fn compound_segments(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let (mut out, mut start, mut i) = (Vec::new(), 0, 0);
    let (mut single, mut double, mut tick, mut depth) = (false, false, false, 0usize);
    while i < bytes.len() {
        let b = bytes[i];
        let width = match b {
            b'\\' if !single => {
                i += 2;
                continue;
            }
            b'\'' if !double && !tick && depth == 0 => {
                single = !single;
                0
            }
            _ if single => 0,
            b'"' if depth == 0 && !tick => {
                double = !double;
                0
            }
            b'`' => {
                tick = !tick;
                0
            }
            b'$' if bytes.get(i + 1) == Some(&b'(') => {
                depth += 1;
                i += 2;
                continue;
            }
            b'(' if depth > 0 => {
                depth += 1;
                0
            }
            b')' if depth > 0 => {
                depth -= 1;
                0
            }
            _ if double || tick || depth > 0 => 0,
            b';' | b'\n' => 1,
            b'&' | b'|' if bytes.get(i + 1) == Some(&b) => 2,
            // #10702 r4: a single `|` or bare `&` ends a command too; `>&`,
            // `&>` and `2>&1` are redirects.
            b'|' => 1,
            b'&' if i > 0 && matches!(bytes[i - 1], b'>' | b'<') => 0,
            b'&' if bytes.get(i + 1) == Some(&b'>') => 0,
            b'&' => 1,
            _ => 0,
        };
        if width > 0 {
            out.push(text[start..i].to_string());
            start = i + width;
            i += width;
        } else {
            i += 1;
        }
    }
    out.push(text[start..].to_string());
    out.retain(|s| !s.trim().is_empty());
    out
}

/// Whether the segment's first program, past compound heads, is in [`DATA`].
fn data_opener(words: &[String]) -> bool {
    words
        .iter()
        .find(|w| !HEADS.contains(&w.as_str()))
        .is_some_and(|w| DATA.contains(&base(w).as_str()))
}

/// The kill program `words[i]` runs, when it sits in command position.
fn runs_kill(words: &[String], i: usize) -> Option<&'static str> {
    let program = kill_word(&words[i])?;
    let opener = words[i]
        .rsplit('=')
        .next()
        .is_some_and(|t| t.starts_with(|c| "({`$!".contains(c)));
    (opener || command_position(words, i)).then_some(program)
}

/// Whether `words[i]` runs as a command rather than being an operand.
fn command_position(words: &[String], i: usize) -> bool {
    let mut j = i;
    while j > 0 {
        j -= 1;
        let word = words[j].as_str();
        if HEADS.contains(&word) || word.ends_with([';', '&', '|', '(', '{']) {
            return true;
        }
        if word == "-c" || word == "-lc" {
            return j > 0 && SHELLS.contains(&base(&words[j - 1]).as_str());
        }
        let skip = WRAPPERS.contains(&base(word).as_str())
            || word.starts_with('-')
            || word.parse::<f64>().is_ok()
            || (word.contains('=') && !word.starts_with('='))
            || (j > 0 && words[j - 1].starts_with('-'));
        if !skip {
            return false;
        }
    }
    true
}

/// The operands after a kill word, one group per word or substitution.
fn operand_groups(rest: &[String]) -> Vec<String> {
    let mut groups = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut level: i64 = 0;
    let mut tick = false;
    for word in rest {
        if level == 0 && !tick && (STOPS.contains(&word.as_str()) || word == "\\;") {
            break;
        }
        current.push(word);
        level += word.matches('(').count() as i64 - word.matches(')').count() as i64;
        tick ^= word.matches('`').count() % 2 == 1;
        if level <= 0 && !tick {
            groups.push(current.join(" "));
            current.clear();
            if level < 0 || word.ends_with(';') || word.ends_with('}') {
                break;
            }
            level = 0;
        }
    }
    if !current.is_empty() {
        groups.push(current.join(" "));
    }
    groups.retain(|g| !g.starts_with('-'));
    groups
}

/// Whether `group` names `tmux` in an alternative with no scratch server.
fn unscratched_tmux(group: &str, env: &ServerEnv) -> bool {
    group.split('|').any(|alternative| {
        if !alternative.to_ascii_lowercase().contains("tmux") {
            return false;
        }
        let words: Vec<String> = alternative
            .split_whitespace()
            .map(|w| w.trim_matches(|c| "'\"()`$;{}".contains(c)).to_string())
            .collect();
        let server: Vec<String> = words
            .windows(2)
            .filter(|w| w[0] == "-L" || w[0] == "-S")
            .flat_map(|w| w.to_vec())
            .collect();
        server.is_empty() || scratch_server(&server, env).is_err()
    })
}

/// Whether `group` is a shell variable such as `$p` or `${pid}`.
fn is_variable(group: &str) -> bool {
    let g = group.trim_matches(|c| "\"';".contains(c));
    g.starts_with('$') && !g.starts_with("$(")
}

/// `pkill`, `killall` or `kill` when `word` runs one, past `(`, `{`, `` ` ``,
/// `!`, `$(` and an `x=` assignment.
fn kill_word(word: &str) -> Option<&'static str> {
    if word.starts_with('-') {
        return None;
    }
    let tail = word.rsplit('=').next().unwrap_or(word);
    // #10702: a `case` arm `a)pkill` reaches shlex as one word; drop the `<pattern>)`.
    let tail = match tail.split_once(')') {
        Some((_, rest)) if !rest.is_empty() => rest,
        _ => tail,
    };
    let program = base(tail.trim_start_matches(|c| "({`$!".contains(c)));
    match program.trim_end_matches(|c| ";)`}".contains(c)) {
        "pkill" => Some("pkill"),
        "killall" => Some("killall"),
        "kill" => Some("kill"),
        _ => None,
    }
}

/// The refusal reason for `program`.
fn refusal(program: &str) -> String {
    format!(
        "`{program}` signals a tmux server process, which kills the fleet's server \
         and every session on it"
    )
}

/// `text` split as the shell would, or on whitespace when it cannot parse.
fn shell_words(text: &str) -> Vec<String> {
    shlex::split(text).unwrap_or_else(|| text.split_whitespace().map(str::to_string).collect())
}

/// The lower-cased basename of a program word (`/usr/bin/pkill` is `pkill`).
fn base(word: &str) -> String {
    word.rsplit('/').next().unwrap_or(word).to_ascii_lowercase()
}
