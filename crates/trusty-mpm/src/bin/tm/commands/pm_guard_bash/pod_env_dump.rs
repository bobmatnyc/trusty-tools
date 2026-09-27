//! Refuse an unscoped environment dump inside a Kubernetes pod or a container
//! (#7648).
//!
//! Why: `kubectl exec <pod> -- env | grep -i LOG` printed a Twenty API key JWT
//! and a Postgres DSN into a transcript, and a bare `env` in a pod did the same
//! on 2026-09-20. A pod's environment is where Kubernetes injects its Secrets,
//! and a container's is where `docker run -e`/`--env-file` put them, so a dump
//! of either is a credential print that names no file — the #7266 rule never
//! sees it.
//! What: [`evaluate_pod_env_dump_command`] refuses a `kubectl`/`oc` `exec`, an
//! `oc rsh`, or a `docker`/`podman`/`nerdctl`/compose `exec`, whose remote
//! command dumps the environment: `env` with no command to run, `printenv`
//! with no variable name, a bare `export`/`set`/`declare`, or a
//! `/proc/<pid>/environ` read — directly or inside `sh -c '…'`. Naming a
//! variable (`printenv LOG_LEVEL`) allows. An option the flag tables do not
//! know is read both as taking a value and as not, and any reading that dumps
//! refuses. A segment naming the shape that does not lex, or nesting past
//! [`MAX_DEPTH`], fails CLOSED.
//! Test: `refuses_an_unscoped_pod_env_dump`, `allows_a_scoped_pod_command`,
//! `refuses_an_oc_rsh_env_dump`, `refuses_what_it_cannot_read`,
//! `refuses_an_oc_rsh_dump_behind_an_unlisted_value_flag`,
//! `refuses_a_container_env_dump`.

use super::bash_tokens::tokenize;
use super::split_shell_segments;

/// Programs whose `exec` subcommand runs a command inside a pod.
const POD_CLIS: &[&str] = &["kubectl", "oc"];

/// Programs whose `exec` subcommand runs a command inside a container
/// (#7648 round 3: the same dump, owner ruling "fix the class").
const CONTAINER_CLIS: &[&str] = &[
    "docker",
    "podman",
    "nerdctl",
    "docker-compose",
    "podman-compose",
];

/// Subcommands that run a command inside a pod or container (#7648; `rsh` is
/// `oc`'s remote shell, critic HIGH 1).
const REMOTE_VERBS: &[&str] = &["exec", "rsh"];

/// Pod-CLI options known to take the next word as their value.
const POD_VALUE_FLAGS: &[&str] = &[
    "-c",
    "--container",
    "-n",
    "--namespace",
    "--shell",
    "--timeout",
    "-f",
    "--filename",
    "--context",
    "--kubeconfig",
    "--server",
    "--token",
    "--user",
    "--cluster",
];

/// Pod-CLI options known to take no value.
const POD_BOOLEAN_FLAGS: &[&str] = &[
    "-i", "-t", "-it", "-ti", "-T", "-q", "--stdin", "--tty", "--no-tty", "--quiet",
];

/// Container-CLI `exec` options known to take the next word as their value.
const CONTAINER_VALUE_FLAGS: &[&str] = &[
    "-e",
    "--env",
    "--env-file",
    "-u",
    "--user",
    "-w",
    "--workdir",
    "--detach-keys",
    "--index",
];

/// Container-CLI `exec` options known to take no value.
///
/// Why: #8523 round 4 LOW — `--no-TTY` (capital TTY) matches no real flag.
/// `docker exec`/`podman exec` have no such option at all; `docker compose
/// exec`/`podman-compose exec` spell it `-T, --no-tty` (lowercase), per
/// `docker compose exec --help`.
const CONTAINER_BOOLEAN_FLAGS: &[&str] = &[
    "-d",
    "--detach",
    "-i",
    "--interactive",
    "-t",
    "--tty",
    "-it",
    "-ti",
    "-T",
    "--no-tty",
    "--privileged",
];

/// Shells whose `-c` operand is a script to judge in turn.
const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ash", "ksh"];

/// `env` options whose value is the next token.
const ENV_VALUE_FLAGS: &[&str] = &["-u", "--unset", "-C", "--chdir"];

/// Nested `sh -c` / `env` levels followed before refusing.
const MAX_DEPTH: usize = 4;

/// Option tables for one CLI family: (takes a value, takes none).
type FlagTables = (&'static [&'static str], &'static [&'static str]);

