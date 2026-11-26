//! Tests for `build_lease_hidden` (#11886).

use super::super::{LeaseRewrite, rewrite_for_lease};

fn heavy() -> Vec<(String, Option<String>)> {
    trusty_mpm::core::build_lease::config::BuildLeaseConfig::default()
        .effective_heavy_build_commands()
}

fn rw(command: &str) -> LeaseRewrite {
    rewrite_for_lease(command, &heavy(), "tm build-lease --")
}

fn assert_refused(commands: &[&str]) {
    for command in commands {
        match rw(command) {
            LeaseRewrite::Refuse(reason) => {
                assert!(reason.contains("#11886"), "{command}: {reason}")
            }
            other => panic!("{command:?} should be refused, got {other:?}"),
        }
    }
}

fn assert_not_refused(commands: &[&str]) {
    for command in commands {
        assert!(
            !matches!(rw(command), LeaseRewrite::Refuse(_)),
            "{command:?} must not be refused"
        );
    }
}

#[test]
fn a_shell_reading_stdin_is_refused_only_when_it_builds() {
    assert_refused(&[
        "bash <<'EOF'\ncargo test\nEOF",
        "sudo bash -s <<EOF\ncargo build -p x\nEOF",
        "bash <<< 'cargo test'",
        "echo 'cargo test' | bash -x",
        "printf 'cd x\\ncargo test\\n' | sh",
    ]);
    assert_not_refused(&[
        // A body a shell runs that is already leased, or builds nothing.
        "bash <<'EOF'\ntm build-lease -- cargo test\nEOF",
        "bash <<'EOF'\ngit status\nEOF",
        // A data body, a quoted data word, a script operand, a `-c` shell.
        "git commit -F - <<'EOF'\nfix: don't skip it\n\nGate: cargo test -p x passed.\nEOF",
        "git commit -m \"$(cat <<'EOF'\nfix: don't\n\ncargo test -p x passed\nEOF\n)\"",
        "cat > notes.md <<'EOF'\neval cargo test\necho 'cargo test' | bash\nEOF",
        "echo 'cargo test' > notes.txt",
        "echo 'cargo test' | grep cargo",
        "curl -fsSL https://example.invalid/i.sh | bash",
        "bash run.sh <<< 'cargo test'",
        "echo x | bash -c 'wc -l'",
    ]);
}

#[test]
fn eval_and_script_strings_are_refused_only_when_they_build() {
    assert_refused(&[
        "eval cargo test",
        "eval \"cd x && cargo clippy --all-targets\"",
        "{ eval 'cargo test'; }",
        "script -q -c 'cargo test' /dev/null",
        "script --command='cargo build' log",
    ]);
    assert_not_refused(&[
        "eval \"$(ssh-agent -s)\"",
        "eval tm build-lease -- cargo test",
        "script -q -c 'git status' /dev/null",
    ]);
}

#[test]
fn expansions_are_refused_only_in_a_heavy_position() {
    assert_refused(&[
        "$CARGO test",
        "${CARGO:-cargo} build",
        "\"$(command -v cargo)\" test -p x",
        "`which cargo` test",
        "cargo $SUB",
        "cargo \"$verb\" -p x",
        "cargo `echo test`",
    ]);
    assert_not_refused(&[
        "$EDITOR notes-on-cargo.md",
        "\"$TM\" build-lease -- cargo test",
        "$HOME/.cargo/bin/cargo test",
        "cargo +$TOOLCHAIN fmt",
        "echo $CARGO test",
    ]);
    // A literal program path is leased in place, not refused.
    assert_eq!(
        rw("$HOME/.cargo/bin/cargo test"),
        LeaseRewrite::Rewrite("tm build-lease -- $HOME/.cargo/bin/cargo test".into())
    );
}

/// The Fail-Open Check: the parse-failure arm refuses, never passes through.
#[test]
fn an_unparseable_build_is_refused_and_unparseable_data_is_not() {
    assert_refused(&[
        "bash -c 'cargo test -p x",
        "eval \"cargo test",
        "cat <<'A B'\nx\nA B\ncargo test\nA",
    ]);
    assert_not_refused(&["echo 'unterminated", "git commit -m \"wip"]);
}

