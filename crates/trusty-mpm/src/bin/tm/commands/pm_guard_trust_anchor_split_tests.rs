//! Unit tests for #8878 Q2 delete round 2: the critic's four HIGH findings
//! (H1-H4) and the owner's PM/agent split ("Keep the split", 2026-09-30).

use super::*;

/// The phrase [`anchor_reason`] carries.
const ANCHOR: &str = "resolves to a trust anchor";
/// The phrase [`unknown_reason`] carries.
const UNKNOWN: &str = "depends on a shell expansion";
/// The phrase the `xargs` delete deny carries.
const XARGS: &str = "delete run by `xargs`";

/// Run `command` from a subagent of a PM session (`agent_id` set).
fn agent_bash(fx: &Fixture, command: &str) -> Option<String> {
    let mut p = payload(fx, "Bash", json!({ "command": command }));
    p["agent_id"] = json!("agent-7");
    evaluate(&p, &pm_env(fx), MpmConfig::default)
}

/// Assert `command` is denied to both the PM and an agent with `phrase`.
fn denied_to_all(fx: &Fixture, command: &str, phrase: &str) {
    for (who, verdict) in [
        ("pm", pm_bash(fx, command)),
        ("agent", agent_bash(fx, command)),
    ] {
        let reason = verdict.unwrap_or_else(|| panic!("{who} allowed: {command}"));
        assert!(reason.contains(phrase), "{who} {command}: {reason}");
    }
}

/// H1: a wrapper option's value is not the program, even when it names a verb.
#[test]
fn a_wrapper_option_value_is_not_the_program() {
    let fx = fixture();
    launch_record(&fx, ARCHITECT, &fx.project);
    for command in [
        "env -u find cp a.txt ~/.trusty-mpm/config.toml",
        "env -u find rm -f ~/.trusty-mpm/architect-launch/60.architect",
        "sudo -u root find /tmp -exec cp a.txt ~/.trusty-mpm/config.toml \\;",
        "env -u rm cp a.txt ~/.trusty-mpm/config.toml",
    ] {
        denied_to_all(&fx, command, ANCHOR);
    }
}

/// H2: the command a `find -exec`-family action runs is read as a segment.
#[test]
fn a_find_exec_action_is_read_as_a_command() {
    let fx = fixture();
    launch_record(&fx, ARCHITECT, &fx.project);
    for command in [
        "find /tmp -maxdepth 0 -exec rm -f ~/.trusty-mpm/architect-launch/60.architect \\;",
        "find /tmp -maxdepth 0 -exec sh -c 'rm -rf ~/.trusty-mpm/architect-launch' \\;",
        "find /tmp -maxdepth 0 -exec mv ~/.trusty-mpm/architect-launch /tmp/x \\;",
        "find /tmp -maxdepth 0 -execdir cp a.txt ~/.trusty-mpm/config.toml {} +",
        "find /tmp -maxdepth 0 -ok bash -c 'cp a.txt ~/.trusty-mpm/config.toml' \\;",
    ] {
        denied_to_all(&fx, command, ANCHOR);
    }
    for command in [
        "find /tmp -maxdepth 0 -exec rm -f {} \\;",
        "find . -name '*.orig' -exec rm {} +",
        "find /tmp -maxdepth 0 -exec cat ~/.trusty-mpm/config.toml \\;",
    ] {
        assert_eq!(pm_bash(&fx, command), None, "denied: {command}");
    }
}

/// H3: a brace group spanning `/` is expanded, and each reading judged.
#[test]
fn a_brace_group_spanning_a_slash_is_expanded() {
    let fx = fixture();
    launch_record(&fx, ARCHITECT, &fx.project);
    for command in [
        "rm -rf ~/{.trusty-mpm,x}",
        "rm -rf ~/{.trusty-mpm/architect-launch,x}",
        "rm -f ~/{.trusty-mpm/config.toml,x}",
        "echo x | tee ~/{.trusty-mpm/config.toml,y}",
        "mv ~/{.trusty-mpm/config.toml,/tmp/x}",
        "mv ~/.trusty-mpm/{config.toml,config.bak}",
        "cp ~/.trusty-mpm/config.{bak,toml}",
    ] {
        denied_to_all(&fx, command, ANCHOR);
    }
    // A group the expander cannot read (nested in a comma body): deny.
    denied_to_all(&fx, "rm -f ~/{.trusty-mpm/{config.toml,x},y}", UNKNOWN);
    for command in [
        "rm -f {a,b}.txt",
        "rm -rf \"${TMPDIR}\"/x",
        "echo x > {a,b}.log",
    ] {
        assert_eq!(agent_bash(&fx, command), None, "denied: {command}");
    }
}

