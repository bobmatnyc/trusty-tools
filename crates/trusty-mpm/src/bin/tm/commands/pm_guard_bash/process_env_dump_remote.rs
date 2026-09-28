//! Refuse an `ssh` command whose remote command prints the remote host's
//! environment (#8756 round 3).
//!
//! Why: `ssh host env`, `ssh host printenv` and `ssh host 'cat
//! /proc/1/environ'` print a server's environment — its API keys — into the
//! transcript. The local rules never saw them: the remote command is an ssh
//! operand, and a bare `env` or `printenv` on the local machine is not a
//! managed process's environment.
//! What: [`ssh_dumps_remote_env`] finds the remote command (every word after
//! the destination, joined by spaces as ssh itself does) and refuses it when
//! the process or pod dump rules refuse it, or when one of its segments runs
//! an environment printer in command position: `printenv`, an `env` that runs
//! no command, or a bare `set`, `export`, `export -p`, `declare -x|-p` or
//! `typeset -x`. `sudo`, `doas`, `nohup`, `exec`, `command`, `time` and
//! `nice` prefixes and leading assignments are skipped first.
//! Test: `refuses_a_remote_environment_dump_over_ssh`,
//! `allows_routine_ps_pm2_and_ssh_forms`.

use super::super::bash_tokens::tokenize;
use super::super::{evaluate_pod_env_dump_command, split_shell_segments};
use super::{approximate_argv, evaluate_process_env_dump_command, program_name};

/// `ssh` option letters that take a value (OpenSSH `ssh(1)` synopsis).
const SSH_VALUE_OPTIONS: &str = "BbcDEeFIiJLlmOoPpQRSWw";

/// Prefix programs that run the rest of their argv, and the options among
/// theirs that take a value.
const PREFIXES: &[&str] = &["sudo", "doas", "nohup", "exec", "command", "time", "nice"];
const PREFIX_VALUE_OPTIONS: &[&str] = &[
    "-u", "-g", "-C", "-h", "-p", "-r", "-t", "-U", "-D", "-R", "-n",
];

/// Whether `ssh <args>` runs a remote command that prints an environment.
pub(super) fn ssh_dumps_remote_env(args: &[String]) -> bool {
    remote_command(args).is_some_and(|text| remote_text_dumps_env(&text))
}

/// The remote command of `ssh <args>`, or `None` for an interactive login.
///
/// What: OpenSSH reads options before AND after the destination until `--` or
/// the first non-option word after it; that word starts the remote command.
fn remote_command(args: &[String]) -> Option<String> {
    let (mut destination, mut options_done, mut i) = (false, false, 0);
    while let Some(arg) = args.get(i) {
        i += 1;
        if !options_done && arg == "--" {
            options_done = true;
        } else if !options_done && arg.len() > 1 && arg.starts_with('-') {
            let cluster = &arg[1..];
            if let Some(at) = cluster.find(|c| SSH_VALUE_OPTIONS.contains(c)) {
                // The value is attached (`-p2222`) or the next word.
                i += usize::from(at + 1 == cluster.len());
            }
        } else if destination {
            return Some(args[i - 1..].join(" "));
        } else {
            destination = true;
        }
    }
    None
}

/// Whether remote command text prints an environment.
fn remote_text_dumps_env(text: &str) -> bool {
    evaluate_process_env_dump_command(text).is_some()
        || evaluate_pod_env_dump_command(text).is_some()
        || split_shell_segments(text).iter().any(|segment| {
            let argv = tokenize(segment).unwrap_or_else(|_| approximate_argv(segment));
            prints_environment(&argv)
        })
}

/// Whether one segment's command-position program prints the environment.
fn prints_environment(argv: &[String]) -> bool {
    let words = command_words(argv);
    let Some((program, args)) = words.split_first() else {
        return false;
    };
    let options_only = args.iter().all(|a| a.starts_with('-'));
    match program_name(program) {
        "printenv" => true,
        "env" => env_prints(args),
        "set" => args.is_empty(),
        "export" => args.is_empty() || args.iter().all(|a| a == "-p"),
        "declare" | "typeset" => options_only,
        _ => false,
    }
}

/// `argv` past its leading assignments and [`PREFIXES`] programs.
fn command_words(argv: &[String]) -> &[String] {
    let mut i = 0;
    while let Some(word) = argv.get(i) {
        if word.contains('=') && !word.starts_with('-') {
            i += 1;
        } else if PREFIXES.contains(&program_name(word)) {
            i += 1;
            while let Some(option) = argv.get(i).filter(|o| o.starts_with('-')) {
                i += 1 + usize::from(PREFIX_VALUE_OPTIONS.contains(&option.as_str()));
            }
        } else {
            break;
        }
    }
    &argv[i..]
}

/// Whether `env <args>` prints the environment, directly or through the
/// command it runs. `-S` hands a string to a shell, so it fails closed.
fn env_prints(args: &[String]) -> bool {
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        if arg.starts_with("-S") || arg.starts_with("--split-string") {
            return true;
        } else if matches!(arg.as_str(), "-u" | "-C" | "-P" | "--unset" | "--chdir") {
            i += 2;
        } else if arg.starts_with('-') || arg.contains('=') {
            i += 1;
        } else {
            return prints_environment(&args[i..]);
        }
    }
    true
}