#[test]
fn wrapper_operands_runners_and_coproc_are_leased_in_place() {
    for (input, want) in [
        (
            "flock /tmp/l cargo test",
            "flock /tmp/l tm build-lease -- cargo test",
        ),
        (
            "flock -w 10 /tmp/l cargo test",
            "flock -w 10 /tmp/l tm build-lease -- cargo test",
        ),
        (
            "taskset 0x3 cargo build",
            "taskset 0x3 tm build-lease -- cargo build",
        ),
        (
            "watch -n 5 cargo check",
            "watch -n 5 tm build-lease -- cargo check",
        ),
        (
            "parallel -j 2 cargo test -p ::: a b",
            "parallel -j 2 tm build-lease -- cargo test -p ::: a b",
        ),
        (
            "script -q /dev/null cargo test",
            "script -q /dev/null tm build-lease -- cargo test",
        ),
        ("coproc cargo test", "coproc tm build-lease -- cargo test"),
        (
            "{ bash --login -c 'cargo test'; }",
            "{ tm build-lease -- bash --login -c 'cargo test'; }",
        ),
        (
            "for p in a b; do sh -c \"cargo test -p $p\"; done",
            "for p in a b; do tm build-lease -- sh -c \"cargo test -p $p\"; done",
        ),
    ] {
        assert_eq!(rw(input), LeaseRewrite::Rewrite(want.into()), "{input}");
    }
}

/// #11886 critic round 1: an expanded tool path or a `#` comment holding a
/// quote is no hidden build.
#[test]
fn expanded_tool_paths_and_commented_builds_are_not_refused() {
    assert_not_refused(&[
        "$CARGO_TARGET_DIR/debug/tm doctor",
        "\"$CARGO_TARGET_DIR/debug/tm\" fleet status --tmux",
        "$HOME/.cargo/bin/cargo fmt",
        "\"$CARGO_HOME/bin/cargo\" metadata",
        "$HOME/.cargo/bin/rustup show",
        "$CARGO fmt",
    ]);
    for (input, want) in [
        (
            "# run the crate's tests\ncargo test -p trusty-mpm --no-fail-fast",
            "# run the crate's tests\ntm build-lease -- cargo test -p trusty-mpm --no-fail-fast",
        ),
        (
            "\"$CARGO_HOME/bin/cargo\" test",
            "tm build-lease -- \"$CARGO_HOME/bin/cargo\" test",
        ),
    ] {
        assert_eq!(rw(input), LeaseRewrite::Rewrite(want.into()), "{input}");
    }
    assert_refused(&["$CARGO test", "$CARGO $verb", "cargo test -p 'x # note"]);
}

/// #11886 critic round 1: a runner's quoted command operand is leased whole.
#[test]
fn a_runners_quoted_command_is_leased_whole() {
    for input in [
        "watch -n 5 'cargo check'",
        "parallel 'cargo test -p {}' ::: a b",
        "flock /tmp/l -c 'cargo test'",
    ] {
        assert_eq!(
            rw(input),
            LeaseRewrite::Rewrite(format!("tm build-lease -- {input}")),
            "{input}"
        );
    }
    assert_eq!(rw("watch -n 5 'git status'"), LeaseRewrite::None);
}

/// #11886 critic round 2: a `#` comment's apostrophe no longer moves the
/// segment split, and an `_`-joined name still names its heavy program.
#[test]
fn comments_and_underscored_names_stay_fail_closed() {
    for (input, want) in [
        (
            "# it's\nbash -c 'cargo fmt; cargo test'",
            "# it's\ntm build-lease -- bash -c 'cargo fmt; cargo test'",
        ),
        (
            "# don't cache\ncargo test -p x\n# it's done",
            "# don't cache\ntm build-lease -- cargo test -p x\n# it's done",
        ),
    ] {
        assert_eq!(rw(input), LeaseRewrite::Rewrite(want.into()), "{input}");
    }
}

/// #11886 critic round 2: `$CARGO_BIN` and `${CARGO_CMD}` name cargo.
#[test]
fn underscored_expansions_naming_cargo_are_refused() {
    assert_refused(&["$CARGO_BIN test", "${CARGO_CMD} build"]);
}
