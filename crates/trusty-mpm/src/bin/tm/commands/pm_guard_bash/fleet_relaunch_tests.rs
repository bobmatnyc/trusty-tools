//! Unit tests for the #10033 `tm fleet relaunch` floor (`fleet_relaunch.rs`).

use super::*;

/// Assert every command in `commands` is denied by this floor.
fn assert_denied(commands: &[&str]) {
    for command in commands {
        let reason = evaluate_fleet_relaunch(command);
        assert!(
            reason.as_deref().is_some_and(|r| r.contains("#10033")),
            "{command}: {reason:?}"
        );
    }
}

/// Assert no command in `commands` is denied by this floor.
fn assert_passed(commands: &[&str]) {
    for command in commands {
        assert_eq!(evaluate_fleet_relaunch(command), None, "{command}");
    }
}

/// #10033: `tm fleet relaunch` is denied in every spelling `tm` accepts —
/// clap prefixes, global options, paths, case, `trusty-mpm`, `cargo run`.
#[test]
fn every_spelling_of_tm_fleet_relaunch_is_denied() {
    assert_denied(&[
        "tm fleet relaunch",
        "tm fleet relaunch --dir /tmp/arch --session tm-architect",
        "tm fl rel",
        "tm fleet r",
        "tm --url http://127.0.0.1:1 fleet relaunch",
        "TM fleet relaunch",
        "/usr/local/bin/tm fleet relaunch",
        "~/.cargo/bin/tm fleet relaunch",
        "trusty-mpm fleet relaunch",
        "cargo run -p trusty-mpm --bin tm -- fleet relaunch",
        "sudo tm fleet relaunch",
        "FOO=1 tm fleet relaunch",
        "cd /tmp && tm fleet relaunch",
        "tm fleet status; tm fleet relaunch",
    ]);
}

/// #10033: a relaunch run through a wrapper, runner, substitution, `find
/// -exec`, `xargs`, an expanded program word or a `$'…'` quote is denied.
#[test]
fn a_relaunch_reached_through_a_wrapper_is_denied() {
    assert_denied(&[
        "bash -c 'tm fleet relaunch'",
        "sh -c \"tm fleet relaunch\"",
        "eval 'tm fleet relaunch'",
        "echo $(tm fleet relaunch)",
        "echo `tm fleet relaunch`",
        "watch tm fleet relaunch",
        "find . -maxdepth 0 -exec tm fleet relaunch \\;",
        "echo relaunch | xargs tm fleet",
        "T=tm; $T fleet relaunch",
        "$'tm' fleet relaunch",
    ]);
}

/// FAIL-CLOSED: a word the shell expands where the subcommand is chosen, and
/// a segment naming `tm` that does not lex, deny.
#[test]
fn an_expanded_word_where_the_subcommand_goes_denies() {
    assert_denied(&[
        "tm $VERB",
        "tm fleet \"$ACTION\"",
        "tm fleet `echo relaunch`",
        "tm fl $(printf relaunch)",
        "tm --url \"$U\" fleet relaunch",
        "find . -exec tm {} relaunch \\;",
        "tm fleet relaunch 'unclosed",
    ]);
}

/// An expanded word after a leaf subcommand, or under a group other than
/// `fleet`, cannot reach `fleet relaunch`, so it passes.
#[test]
fn an_expanded_word_after_another_subcommand_passes() {
    assert_passed(&[
        "tm fleet status --dir \"$DIR\"",
        "tm session $VERB",
        "ls | xargs tm fleet status --dir",
        "$EDITOR notes.md",
    ]);
}

/// Other `tm` verbs, a refused argv (`--help` never runs the verb), and text
/// that only mentions the verb pass.
#[test]
fn other_tm_commands_and_mentions_pass() {
    assert_passed(&[
        "tm fleet status",
        "tm fleet init --no-launch",
        "tm fleet relaunch --help",
        "tm fleet relaunch --no-such-flag",
        "echo 'tm fleet relaunch'",
        "grep -rn 'tm fleet relaunch' crates",
        "git commit -m \"$(cat <<'EOF'\nfix: deny tm fleet relaunch\nEOF\n)\"",
        "gh issue view 10033",
    ]);
}
