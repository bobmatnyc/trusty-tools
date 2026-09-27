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
//! Test: `refuses_a_launchctl_environment_dump`,
//! `refuses_a_pm2_environment_dump`, `allows_process_manager_status_queries`,
//! `refuses_what_it_cannot_read`.

use super::bash_tokens::tokenize;
use super::split_shell_segments;

/// `launchctl` subcommands whose output carries an environment (#8756):
/// `print` shows a service's (and a GUI domain's) environment block,
/// `dumpstate` every job's, and the legacy `export` launchd's own.
const LAUNCHCTL_DUMP_VERBS: &[&str] = &["print", "dumpstate", "export"];

/// `pm2` subcommands whose output carries a managed process's env (#8756):
/// `jlist`/`prettylist` print each `pm2_env`, `env` lists one process's
/// variables, and `describe` (aliases `desc`, `info`, `show`) prints a
/// "Divergent env variables" table.
const PM2_DUMP_VERBS: &[&str] = &[
    "jlist",
    "prettylist",
    "env",
    "describe",
    "desc",
    "info",
    "show",
];

/// `Some(reason)` when `command` prints a launchd or pm2 job's environment.
///
/// Test: `refuses_a_launchctl_environment_dump`,
/// `refuses_a_pm2_environment_dump`, `allows_process_manager_status_queries`.
pub(crate) fn evaluate_process_env_dump_command(command: &str) -> Option<String> {
    if !command.contains("launchctl") && !command.contains("pm2") {
        return None;
    }
    split_shell_segments(command)
        .iter()
        .any(|segment| segment_dumps_process_env(segment))
        .then(deny_reason)
}

/// The dump-verb table for a program word, if it names `launchctl` or `pm2`.
fn dump_verbs(word: &str) -> Option<&'static [&'static str]> {
    let program = word.rsplit('/').next().unwrap_or(word);
    match program.split_once('@').map_or(program, |(name, _)| name) {
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
            .any(|w| dump_verbs(w).is_some_and(|verbs| words.iter().any(|v| verbs.contains(v))));
    };
    argv.iter().enumerate().any(|(at, word)| {
        dump_verbs(word).is_some_and(|verbs| {
            subcommands(&argv[at + 1..])
                .into_iter()
                .any(|sub| verbs.contains(&sub))
        })
    })
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
    "a process-manager query that prints a managed job's environment (`launchctl print`, \
     `launchctl dumpstate`, `pm2 jlist`, `pm2 prettylist`, `pm2 env`, `pm2 describe|show|info`) \
     is refused (issue #8756) — a launchd job's EnvironmentVariables and a pm2 process's env \
     carry its API keys, so the dump prints them into the transcript, and a `| jq` or `| grep` \
     filter after it cannot be proven to drop every value. For status, use `launchctl list \
     <label>` (PID and LastExitStatus) or `pm2 ls` / `pm2 pid <name>`."
        .to_string()
}

#[cfg(test)]
#[path = "process_env_dump_tests.rs"]
mod tests;