/// `Some(reason)` when `command` dumps a pod's or container's environment.
///
/// Test: `refuses_an_unscoped_pod_env_dump`, `allows_a_scoped_pod_command`,
/// `refuses_what_it_cannot_read`, `refuses_a_container_env_dump`.
pub(crate) fn evaluate_pod_env_dump_command(command: &str) -> Option<String> {
    if !REMOTE_VERBS.iter().any(|verb| command.contains(verb)) {
        return None;
    }
    split_shell_segments(command)
        .iter()
        .any(|segment| segment_dumps_pod_env(segment))
        .then(deny_reason)
}

/// Whether one segment is a pod or container `exec` whose remote command
/// dumps the env.
fn segment_dumps_pod_env(segment: &str) -> bool {
    let is_cli = |word: &str| POD_CLIS.contains(&word) || CONTAINER_CLIS.contains(&word);
    let Ok(argv) = tokenize(segment) else {
        // #7648: fail closed on a segment we cannot read that names the shape.
        let words: Vec<&str> = segment.split(|c: char| !is_word_byte(c)).collect();
        return words.iter().any(|w| is_cli(w))
            && words.iter().any(|w| REMOTE_VERBS.contains(w))
            && words
                .iter()
                .any(|w| matches!(*w, "env" | "printenv" | "environ"));
    };
    let Some(cli) = argv.iter().position(|t| is_cli(basename(t))) else {
        return false;
    };
    let rest = &argv[cli + 1..];
    if CONTAINER_CLIS.contains(&basename(&argv[cli])) {
        // #7648 round 3: `docker [container|compose] exec [options] <name>
        // <command…>` takes no `--`; every word after the name is the command.
        let tables = (CONTAINER_VALUE_FLAGS, CONTAINER_BOOLEAN_FLAGS);
        return verb_positions(rest, &["exec"])
            .any(|at| any_command_dumps(&rest[at + 1..], tables));
    }
    let separator = rest.iter().position(|t| t == "--");
    let options = &rest[..separator.unwrap_or(rest.len())];
    let mut verbs = verb_positions(options, REMOTE_VERBS).peekable();
    if verbs.peek().is_none() {
        return false;
    }
    if let Some(at) = separator {
        return dumps_environment(&rest[at + 1..], 0);
    }
    // #7648 critic HIGH 1: `oc rsh <pod> <command…>` needs no `--`; the words
    // after the pod name are the remote command. The deprecated
    // `kubectl exec <pod> env` form is read the same way, and its last word
    // still decides on its own. #7648 round 3: every `exec`/`rsh` word is
    // tried, since an option before it may have taken the first as its value.
    verbs.any(|at| {
        let args = &options[at + 1..];
        any_command_dumps(args, (POD_VALUE_FLAGS, POD_BOOLEAN_FLAGS))
            || (options[at] == "exec"
                && args
                    .last()
                    .is_some_and(|last| matches!(basename(last), "env" | "printenv")))
    })
}

/// Every index of `args` holding one of `verbs`.
fn verb_positions<'a>(args: &'a [String], verbs: &'a [&str]) -> impl Iterator<Item = usize> + 'a {
    args.iter()
        .enumerate()
        .filter(|(_, t)| verbs.contains(&t.as_str()))
        .map(|(at, _)| at)
}

/// Whether any reading of `[options] <target> <command…>` runs a dump.
fn any_command_dumps(args: &[String], tables: FlagTables) -> bool {
    target_commands(args, tables)
        .into_iter()
        .any(|command| dumps_environment(command, 0))
}

/// The remote command of `[options] <target> <command…>` under every reading
/// of its options (#7648 round 3).
///
/// Why: `oc rsh --as adminuser mypod env` read `adminuser` as the pod because
/// `--as` was not listed as taking a value, and no list of a CLI's global
/// options stays complete.
/// What: walks the options over a reachability table. A listed value option
/// skips its value; a listed boolean option, or any `--flag=value`, skips
/// itself; an UNLISTED option reaches both the next word and the one after,
/// so the word after it is tried as its value and as the target. `--` ends
/// the options. The first non-option word reached is a target, and the words
/// after it (less a leading `--`) are a candidate command. The walk itself is
/// linear, but a run of unlisted flag/word pairs doubles the reachable-target
/// count at each pair, and each extra target's command slice is later walked
/// again by [`any_command_dumps`] — bounded quadratic in `args.len()`, not
/// linear (#8523 round 4).
/// Test: `refuses_an_oc_rsh_dump_behind_an_unlisted_value_flag`,
/// `refuses_a_container_env_dump`.
fn target_commands(args: &[String], (value, boolean): FlagTables) -> Vec<&[String]> {
    let mut reachable = vec![false; args.len() + 1];
    reachable[0] = true;
    let mut commands = Vec::new();
    for (i, arg) in args.iter().enumerate() {
        if !reachable[i] {
            continue;
        }
        let skip_value = (i + 2).min(args.len());
        if arg == "--" {
            if let Some(command) = args.get(i + 2..) {
                commands.push(command);
            }
        } else if !arg.starts_with('-') || arg == "-" {
            let command = &args[i + 1..];
            commands.push(match command.first() {
                Some(first) if first == "--" => &command[1..],
                _ => command,
            });
        } else if value.contains(&arg.as_str()) {
            reachable[skip_value] = true;
        } else if boolean.contains(&arg.as_str()) || arg.contains('=') {
            reachable[i + 1] = true;
        } else {
            // #7648 round 3: fail closed — an unlisted option may take a value.
            reachable[i + 1] = true;
            reachable[skip_value] = true;
        }
    }
    commands
}

