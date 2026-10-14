//! Unit tests for the guard-lift-at-init floor (`guard_lift_init.rs`).

use super::*;

#[test]
fn a_guard_lift_at_init_is_denied_in_every_spelling() {
    for command in [
        "tm fleet init --guard-lift",
        "tm fleet init --dir /w/architect --guard-lift",
        "tm fleet init --guard-lift --dir /w/architect",
        "tm fleet init --dir=/w/architect --no-launch --guard-lift",
        "/Users/x/.cargo/bin/tm fleet init --guard-lift",
        "cd /w/architect && tm fleet init --guard-lift",
        "echo go; tm fleet init --guard-lift",
        "env FOO=1 tm fleet init --guard-lift",
        "sh -c 'tm fleet init --guard-lift'",
        "bash -lc \"cd /w && tm fleet init --dir /w/a --guard-lift\"",
        "timeout 60 tm fleet init --guard-lift",
        "./scripts/init.sh --guard-lift",
        "scripts/init.sh --start --guard-lift",
        "bash /w/architect/scripts/init.sh --guard-lift",
        "ARCHITECT_GUARD_LIFT=1 scripts/init.sh",
        "env ARCHITECT_GUARD_LIFT=1 bash scripts/init.sh --start",
    ] {
        let reason = evaluate_guard_lift_init(command);
        assert!(reason.is_some(), "expected deny: {command}");
        assert!(
            reason.unwrap().contains("--guard-lift"),
            "the deny names the flag: {command}"
        );
    }
}

#[test]
fn an_unparseable_command_naming_the_lift_is_denied() {
    assert!(evaluate_guard_lift_init("tm fleet init --guard-lift 'unclosed").is_some());
}

#[test]
fn everything_else_passes() {
    for command in [
        "tm fleet init",
        "tm fleet init --no-guard-lift",
        "tm fleet init --dir /w/architect --no-launch",
        "tm fleet status --dir /w/architect",
        "scripts/init.sh --start",
        "grep -n guard-lift scripts/init.sh",
        "cat scripts/init.sh",
        "git commit -m 'docs: tm fleet init --guard-lift is the owner choice'",
        "tm fleet --help",
        "ARCHITECT_GUARD_LIFT=0 scripts/init.sh",
    ] {
        assert_eq!(evaluate_guard_lift_init(command), None, "{command}");
    }
}
