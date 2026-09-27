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
