//! Refuse an unscoped environment dump inside a Kubernetes pod (#7648).
//!
//! Why: `kubectl exec <pod> -- env | grep -i LOG` printed a Twenty API key JWT
//! and a Postgres DSN into a transcript, and a bare `env` in a pod did the same
//! on 2026-09-20. A pod's environment is where Kubernetes injects its Secrets,
//! so a dump of it is a credential print that names no file — the #7266 rule
//! never sees it.
//! What: [`evaluate_pod_env_dump_command`] refuses a `kubectl`/`oc` `exec`, or
//! an `oc rsh`, whose remote command dumps the environment: `env` with no command to run,
//! `printenv` with no variable name, a bare `export`/`set`/`declare`, or a
//! `/proc/<pid>/environ` read — directly or inside `sh -c '…'`. Naming a
//! variable (`printenv LOG_LEVEL`) allows. A segment naming `kubectl exec`
//! that does not lex, or nesting past [`MAX_DEPTH`], fails CLOSED.
//! Test: `refuses_an_unscoped_pod_env_dump`, `allows_a_scoped_pod_command`,
//! `refuses_an_oc_rsh_env_dump`, `refuses_what_it_cannot_read`.

use super::bash_tokens::tokenize;
use super::split_shell_segments;

/// Programs whose `exec` subcommand runs a command inside a pod.
const POD_CLIS: &[&str] = &["kubectl", "oc"];

/// Pod-CLI subcommands that run a command inside a pod (#7648; `rsh` is
/// `oc`'s remote shell, critic HIGH 1).
const REMOTE_VERBS: &[&str] = &["exec", "rsh"];

