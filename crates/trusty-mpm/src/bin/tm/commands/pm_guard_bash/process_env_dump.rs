//! Refuse a process-manager query that prints a managed job's environment
//! (#8756).
//!
//! Why: `launchctl print gui/<uid>/<label>` prints a loaded job's
//! `EnvironmentVariables` block, and `pm2 jlist` / `pm2 prettylist` /
//! `pm2 env <id>` / `pm2 describe <id>` print a managed process's env. A
//! daemon's API keys live there, so each is a credential print that names no
//! file and no pod — the #7266 and #7648 rules never see it.
//! What: [`evaluate_process_env_dump_command`] refuses a segment where
//! `launchctl` runs `print`, `dumpstate` or `export`, or `pm2` runs `jlist`,
//! `prettylist`, `env`, `describe`, `desc`, `info` or `show`. The CLI may sit
//! anywhere in the segment (`sudo`, `npx`, an assignment prefix), and a
//! `sh -c` wrapper is read through [`split_shell_segments`]. An option before
//! the subcommand is read both as taking a value and as not, and any reading
//! that reaches a dump verb refuses. A filter after the dump (`| jq`,
//! `| grep`) does not allow it: the guard cannot prove a filter drops every
//! value. A segment naming the shape that does not lex fails CLOSED.
//! `launchctl list [label]`, `pm2 ls` and `pm2 logs` print no environment and
//! allow.
//!
//! Round 3 (#8756) adds three more shapes that print another process's
//! environment: `ps` with the BSD `e` or the `-E` option (`ps eww`, `ps
//! auxe`, `ps -Eww`; the dashed `-e` selects every process and allows), a
//! read of a `/proc/<pid>/environ` path, and `pm2 get` / `pm2 conf` (pm2's
//! module config, which holds module credentials). An `ssh` remote command
//! that prints the remote environment is read by [`remote`].
//! Test: `refuses_a_launchctl_environment_dump`,
//! `refuses_a_pm2_environment_dump`, `allows_process_manager_status_queries`,
//! `refuses_what_it_cannot_read`, `refuses_a_ps_environment_listing`,
//! `refuses_a_proc_environ_read`, `refuses_a_pm2_module_config_print`,
//! `refuses_a_remote_environment_dump_over_ssh`,
//! `allows_routine_ps_pm2_and_ssh_forms`.

#[path = "process_env_dump_remote.rs"]
mod remote;

use super::bash_tokens::tokenize;
use super::split_shell_segments;

/// Words whose absence lets a command skip the parse entirely.
const MARKERS: &[&str] = &["launchctl", "pm2", "ps", "environ", "ssh"];

/// `launchctl` subcommands whose output carries an environment (#8756):
/// `print` shows a service's (and a GUI domain's) environment block,
/// `dumpstate` every job's, and the legacy `export` launchd's own.
const LAUNCHCTL_DUMP_VERBS: &[&str] = &["print", "dumpstate", "export"];

/// `pm2` subcommands whose output carries a managed process's env (#8756):
/// `jlist`/`prettylist` print each `pm2_env`, `env` lists one process's
/// variables, and `describe` (aliases `desc`, `info`, `show`) prints a
/// "Divergent env variables" table.
// #8756: `get` / `conf` / `config` print pm2's module config (module keys).
const PM2_DUMP_VERBS: &[&str] = &[
    "jlist",
    "prettylist",
    "env",
    "describe",
    "desc",
    "info",
    "show",
    "get",
    "conf",
    "config",
];

/// `ps` option letters that take a value, dashed (`-o fmt`) and BSD-style
/// (`o fmt`), across macOS and procps.
const PS_DASH_VALUE_OPTIONS: &str = "CGgMNOopqstUu";
const PS_BSD_VALUE_OPTIONS: &str = "kNOopTtU";
const PS_LONG_VALUE_OPTIONS: &[&str] = &[
    "--cols",
    "--columns",
    "--format",
    "--group",
    "--Group",
    "--lines",
    "--pid",
    "--ppid",
    "--quick-pid",
    "--rows",
    "--sid",
    "--sort",
    "--tty",
    "--user",
    "--User",
    "--width",
];

/// `Some(reason)` when `command` prints a managed or remote process's
/// environment.
///
/// Test: `refuses_a_launchctl_environment_dump`,
/// `refuses_a_pm2_environment_dump`, `allows_process_manager_status_queries`,
/// `refuses_a_ps_environment_listing`, `refuses_a_proc_environ_read`,
/// `refuses_a_remote_environment_dump_over_ssh`.
pub(crate) fn evaluate_process_env_dump_command(command: &str) -> Option<String> {
    if !MARKERS.iter().any(|marker| command.contains(marker)) {
        return None;
    }
    split_shell_segments(command)
        .iter()
        .any(|segment| segment_dumps_process_env(segment))
        .then(deny_reason)
}

/// A program word's basename, without an `npx`-style `@version`.
fn program_name(word: &str) -> &str {
    let program = word.rsplit('/').next().unwrap_or(word);
    program.split_once('@').map_or(program, |(name, _)| name)
}

/// The dump-verb table for a program word, if it names `launchctl` or `pm2`.
fn dump_verbs(word: &str) -> Option<&'static [&'static str]> {
    match program_name(word) {
        "launchctl" => Some(LAUNCHCTL_DUMP_VERBS),
        // `npx pm2@latest jlist` names the same CLI.
        "pm2" => Some(PM2_DUMP_VERBS),
        _ => None,
    }
}