/// Whether the remote `argv` prints the whole environment.
fn dumps_environment(argv: &[String], depth: usize) -> bool {
    if depth > MAX_DEPTH {
        return true;
    }
    if argv
        .iter()
        .any(|t| t.starts_with("/proc/") && t.ends_with("/environ"))
    {
        return true;
    }
    let Some(at) = argv.iter().position(|t| !is_assignment(t)) else {
        return false;
    };
    let args = &argv[at + 1..];
    match basename(&argv[at]) {
        "env" => env_dumps(args, depth),
        "printenv" => args.iter().all(|a| a.starts_with('-')),
        "set" => args.is_empty(),
        "export" | "declare" | "typeset" => args.iter().all(|a| a.starts_with('-')),
        "busybox" => dumps_environment(args, depth + 1),
        program if SHELLS.contains(&program) => {
            shell_script(args).is_some_and(|script| script_dumps_environment(script, depth + 1))
        }
        _ => false,
    }
}

/// `env [options] [NAME=value…] [command…]`: a dump unless a command follows.
fn env_dumps(args: &[String], depth: usize) -> bool {
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        if ENV_VALUE_FLAGS.contains(&arg.as_str()) {
            i += 2;
        } else if arg == "-S" || arg == "--split-string" {
            return args
                .get(i + 1)
                .is_none_or(|s| script_dumps_environment(s, depth + 1));
        } else if arg == "--" {
            return dumps_environment_or_empty(&args[i + 1..], depth);
        } else if arg.starts_with('-') || is_assignment(arg) {
            i += 1;
        } else {
            return dumps_environment(&args[i..], depth + 1);
        }
    }
    true
}

/// A dump when nothing follows `env --`, else the command that does.
fn dumps_environment_or_empty(argv: &[String], depth: usize) -> bool {
    argv.is_empty() || dumps_environment(argv, depth + 1)
}

/// The script operand of `sh -c <script>` (any flag cluster carrying `c`).
fn shell_script(args: &[String]) -> Option<&str> {
    let flag = args
        .iter()
        .position(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('c'))?;
    args.get(flag + 1).map(String::as_str)
}

/// Whether any segment of a shell script dumps the environment.
fn script_dumps_environment(script: &str, depth: usize) -> bool {
    split_shell_segments(script)
        .iter()
        .any(|segment| match tokenize(segment) {
            Ok(argv) => dumps_environment(&argv, depth),
            // #7648: an unreadable script fails closed when it names a dump.
            Err(_) => segment
                .split(|c: char| !is_word_byte(c))
                .any(|w| matches!(w, "env" | "printenv" | "environ")),
        })
}

/// A `NAME=value` shell assignment word.
fn is_assignment(token: &str) -> bool {
    token.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// The last path component of a program token.
fn basename(token: &str) -> &str {
    token.rsplit('/').next().unwrap_or(token)
}

/// Bytes that belong to a program or option word.
fn is_word_byte(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-')
}

/// The refusal, naming the scoped way through.
fn deny_reason() -> String {
    // #7648 round 3: the same refusal covers a container `exec`.
    "an unscoped environment dump inside a pod or container (`kubectl exec … -- env`, \
     `docker exec … env`, `printenv`, `export`, `set`, `/proc/*/environ`) is refused \
     (issue #7648) — a pod's or container's environment is where its Secrets are injected, \
     so the dump prints them into the transcript. Query one named variable instead \
     (`kubectl exec <pod> -- printenv NAME`, `docker exec <name> printenv NAME`), or test for \
     its presence without printing it (`kubectl exec <pod> -- sh -c 'test -n \"$NAME\" && echo set'`)."
        .to_string()
}

#[cfg(test)]
#[path = "pod_env_dump_tests.rs"]
mod tests;
