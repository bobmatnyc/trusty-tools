//! Tests for `process_env_dump` (#8756). Split out so the rule file stays
//! under the production SLOC cap; this `_tests.rs` file is a test file.
//!
//! Each case goes through `evaluate_secret_file_read`, the entry `pm_guard`
//! calls, so an allow is an allow from every secret rule, and a deny proves
//! the rule is wired. A failure lists every command that misbehaved.

use super::*;
use crate::commands::pm_guard_secret_read::evaluate_secret_file_read;

/// The secret guard's verdict on one Bash `command`.
fn verdict(command: &str) -> Option<String> {
    let input = serde_json::json!({ "command": command });
    evaluate_secret_file_read("Bash", Some(&input))
}

fn assert_denies(commands: &[&str]) {
    let leaked: Vec<&str> = commands
        .iter()
        .copied()
        .filter(|c| !verdict(c).is_some_and(|r| r.contains("#8756")))
        .collect();
    assert!(leaked.is_empty(), "must deny under #8756: {leaked:#?}");
}

fn assert_allows(commands: &[&str]) {
    let denied: Vec<&str> = commands
        .iter()
        .copied()
        .filter(|c| verdict(c).is_some())
        .collect();
    assert!(denied.is_empty(), "must allow: {denied:#?}");
}

/// The rule itself answers without the entry point, for a caller that asks it
/// directly.
#[test]
fn the_rule_answers_on_its_own() {
    assert!(evaluate_process_env_dump_command("pm2 jlist").is_some());
    assert_eq!(evaluate_process_env_dump_command("pm2 ls"), None);
}

/// #8756: every `launchctl` form whose output carries an environment block.
#[test]
fn refuses_a_launchctl_environment_dump() {
    assert_denies(&[
        "launchctl print gui/501/com.trusty.mpm",
        "launchctl print gui/$(id -u)/com.trusty.mpm",
        "launchctl print gui/501",
        "launchctl print system/com.apple.example",
        "launchctl print gui/501/com.trusty.mpm | grep -E 'state|pid'",
        "sudo launchctl print system/com.example.daemon",
        "/bin/launchctl print gui/501/com.trusty.mpm",
        "sh -c 'launchctl print gui/501/com.trusty.mpm'",
        "cd /tmp && launchctl dumpstate > /dev/null; launchctl dumpstate | head",
        "launchctl export",
    ]);
}

/// #8756: every `pm2` form whose output carries a managed process's env.
#[test]
fn refuses_a_pm2_environment_dump() {
    assert_denies(&[
        "pm2 jlist",
        "pm2 prettylist",
        "pm2 env 0",
        "pm2 describe api",
        "pm2 desc api",
        "pm2 info 0",
        "pm2 show api",
        "pm2 jlist | jq '.[].name'",
        "npx pm2 jlist",
        "npx pm2@latest prettylist",
        "./node_modules/.bin/pm2 env 3",
        "PM2_HOME=/srv/pm2 pm2 jlist",
        "pm2 --no-color show api",
        "bash -lc \"pm2 jlist\"",
    ]);
}

/// #8756: status and log queries print no environment and still run.
#[test]
fn allows_process_manager_status_queries() {
    assert_allows(&[
        "launchctl list",
        "launchctl list com.trusty.mpm",
        "launchctl list -p",
        "launchctl kickstart -k gui/501/com.trusty.mpm",
        "launchctl bootout gui/501/com.trusty.mpm",
        "launchctl blame gui/501/com.trusty.mpm",
        "launchctl help print",
        "launchctl getenv PATH",
        "pm2 ls",
        "pm2 list",
        "pm2 status",
        "pm2 logs api",
        "pm2 logs api --lines 50",
        "pm2 logs info",
        "pm2 start app.js --name show",
        "pm2 restart api",
        "pm2 pid api",
        "git commit -m 'refuse pm2 jlist and launchctl print'",
        "cargo test -p trusty-mpm",
    ]);
}

/// #8756: a segment naming the shape that does not lex fails closed.
#[test]
fn refuses_what_it_cannot_read() {
    assert_denies(&["pm2 jlist 'unterminated", "launchctl print \"gui/501"]);
    assert_allows(&["pm2 logs 'unterminated"]);
}

