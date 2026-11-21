//! The process-signal half of the whole-server kill floor (#10702).
//!
//! Why: the #10665 floor reads tmux verbs, so `pkill -x tmux`, `killall tmux`
//! and `kill $(tmux display -p '#{pid}')` killed the fleet's server with no
//! verb to read.
//! What: [`signal_kill`] reads each segment (wrappers such as `sh -c` and
//! `sudo` included) and names why it signals a tmux server: `pkill` or
//! `killall` with an operand naming `tmux`, `kill` with an argument naming
//! `tmux` (a `$(tmux display …)` or `$(pgrep tmux)` pid), or `xargs kill` fed
//! by a `pgrep`/`pidof` naming `tmux`. A match whose text names a scratch
//! server (`-L <label>` or `-S <path>` that [`scratch_server`] accepts) is
//! allowed. `pgrep`, `ps` and printed text do not signal and are allowed.
//! Test: `a_tmux_process_kill_is_refused`,
//! `a_scratch_or_unrelated_process_kill_is_allowed`.

use super::super::super::split_shell_segments;
use super::{ServerEnv, scratch_server};

/// Programs that run the rest of their words as a command.
const WRAPPERS: &[&str] = &["sudo", "command", "exec", "nohup", "env", "doas"];

/// Why `command` signals the fleet's tmux server, or `None`.
pub(super) fn signal_kill(command: &str, env: &ServerEnv) -> Option<String> {
    let segments = split_shell_segments(command);
    let finders: Vec<Vec<String>> = segments
        .iter()
        .map(|s| argv(s))
        .filter(|a| matches!(a.first().map(String::as_str), Some("pgrep" | "pidof")))
        .collect();
    segments.iter().find_map(|segment| {
        let argv = argv(segment);
        let (program, args) = argv.split_first()?;
        match program.as_str() {
            "pkill" | "killall" => judge(program, args, env),
            "kill" => judge("kill", args, env),
            "xargs" if args.iter().any(|a| base(a) == "kill") => finders
                .iter()
                .find_map(|f| judge("xargs kill", f.get(1..).unwrap_or(&[]), env)),
            _ => None,
        }
    })
}

/// `Some` when an operand of `program` names `tmux` and `args` name no
/// scratch server.
fn judge(program: &str, args: &[String], env: &ServerEnv) -> Option<String> {
    let names_tmux = args
        .iter()
        .any(|a| !a.starts_with('-') && a.to_ascii_lowercase().contains("tmux"));
    if !names_tmux {
        return None;
    }
    let words: Vec<String> = args
        .iter()
        .flat_map(|a| a.split_whitespace())
        .map(|w| w.trim_matches(|c| "'\"()`$".contains(c)).to_string())
        .collect();
    let server: Vec<String> = words
        .windows(2)
        .filter(|w| w[0] == "-L" || w[0] == "-S")
        .flat_map(|w| w.to_vec())
        .collect();
    if !server.is_empty() && scratch_server(&server, env).is_ok() {
        return None;
    }
    Some(format!(
        "`{program}` signals a tmux server process, which kills the fleet's server \
         and every session on it"
    ))
}

/// `segment`'s words with leading wrappers and assignments removed, the
/// program word reduced to its lower-cased basename.
fn argv(segment: &str) -> Vec<String> {
    let words = shlex::split(segment)
        .unwrap_or_else(|| segment.split_whitespace().map(str::to_string).collect());
    let mut rest = words.as_slice();
    while let Some((first, tail)) = rest.split_first() {
        let assignment = first.contains('=') && !first.starts_with('-');
        if WRAPPERS.contains(&base(first).as_str()) || assignment || first.starts_with('-') {
            rest = tail;
        } else {
            break;
        }
    }
    let mut out = rest.to_vec();
    if let Some(first) = out.first_mut() {
        *first = base(first);
    }
    out
}

/// The lower-cased basename of a program word (`/usr/bin/pkill` is `pkill`).
fn base(word: &str) -> String {
    word.rsplit('/').next().unwrap_or(word).to_ascii_lowercase()
}