/// H4: a leading reserved word does not hide the command after it.
#[test]
fn a_reserved_word_does_not_hide_the_command() {
    let fx = fixture();
    let anchor = "~/.trusty-mpm/config.toml";
    for command in [
        format!("if [ -f {anchor} ]; then rm -f {anchor}; fi"),
        format!("for f in a; do rm -f {anchor}; done"),
        format!("while true; do rm -f {anchor}; done"),
        format!("{{ rm -f {anchor}; }}"),
        format!("! rm -f {anchor}"),
        format!("if true; then cp a.txt {anchor}; fi"),
        format!("until false; do mv {anchor} /tmp/x; done"),
        format!("if true; then ln -sf /tmp/x {anchor}; fi"),
    ] {
        denied_to_all(&fx, &command, ANCHOR);
    }
}

/// Owner ruling "Keep the split": the fail-closed over-denials bind the PM
/// only; an agent is denied a delete only when it reaches a real anchor.
#[test]
fn expansion_and_xargs_deletes_are_denied_to_the_pm_only() {
    let fx = fixture();
    launch_record(&fx, ARCHITECT, &fx.project);
    std::fs::create_dir_all(fx.home.join(ANCHOR_ROOT).join("logs/old")).expect("mkdir logs");
    for (command, phrase) in [
        ("rm -rf \"$VAR\"", UNKNOWN),
        ("rm -rf $VAR/architect-launch", UNKNOWN),
        ("echo a | xargs rm", XARGS),
        ("ls | xargs rm -f", XARGS),
        ("ls | xargs --bogus rm", XARGS),
        ("rm ~/*.log", ANCHOR),
        ("find ~ -name '*.log' -delete", ANCHOR),
        ("find ~/.trusty-mpm/twin -delete", ANCHOR),
        ("rmdir -p ~/.trusty-mpm/logs/old", ANCHOR),
        ("cd /tmp && rm -rf architect-launch", UNKNOWN),
        ("truncate -s 0 \"$F\"", UNKNOWN),
    ] {
        let reason = pm_bash(&fx, command).unwrap_or_else(|| panic!("pm allowed: {command}"));
        assert!(reason.contains(phrase), "pm {command}: {reason}");
        assert_eq!(agent_bash(&fx, command), None, "agent denied: {command}");
    }
}

/// An agent is still denied every delete whose path reaches a real anchor,
/// and every write main already denies it.
#[test]
fn an_agent_is_denied_a_delete_reaching_a_real_anchor() {
    let fx = fixture();
    launch_record(&fx, ARCHITECT, &fx.project);
    arm(&fx);
    for command in [
        "rm -f ~/.trusty-mpm/config.toml",
        "rm -rf ~/.trusty-mpm",
        "rm -rf ~/.trusty-mpm/architect-launch/*",
        "rm -f ~/.trusty-mpm/*.toml",
        "rm -rf ~/.*",
        "rm -f ~/.trusty-mpm/twin/armed/*.json",
        "find ~/.trusty-mpm/architect-launch -delete",
        "find ~/.trusty-mpm/twin/armed -name '*.json' -delete",
        "echo a | xargs rm -f ~/.trusty-mpm/config.toml",
        "rmdir ~/.trusty-mpm/architect-launch",
        "truncate -s 0 ~/.trusty-mpm/config.toml",
        "echo ~/.trusty-mpm/config.toml | xargs cp a.txt",
    ] {
        let reason = agent_bash(&fx, command).unwrap_or_else(|| panic!("agent allowed: {command}"));
        assert!(
            reason.contains("Trust-anchor write denied"),
            "{command}: {reason}"
        );
    }
    // Writes main already denies stay denied (no weaker than main).
    for command in ["cp a.txt $D", "cd /tmp && cp a.txt config.toml"] {
        let reason = agent_bash(&fx, command).unwrap_or_else(|| panic!("agent allowed: {command}"));
        assert!(reason.contains(UNKNOWN), "{command}: {reason}");
    }
}