/// #8756 round 3: `ps` with the BSD `e` or the `-E` option prints each
/// process's environment, bare and through `$( )`, `eval` and `sh -c`.
#[test]
fn refuses_a_ps_environment_listing() {
    assert_denies(&[
        "ps eww",
        "ps auxeww",
        "ps auxe",
        "ps axe",
        "ps ewwx -p 123",
        "ps e -o pid,command",
        "ps -Eww",
        "ps -E -p 123",
        "ps -ax -E",
        "sudo ps -Ef",
        "/bin/ps eww",
        "ps auxeww | grep node",
        "ps eww 'unterminated",
        "echo \"$(ps eww)\"",
        "eval 'ps auxe'",
        "sh -c 'ps -Eww'",
    ]);
}

/// #8756 round 3: a `/proc/<pid>/environ` read prints that process's env.
#[test]
fn refuses_a_proc_environ_read() {
    assert_denies(&[
        "cat /proc/1/environ",
        "tr '\\0' '\\n' < /proc/1/environ",
        "strings /proc/self/environ",
        "xargs -0 -n1 < /proc/$PID/environ",
        "cat /proc/*/environ",
        "cat /proc/1/task/7/environ",
        "echo \"$(cat /proc/1/environ)\"",
        "eval 'cat /proc/1/environ'",
        "sh -c 'strings /proc/1/environ'",
    ]);
}

/// #8756 round 3: `pm2 get` / `pm2 conf` print pm2's module config, where
/// a module keeps its credentials.
#[test]
fn refuses_a_pm2_module_config_print() {
    assert_denies(&[
        "pm2 get",
        "pm2 get pm2-slack:slack_url",
        "pm2 conf",
        "pm2 conf pm2-logrotate",
        "pm2 config pm2-slack",
        "echo \"$(pm2 conf)\"",
        "eval 'pm2 get'",
        "sh -c 'pm2 get pm2-slack'",
    ]);
}

/// #8756 round 3: an `ssh` remote command that prints the remote host's
/// environment.
#[test]
fn refuses_a_remote_environment_dump_over_ssh() {
    assert_denies(&[
        "ssh host env",
        "ssh host printenv",
        "ssh host printenv AWS_SECRET_ACCESS_KEY",
        "ssh host 'cat /proc/1/environ'",
        "ssh host sudo cat /proc/1/environ",
        "ssh -p 2222 user@host env",
        "ssh -i ~/.ssh/deploy -o StrictHostKeyChecking=no host printenv",
        "ssh host -t env",
        "ssh -- host env",
        "ssh host 'pm2 jlist'",
        "ssh host 'ps eww'",
        "ssh host set",
        "ssh host export -p",
        "ssh host 'declare -x'",
        "ssh host 'cd /srv/app && env | sort'",
        "ssh host sudo -u app printenv",
        "ssh host env -i FOO=1 printenv",
        "ssh host 'kubectl exec api -- env'",
        "gcloud compute ssh vm -- printenv",
        "ssh host \"printenv",
        "echo \"$(ssh host env)\"",
        "eval 'ssh host printenv'",
        "sh -c 'ssh host env'",
    ]);
}

/// #8756 round 3: routine `ps`, `pm2` and `ssh` forms print no environment.
#[test]
fn allows_routine_ps_pm2_and_ssh_forms() {
    assert_allows(&[
        "ps aux",
        "ps -ef",
        "ps -A",
        "ps -o pid,command",
        "ps -o user,pid,command -p 123",
        "ps -axo pid,user,command",
        "ps -eo pid,comm",
        "ps -p 123 -o etime=",
        "ps -U steve",
        "ps aux | grep node",
        "man ps",
        "pm2 ls",
        "pm2 logs api",
        "ssh host",
        "ssh -G host",
        "ssh host uptime",
        "ssh host 'git -C repo status'",
        "ssh -o StrictHostKeyChecking=no host uptime",
        "ssh host 'set -e; make'",
        "ssh host 'python3 -m venv env'",
        "ssh host 'cat /proc/1/status'",
        "cat /proc/meminfo",
        "ls /proc/1/",
        "git commit -m 'refuse ps eww, pm2 get, /proc/1/environ and ssh host env'",
    ]);
}