/// Whether one segment runs a `launchctl`/`pm2` subcommand that prints env.
fn segment_dumps_process_env(segment: &str) -> bool {
    let Ok(argv) = tokenize(segment) else {
        // #8756: fail closed on a segment we cannot read that names the shape.
        let words: Vec<&str> = segment
            .split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '@')))
            .collect();
        return words
            .iter()
            .any(|w| dump_verbs(w).is_some_and(|verbs| words.iter().any(|v| verbs.contains(v))))
            || argv_dumps_env(&approximate_argv(segment));
    };
    argv_dumps_env(&argv)
}

/// `segment` cut on whitespace with quote and substitution syntax trimmed: the
/// fail-closed reading of a segment that does not lex (#8756).
fn approximate_argv(segment: &str) -> Vec<String> {
    segment
        .split_whitespace()
        .map(|w| w.trim_matches(['\'', '"', '(', ')', '$', '`', '<', '>', ';']))
        .map(str::to_string)
        .collect()
}

/// Whether an argv prints a process's environment: a `launchctl`/`pm2` dump
/// verb, `ps` showing environments, a `/proc/<pid>/environ` path, or an `ssh`
/// remote command that dumps one.
fn argv_dumps_env(argv: &[String]) -> bool {
    argv.iter().enumerate().any(|(at, word)| {
        let rest = &argv[at + 1..];
        dump_verbs(word)
            .is_some_and(|verbs| subcommands(rest).into_iter().any(|sub| verbs.contains(&sub)))
            // #8756: `ps e`/`ps -E`, `/proc/<pid>/environ`, `ssh host env`.
            || (program_name(word) == "ps" && ps_shows_environment(rest))
            || names_a_process_environ(word)
            || (program_name(word) == "ssh" && remote::ssh_dumps_remote_env(rest))
    })
}

/// Whether `ps <args>` prints each process's environment (#8756).
///
/// What: the dashed `-E` (macOS) or a BSD-style option word — letters with no
/// dash — holding `e` (macOS and procps) shows it; the dashed `-e` selects
/// every process and does not. An option that takes a value (`-o user`, `-p
/// 12`, `U steve`) consumes it, so a format or user name is never read as
/// options.
fn ps_shows_environment(args: &[String]) -> bool {
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        i += 1;
        if arg.starts_with("--") {
            i += usize::from(PS_LONG_VALUE_OPTIONS.contains(&arg.as_str()));
            continue;
        }
        let (cluster, env, values) = match arg.strip_prefix('-') {
            Some(cluster) => (cluster, 'E', PS_DASH_VALUE_OPTIONS),
            None if arg.bytes().all(|b| b.is_ascii_alphabetic()) => {
                (arg.as_str(), 'e', PS_BSD_VALUE_OPTIONS)
            }
            None => continue,
        };
        for (at, c) in cluster.char_indices() {
            if c == env {
                return true;
            }
            if values.contains(c) {
                i += usize::from(at + 1 == cluster.len());
                break;
            }
        }
    }
    false
}

/// Whether `word` names a `/proc/<pid>/environ` file, or a glob over a
/// `/proc/<pid>/` directory that can select one (#8756).
fn names_a_process_environ(word: &str) -> bool {
    let Some(at) = word.find("/proc/") else {
        return false;
    };
    if word.chars().any(char::is_whitespace) {
        return false; // prose, such as a commit message naming the path
    }
    let parts: Vec<&str> = word[at..].split('/').filter(|p| !p.is_empty()).collect();
    let last = parts.last().copied().unwrap_or_default();
    parts.len() >= 3 && (last == "environ" || last.contains(['*', '?', '[']))
}

/// Every word that can be the subcommand of `[options] <subcommand> …`.
///
/// What: an option reaches the next word, and — unless it is `--flag=value` —
/// the one after, since it may take that word as its value; `--` reaches the
/// word after it. The first non-option word reached is a candidate, and the
/// walk does not continue past it. Mirrors `pod_env_dump`'s fail-closed
/// reading of an unlisted option (#7648 round 3).
fn subcommands(args: &[String]) -> Vec<&str> {
    let mut reachable = vec![false; args.len() + 2];
    reachable[0] = true;
    let mut found = Vec::new();
    for (i, arg) in args.iter().enumerate() {
        if !reachable[i] {
            continue;
        }
        if arg == "--" {
            reachable[i + 1] = true;
        } else if !arg.starts_with('-') || arg == "-" {
            found.push(arg.as_str());
        } else {
            reachable[i + 1] = true;
            reachable[i + 2] = !arg.contains('=') || reachable[i + 2];
        }
    }
    found
}

/// The refusal, naming the status queries that print no environment.
fn deny_reason() -> String {
    "a query that prints another process's environment (`launchctl print`, `launchctl \
     dumpstate`, `pm2 jlist|prettylist|env|describe|show|info`, `pm2 get|conf`, `ps eww` / \
     `ps -E`, a `/proc/<pid>/environ` read, or `ssh <host> env|printenv`) is refused (issue \
     #8756) — a launchd job's EnvironmentVariables, a pm2 process's env or module config, and \
     a server's environment carry its API keys, so the dump prints them into the transcript, \
     and a `| jq` or `| grep` filter after it cannot be proven to drop every value. For \
     status, use `launchctl list <label>` (PID and LastExitStatus), `pm2 ls` / `pm2 pid \
     <name>`, `ps aux` / `ps -o pid,command`, or `ssh <host> <status command>`."
        .to_string()
}

#[cfg(test)]
#[path = "process_env_dump_tests.rs"]
mod tests;