/// `oc rsh` options whose value is the next token when written without `=`.
const RSH_VALUE_FLAGS: &[&str] = &[
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

/// Shells whose `-c` operand is a script to judge in turn.
const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ash", "ksh"];

/// `env` options whose value is the next token.
const ENV_VALUE_FLAGS: &[&str] = &["-u", "--unset", "-C", "--chdir"];

/// Nested `sh -c` / `env` levels followed before refusing.
const MAX_DEPTH: usize = 4;

/// `Some(reason)` when `command` dumps a pod's environment, else `None`.
///
/// Test: `refuses_an_unscoped_pod_env_dump`, `allows_a_scoped_pod_command`,
/// `refuses_what_it_cannot_read`.
pub(crate) fn evaluate_pod_env_dump_command(command: &str) -> Option<String> {
    if !REMOTE_VERBS.iter().any(|verb| command.contains(verb)) {
        return None;
    }
    split_shell_segments(command)
        .iter()
        .any(|segment| segment_dumps_pod_env(segment))
        .then(deny_reason)
}

/// Whether one segment is a pod `exec` whose remote command dumps the env.
fn segment_dumps_pod_env(segment: &str) -> bool {
    let Ok(argv) = tokenize(segment) else {
        // #7648: fail closed on a segment we cannot read that names the shape.
        let words: Vec<&str> = segment.split(|c: char| !is_word_byte(c)).collect();
        return words.iter().any(|w| POD_CLIS.contains(w))
            && words.iter().any(|w| REMOTE_VERBS.contains(w))
            && words
                .iter()
                .any(|w| matches!(*w, "env" | "printenv" | "environ"));
    };
    let Some(cli) = argv.iter().position(|t| POD_CLIS.contains(&basename(t))) else {
        return false;
    };
    let rest = &argv[cli + 1..];
    let separator = rest.iter().position(|t| t == "--");
    let options = &rest[..separator.unwrap_or(rest.len())];
    let Some(exec_at) = options
        .iter()
        .position(|t| REMOTE_VERBS.contains(&t.as_str()))
    else {
        return false;
    };
    match separator {
        Some(at) => dumps_environment(&rest[at + 1..], 0),
        // #7648 critic HIGH 1: `oc rsh <pod> <command…>` needs no `--`; the
        // words after the pod name are the remote command.
        None if options[exec_at] == "rsh" => rsh_remote_command(&options[exec_at + 1..])
            .is_some_and(|argv| dumps_environment(argv, 0)),
        // The deprecated `kubectl exec <pod> env` form has no `--`, so the
        // remote command cannot be told from the options; its last word decides.
        None => options[exec_at + 1..]
            .last()
            .is_some_and(|last| matches!(basename(last), "env" | "printenv")),
    }
}

/// The remote command of `oc rsh [options] <pod> <command…>`, if one follows.
///
/// What: skips options, and the value of each [`RSH_VALUE_FLAGS`] spelling
/// written without `=`; the first other word is the pod, and every word after
/// it is the command. `None` for an interactive shell (no command).
/// Test: `refuses_an_oc_rsh_env_dump`.
fn rsh_remote_command(args: &[String]) -> Option<&[String]> {
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        if RSH_VALUE_FLAGS.contains(&arg.as_str()) {
            i += 2;
        } else if arg.starts_with('-') {
            i += 1;
        } else {
            return args.get(i + 1..).filter(|command| !command.is_empty());
        }
    }
    None
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
    "an unscoped environment dump inside a pod (`kubectl exec … -- env`, `printenv`, \
     `export`, `set`, `/proc/*/environ`) is refused (issue #7648) — a pod's environment is \
     where Kubernetes injects its Secrets, so the dump prints them into the transcript. Query \
     one named variable instead (`kubectl exec <pod> -- printenv NAME`), or test for its \
     presence without printing it (`kubectl exec <pod> -- sh -c 'test -n \"$NAME\" && echo set'`)."
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_an_unscoped_pod_env_dump() {
        for command in [
            "kubectl exec my-pod -- env",
            "kubectl exec my-pod -- env | grep -i LOG",
            "kubectl -n prod exec -it my-pod -c app -- /usr/bin/env",
            "kubectl exec deploy/api -- printenv",
            "kubectl exec my-pod -- printenv -0",
            "kubectl exec my-pod -- env -i -u X",
            "kubectl exec my-pod -- env -- ",
            "kubectl exec my-pod -- sh -c 'env | sort'",
            "kubectl exec my-pod -- bash -lc \"printenv\"",
            "kubectl exec my-pod -- env FOO=1 sh -c env",
            "kubectl exec my-pod -- export",
            "kubectl exec my-pod -- sh -c set",
            "kubectl exec my-pod -- cat /proc/1/environ",
            "oc exec my-pod -- env",
            "kubectl exec my-pod env",
            "cd /tmp && kubectl exec my-pod -- env",
        ] {
            assert!(
                evaluate_pod_env_dump_command(command).is_some(),
                "`{command}` must deny"
            );
        }
    }

    /// 🔴 REGRESSION (#7648 critic HIGH 1): `oc rsh` runs a remote command
    /// with no `exec` and no `--`. Both deny rows ALLOWED on `8dfcf2e1e`.
    #[test]
    fn refuses_an_oc_rsh_env_dump() {
        for command in [
            "oc rsh mypod env",
            "oc rsh mypod -- env",
            "oc -n prod rsh -c app mypod printenv",
            "oc rsh --shell=/bin/bash mypod sh -c 'env | sort'",
        ] {
            assert!(
                evaluate_pod_env_dump_command(command).is_some(),
                "`{command}` must deny"
            );
        }
        for command in [
            "oc rsh mypod ls",
            "oc rsh mypod",
            "oc rsh mypod printenv HOME",
        ] {
            assert_eq!(
                evaluate_pod_env_dump_command(command),
                None,
                "`{command}` must allow"
            );
        }
    }

    #[test]
    fn allows_a_scoped_pod_command() {
        for command in [
            "kubectl exec my-pod -- printenv LOG_LEVEL",
            "kubectl exec my-pod -- sh -c 'echo $LOG_LEVEL'",
            "kubectl exec my-pod -- env FOO=1 node app.js",
            "kubectl exec my-pod -- ls /app",
            "kubectl exec my-pod printenv LOG_LEVEL",
            "kubectl exec -it my-pod -- sh",
            "kubectl get pods -n prod",
            "kubectl logs my-pod | grep env",
            "env | grep PATH",
            "printenv HOME",
        ] {
            assert_eq!(
                evaluate_pod_env_dump_command(command),
                None,
                "`{command}` must allow"
            );
        }
    }

    /// Error arms: an unlexable segment naming the shape, an unreadable inner
    /// script, and nesting past the bound all fail closed.
    #[test]
    fn refuses_what_it_cannot_read() {
        for command in [
            "kubectl exec my-pod -- env 'unclosed",
            "kubectl exec my-pod -- sh -c 'env \"unclosed'",
            "kubectl exec p -- env env env env env env true",
        ] {
            assert!(
                evaluate_pod_env_dump_command(command).is_some(),
                "`{command}` must deny"
            );
        }
        assert!(dumps_environment(&["true".to_string()], MAX_DEPTH + 1));
    }
}