/// An unknown thread kind is the PM (fail closed); `CLAUDE_MPM_SUB_AGENT`
/// marks an agent the same way `agent_id` does.
#[test]
fn an_unknown_thread_is_judged_as_the_pm() {
    let fx = fixture();
    let command = "rm -rf \"$VAR\"";
    let mut unknown = payload(&fx, "Bash", json!({ "command": command }));
    unknown
        .as_object_mut()
        .expect("object payload")
        .remove("session_id");
    let reason = evaluate(&unknown, &pm_env(&fx), MpmConfig::default).expect("unknown denied");
    assert!(reason.contains(UNKNOWN), "{reason}");
    let nested = HookEnv {
        sub_agent: true,
        ..pm_env(&fx)
    };
    let main = payload(&fx, "Bash", json!({ "command": command }));
    assert_eq!(evaluate(&main, &nested, MpmConfig::default), None);
}

/// The fail-closed arm of a `find -exec` whose wrapper the resolver cannot
/// measure: any delete word counts, so the start point is judged.
#[test]
fn an_unmeasured_find_exec_wrapper_deletes() {
    let fx = fixture();
    launch_record(&fx, ARCHITECT, &fx.project);
    denied_to_all(
        &fx,
        "find ~/.trusty-mpm/architect-launch -exec sudo -X rm {} +",
        ANCHOR,
    );
}

/// Round-2 low findings: `env -C`/`sudo -D` move the directory like a `cd`,
/// and zsh's `=rm` runs `rm`.
#[test]
fn a_chdir_wrapper_and_an_equals_program_are_read() {
    let fx = fixture();
    launch_record(&fx, ARCHITECT, &fx.project);
    for command in [
        "env -C /tmp cp a.txt config.toml",
        "sudo -D /tmp cp a.txt config.toml",
    ] {
        let reason = pm_bash(&fx, command).unwrap_or_else(|| panic!("allowed: {command}"));
        assert!(reason.contains(UNKNOWN), "{command}: {reason}");
    }
    denied_to_all(&fx, "=rm -f ~/.trusty-mpm/config.toml", ANCHOR);
}

/// Fail-Open Check on the agent's error arms: a glob directory or entry
/// parent that does not resolve, a directory that cannot be listed, and a
/// `find` action that cannot be re-spelled each deny.
#[test]
fn an_agent_delete_that_cannot_be_read_is_denied() {
    use std::os::unix::fs::PermissionsExt;
    let fx = fixture();
    std::os::unix::fs::symlink("loop", fx.cwd.join("loop")).expect("symlink loop");
    let locked = fx.cwd.join("locked");
    std::fs::create_dir(&locked).expect("mkdir locked");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let listed = std::fs::read_dir(&locked).is_ok();
    for (command, phrase) in [
        ("rm -f loop/*", "does not resolve"),
        ("rm -f loop/x", "does not resolve"),
        ("rm -f locked/*", "does not resolve"),
        (
            "find /tmp -maxdepth 0 -exec rm \"a\0b\" \\;",
            "does not lex",
        ),
    ] {
        if listed && command.contains("locked") {
            continue; // Running as root: the directory lists.
        }
        let reason = agent_bash(&fx, command).unwrap_or_else(|| panic!("agent allowed: {command}"));
        assert!(reason.contains(phrase), "{command}: {reason}");
    }
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("chmod back");
}

/// A word of thirty `{a,b}` groups (2^30 readings) is refused past the
/// expander's cap, quickly, and denied: the hook's timeout fails open.
#[test]
fn a_brace_bomb_is_denied_quickly() {
    let fx = fixture();
    let command = format!("rm -f ~/.trusty-mpm/{}", "{a,b}".repeat(30));
    let started = std::time::Instant::now();
    denied_to_all(&fx, &command, UNKNOWN);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "took {:?}",
        started.elapsed()
    );
}
