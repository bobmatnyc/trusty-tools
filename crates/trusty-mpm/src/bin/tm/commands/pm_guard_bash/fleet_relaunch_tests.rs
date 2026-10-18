//! Unit tests for the #10033 `tm fleet relaunch` floor (`fleet_relaunch.rs`).

use super::*;

/// Assert every command in `commands` is denied by this floor; a failure
/// lists every row that passed, not only the first.
fn assert_denied(commands: &[&str]) {
    let wrong: Vec<&str> = commands
        .iter()
        .copied()
        .filter(|c| !evaluate_fleet_relaunch(c).is_some_and(|r| r.contains("#10033")))
        .collect();
    assert!(
        wrong.is_empty(),
        "{} rows not denied: {wrong:#?}",
        wrong.len()
    );
}

/// Assert no command in `commands` is denied by this floor; a failure lists
/// every denied row with its reason.
fn assert_passed(commands: &[&str]) {
    let wrong: Vec<(&str, String)> = commands
        .iter()
        .filter_map(|c| evaluate_fleet_relaunch(c).map(|r| (*c, r)))
        .collect();
    assert!(wrong.is_empty(), "{} rows denied: {wrong:#?}", wrong.len());
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

/// Fail-open check: each arm that could let an unread `tm` through denies —
/// a `$'…'`-spelled program, text nested past the depth bound, and an
/// expanded program word with an expanded subcommand in a command naming
/// `tm`. An expanded program in a command that never names `tm` passes.
#[test]
fn every_unreadable_arm_naming_tm_denies() {
    assert_denied(&[
        r"$'\x74\x6d' fleet relaunch",
        "echo $(echo $(echo $(echo $(tm fleet relaunch))))",
        "T=tm; $T $VERB",
    ]);
    assert_passed(&["$RUNNER $ARG", "echo $(echo $(echo $(echo $(ls))))"]);
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
        "echo 'tm fleet relaunch'",
        "grep -rn 'tm fleet relaunch' crates",
        "git commit -m \"$(cat <<'EOF'\nfix: deny tm fleet relaunch\nEOF\n)\"",
        "gh issue view 10033",
    ]);
}

/// #10033 critic r1 finding 1: shell syntax bash strips before `tm` runs —
/// redirections, a background `&`, subshell and group closers — does not
/// hide the relaunch. An argv clap refuses for any reason but help or
/// version is read loosely, so a refused flag after `relaunch` denies too.
#[test]
fn shell_syntax_around_a_relaunch_does_not_hide_it() {
    assert_denied(&[
        "tm fleet relaunch 2>&1",
        "tm fleet relaunch > log",
        "tm fleet relaunch >log",
        "tm fleet relaunch &>/dev/null",
        "tm fleet relaunch </dev/null",
        "tm fleet relaunch <<< x",
        "tm fleet relaunch &",
        "tm fleet relaunch&",
        "2>/dev/null tm fleet relaunch",
        "tm 2>/dev/null fleet relaunch",
        "(cd x && tm fleet relaunch)",
        "( tm fleet relaunch )",
        "(tm fleet relaunch) 2>&1",
        "{ tm fleet relaunch; }",
        "tm fleet relaunch --no-such-flag",
    ]);
}

/// #10033 critic r1 finding 2: an `xargs` word that is not the program, or
/// a `tm`/`trusty-mpm` word that is not the utility `xargs` runs, passes.
#[test]
fn a_tm_word_xargs_does_not_run_passes() {
    assert_passed(&[
        "grep -rn xargs crates/trusty-mpm",
        "git ls-files | xargs grep -l trusty-mpm",
        "git ls-files | xargs cargo test -p trusty-mpm",
        "xargs grep -w tm",
        "git ls-files | xargs -0 grep -l tm",
    ]);
    assert_denied(&[
        "echo relaunch | xargs -n1 tm fleet",
        "echo relaunch | xargs sudo tm fleet",
    ]);
}

/// #10033 critic r1 finding 3: a command naming `tm` only through variables
/// set in the root command, read inside a substitution, denies; so does an
/// expanded program nested past the depth bound.
#[test]
fn an_expanded_program_in_a_substitution_is_judged_on_the_root_command() {
    assert_denied(&[
        "T=tm A=fleet B=relaunch; echo $($T $A $B)",
        "T=tm; echo $($T fleet relaunch)",
        "T=tm; echo $(echo $(echo $(echo $($T fleet relaunch))))",
    ]);
}

/// #10033 critic r1 finding 4: brace expansion or a glob where the
/// subcommand is chosen denies.
#[test]
fn a_brace_expansion_or_glob_where_the_subcommand_goes_denies() {
    assert_denied(&[
        "tm {fleet,} {relaunch,}",
        "tm fleet {relaunch,status}",
        "tm fleet rel*",
        "tm fl?et relaunch",
        "tm fleet [r]elaunch",
    ]);
    assert_passed(&["tm fleet status '*'", "tm fleet status --dir 'a{b,c}'"]);
}

/// #10033 critic r1 finding 5: the indirect routes the #8902 floor refuses
/// (`opaque_route`) deny when the command names `tm`.
#[test]
fn an_opaque_route_in_a_command_naming_tm_denies() {
    assert_denied(&[
        "echo tm fleet relaunch | sh",
        "bash <<< 'tm fleet relaunch'",
        "echo relaunch | xargs -I{} sh -c 'tm fleet {}'",
        "echo 'tm fleet relaunch' | xargs env",
        "parallel ::: 'tm fleet relaunch'",
        "echo tm | xargs -I{} {} fleet relaunch",
    ]);
    assert_passed(&[
        "echo hello | sh",
        "cat notes.txt | grep tm",
        "git ls-files crates/trusty-mpm | xargs -I{} wc -l {}",
        "ls | xargs -I{} cp {} crates/trusty-mpm/",
    ]);
}

/// #10033 critic r1 finding 6: `cargo run` hands trailing positionals to the
/// binary without `--`.
#[test]
fn cargo_run_without_dashes_denies() {
    assert_denied(&[
        "cargo run --bin tm fleet relaunch",
        "cargo run -q -p trusty-mpm --bin tm fleet relaunch",
        "cargo run --bin=tm -- fleet relaunch",
    ]);
    assert_passed(&[
        "cargo run --bin tm fleet status",
        "cargo test -p trusty-mpm fleet_relaunch",
        "cargo run --bin tm -- fleet relaunch --help",
    ]);
}

/// The pass shapes the critic verified at 73341c014d stay passing.
#[test]
fn critic_verified_pass_shapes_stay_passing() {
    assert_passed(&[
        "tm fleet status",
        "tm fleet relaunch --help",
        "tm fleet",
        "tm help fleet relaunch",
        "tm --version fleet relaunch",
        "grep -rn 'tm fleet relaunch' crates",
        "echo 'tm fleet relaunch'",
        "git commit -m \"fix: tm fleet relaunch is denied\"",
        "git commit -F - <<'EOF'\nfix: deny tm fleet relaunch\nEOF",
        "gh pr create --title t --body \"Denies tm fleet relaunch from hooked sessions\"",
        "cat > notes.md <<'EOF'\nrun tm fleet relaunch from a terminal\nEOF",
        "cargo test -p trusty-mpm fleet_relaunch",
    ]);
}
