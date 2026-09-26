//! Rewrite heavy build commands to run under `tm build-lease` (#8261).
//!
//! Why: the machine-wide builder cap moved from the dispatch to the build
//! command (owner ruling 2026-09-24, option D). The hook cannot wait — its
//! budget is seconds — so it rewrites the command and the lease waits inside
//! the Bash call itself. Composed commands are the norm (`cd crate && cargo
//! test`, `CARGO_BUILD_JOBS=6 cargo test … > log 2>&1; echo EXIT=$?`), so the
//! rewrite works per simple command: `tm build-lease --` is inserted in front
//! of the heavy program word, inside whatever composition surrounds it, and
//! every redirect, env prefix, `cd` and exit-status echo stays exactly where the
//! author put it.
//!
//! What: [`rewrite_for_lease`] returns [`LeaseRewrite::Rewrite`] with the new
//! command, [`LeaseRewrite::Refuse`] for the one shape it cannot rewrite (a
//! heavy build inside an UNTERMINATED `$(…)`), or [`LeaseRewrite::None`]. A
//! heavy build inside `$(…)` or backticks is leased inside the substitution; one
//! inside `sh -c '…'`, `bash -c`, `env -S`, `xargs` or `find -exec` leases the
//! whole wrapper. Which word is a heavy program is `build_lease_program`'s call.
//! Quoted DATA is never matched: `git commit -m "fix cargo test"` is not a build.
//! Test: the `#[cfg(test)]` suite below.

use super::build_lease_program::{
    LEADING_KEYWORDS, find_exec_commands, heavy_program_offset, words,
};
use super::heredoc::HeredocBodies;
use super::shell_lex::{QuoteScan, WrappedCommand, wrapped_command};
use super::{paren_substitution_live_at, split_shell_segments, split_shell_segments_raw};
use crate::commands::hook_rewrite::is_env_assignment;

/// What the hook does with one Bash command.
///
/// Test: every test below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LeaseRewrite {
    /// Not a heavy build; run it as written.
    None,
    /// Run this command instead.
    Rewrite(String),
    /// A heavy build the rewrite cannot reach; deny with this reason.
    Refuse(String),
}

/// Decide the lease rewrite for `command`.
///
/// Why: see the module doc.
/// What: `heavy` is the `(program, optional subcommand)` table; `prefix` is
/// the already shell-safe lease invocation to insert, e.g.
/// `'/path/tm' build-lease --` (optionally with `--wait-secs N`). Nothing
/// inside a here-document body is rewritten or refused. Every simple command whose program is heavy — at top
/// level or inside a substitution body — gets `<tm> build-lease -- ` inserted
/// before its program word; a wrapper hiding one is leased whole.
/// Test: `a_plain_heavy_build_is_wrapped`, `composed_commands_are_wrapped_in_place`,
/// `quoted_data_is_not_a_build`, `nested_builds_are_leased`,
/// `an_unterminated_substitution_is_refused`,
/// `an_already_leased_command_is_left_alone`, `light_cargo_verbs_are_left_alone`,
/// `a_quoted_heredoc_body_is_never_rewritten`,
/// `an_unquoted_heredoc_body_is_never_rewritten`,
/// `an_unterminated_substitution_in_a_heredoc_is_not_refused`,
/// `the_lease_goes_outside_sudo`, `cargo_aliases_and_plugins_are_heavy`,
/// `a_cargo_inside_a_case_arm_is_leased`,
/// `non_compiling_invocations_of_heavy_verbs_are_left_alone`.
pub(crate) fn rewrite_for_lease(
    command: &str,
    heavy: &[(String, Option<String>)],
    prefix: &str,
) -> LeaseRewrite {
    if command.trim().is_empty() || heavy.is_empty() {
        return LeaseRewrite::None;
    }
    // #8261 critic round 1 (CRITICAL): a here-document body is DATA — a commit
    // message, a file's content — even when it is handed to a shell. Nothing
    // inside one is ever rewritten or refused.
    let bodies = HeredocBodies::scan(command);
    let mut inserts = segment_inserts(command, 0, heavy);
    inserts.retain(|at| !bodies.contains(*at));
    for (start, end) in substitution_spans(command) {
        if bodies.contains(start) {
            continue;
        }
        let Some(end) = end else {
            // An unterminated substitution cannot be rewritten in place; refuse
            // only when it hides a heavy build.
            if !segment_inserts(&command[start..], start, heavy).is_empty() {
                return LeaseRewrite::Refuse(refusal());
            }
            continue;
        };
        let inner = segment_inserts(&command[start..end], start, heavy);
        inserts.extend(inner.into_iter().filter(|at| !bodies.contains(*at)));
    }
    if inserts.is_empty() {
        return LeaseRewrite::None;
    }
    inserts.sort_unstable();
    inserts.dedup();
    let mut out = String::with_capacity(command.len() + inserts.len() * 32);
    let mut last = 0;
    for at in inserts {
        out.push_str(&command[last..at]);
        out.push_str(prefix);
        out.push(' ');
        last = at;
    }
    out.push_str(&command[last..]);
    LeaseRewrite::Rewrite(out)
}

/// Insertion offsets (absolute, `base` added) for every simple command in `text`.
///
/// What: a heavy simple command gets an insertion before its program word; a
/// `sh -c` / `bash -c` / `env -S` / `xargs` wrapper whose inner command runs a
/// heavy build gets one before the wrapper, so the whole wrapper runs leased.
fn segment_inserts(text: &str, base: usize, heavy: &[(String, Option<String>)]) -> Vec<usize> {
    let origin = text.as_ptr() as usize;
    split_shell_segments_raw(text)
        .into_iter()
        .filter_map(|seg| {
            let seg_start = base + (seg.as_ptr() as usize - origin);
            if let Some(off) = heavy_program_offset(seg, heavy) {
                return Some(seg_start + off);
            }
            let hidden = match wrapped_command(seg.trim()) {
                WrappedCommand::Inner(inner) => hides_a_heavy_build(&inner, heavy),
                _ => false,
            } || find_exec_commands(seg)
                .iter()
                .any(|inner| hides_a_heavy_build(inner, heavy));
            hidden
                .then(|| command_start_offset(seg).map(|off| seg_start + off))
                .flatten()
        })
        .collect()
}

/// Whether `argv`, run as one command, is a heavy build (#8261 round 3).
///
/// Why: `tm build-lease` must refuse to run anything the classifier would not
/// have leased, so the lease program is never a way to run an arbitrary
/// command (critic finding 4).
/// What: shell-joins `argv` into ONE simple command and asks
/// [`rewrite_for_lease`] whether it would lease it.
/// Test: `a_lease_argv_is_heavy_only_when_the_classifier_says_so`.
pub(crate) fn is_heavy_build(argv: &[String], heavy: &[(String, Option<String>)]) -> bool {
    let Ok(joined) = shlex::try_join(argv.iter().map(String::as_str)) else {
        return false;
    };
    matches!(
        rewrite_for_lease(&joined, heavy, "tm build-lease --"),
        LeaseRewrite::Rewrite(_)
    )
}

/// Whether a wrapper's inner command runs a heavy build at any depth.
fn hides_a_heavy_build(inner: &str, heavy: &[(String, Option<String>)]) -> bool {
    split_shell_segments(inner)
        .iter()
        .any(|seg| heavy_program_offset(seg, heavy).is_some())
}

fn refusal() -> String {
    "Build lease (#8261): this command runs a heavy build (cargo build/test/clippy/check/\
     doc/install/run/bench, or `builders.heavy_build_commands`) inside an unterminated \
     `$(…)` or backtick substitution, which the hook cannot wrap in `tm build-lease`. Every \
     heavy build on this machine takes a build slot so concurrent builds cannot exhaust its \
     memory. Close the substitution or run the build as its own command — or wrap it \
     yourself: `tm build-lease -- <command>`."
        .to_string()
}

/// The `(start, end)` byte span of every live `$(…)` and backtick body;
/// `end` is `None` for an unterminated one.
fn substitution_spans(command: &str) -> Vec<(usize, Option<usize>)> {
    let scan = QuoteScan::new(command);
    let bytes = command.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if paren_substitution_live_at(&scan, bytes, i) == Some(true) {
            let mut depth = 1usize;
            let mut j = i + 2;
            while j < bytes.len() && depth > 0 {
                match bytes[j] {
                    b'(' => depth += 1,
                    b')' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            out.push((i + 2, (depth == 0).then(|| j - 1)));
            i = j;
            continue;
        }
        if bytes[i] == b'`' && (!scan.balanced || scan.allows_substitution(i)) {
            let close = command[i + 1..].find('`').map(|k| i + 1 + k);
            out.push((i + 1, close));
            i = close.map_or(bytes.len(), |c| c + 1);
            continue;
        }
        i += 1;
    }
    out
}

/// Offset of the first word that is neither a keyword, `(`, nor `KEY=value`.
fn command_start_offset(seg: &str) -> Option<usize> {
    words(seg).into_iter().find_map(|word| {
        let bare = word.text.trim_start_matches('(');
        let lead = word.text.len() - bare.len();
        (!(LEADING_KEYWORDS.contains(&word.text.as_str())
            || bare.is_empty()
            || is_env_assignment(bare)))
        .then_some(word.start + lead)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heavy() -> Vec<(String, Option<String>)> {
        trusty_mpm::core::build_lease::config::BuildLeaseConfig::default()
            .effective_heavy_build_commands()
    }

    fn rw(command: &str) -> LeaseRewrite {
        rewrite_for_lease(command, &heavy(), "tm build-lease --")
    }

    fn rewritten(command: &str) -> String {
        match rw(command) {
            LeaseRewrite::Rewrite(out) => out,
            other => panic!("{command:?} should be rewritten, got {other:?}"),
        }
    }

    #[test]
    fn a_plain_heavy_build_is_wrapped() {
        assert_eq!(
            rewritten("cargo test -p x --no-fail-fast"),
            "tm build-lease -- cargo test -p x --no-fail-fast"
        );
        assert_eq!(
            rewritten("/usr/local/bin/cargo +1.94 -C crates/x --locked build"),
            "tm build-lease -- /usr/local/bin/cargo +1.94 -C crates/x --locked build"
        );
    }

    /// The brief's composed-command cases, plus the shapes agents actually run.
    #[test]
    fn composed_commands_are_wrapped_in_place() {
        for (input, want) in [
            ("cd x && cargo test", "cd x && tm build-lease -- cargo test"),
            ("A=1 cargo test", "A=1 tm build-lease -- cargo test"),
            (
                "CARGO_TARGET_DIR=/t CARGO_BUILD_JOBS=6 cargo test -p y > /tmp/l 2>&1; echo EXIT=$?",
                "CARGO_TARGET_DIR=/t CARGO_BUILD_JOBS=6 tm build-lease -- cargo test -p y > /tmp/l 2>&1; echo EXIT=$?",
            ),
            (
                "cargo check && cargo clippy --all-targets",
                "tm build-lease -- cargo check && tm build-lease -- cargo clippy --all-targets",
            ),
            (
                "(cd x && cargo test)",
                "(cd x && tm build-lease -- cargo test)",
            ),
            ("(cargo test)", "(tm build-lease -- cargo test)"),
            ("{ cargo test; }", "{ tm build-lease -- cargo test; }"),
            (
                "timeout 600 cargo test",
                "timeout 600 tm build-lease -- cargo test",
            ),
            (
                "nice -n 10 cargo build",
                "nice -n 10 tm build-lease -- cargo build",
            ),
            (
                "for c in a b; do cargo test -p $c; done",
                "for c in a b; do tm build-lease -- cargo test -p $c; done",
            ),
            (
                "cargo test 2>&1 | tail -30",
                "tm build-lease -- cargo test 2>&1 | tail -30",
            ),
            ("cargo test &", "tm build-lease -- cargo test &"),
        ] {
            assert_eq!(rewritten(input), want, "{input}");
        }
    }

    #[test]
    fn quoted_data_is_not_a_build() {
        for input in [
            "git commit -m \"fix cargo test flake\"",
            "echo 'cargo build'",
            "grep -n 'cargo test' README.md",
        ] {
            assert_eq!(rw(input), LeaseRewrite::None, "{input}");
        }
    }

    #[test]
    fn nested_builds_are_leased() {
        for (input, want) in [
            ("sh -c 'cargo test'", "tm build-lease -- sh -c 'cargo test'"),
            (
                "A=1 bash -c \"cd x && cargo build\"",
                "A=1 tm build-lease -- bash -c \"cd x && cargo build\"",
            ),
            (
                "echo $(cargo build 2>&1 | tail -1)",
                "echo $(tm build-lease -- cargo build 2>&1 | tail -1)",
            ),
            ("x=`cargo check`", "x=`tm build-lease -- cargo check`"),
            (
                "ls | xargs cargo test",
                "ls | tm build-lease -- xargs cargo test",
            ),
            ("sh -c 'git status'", "sh -c 'git status'"),
        ] {
            let got = match rw(input) {
                LeaseRewrite::Rewrite(out) => out,
                LeaseRewrite::None => input.to_string(),
                LeaseRewrite::Refuse(r) => panic!("{input:?} refused: {r}"),
            };
            assert_eq!(got, want, "{input}");
        }
    }

    #[test]
    fn an_unterminated_substitution_is_refused() {
        match rw("echo $(cargo build") {
            LeaseRewrite::Refuse(reason) => assert!(reason.contains("tm build-lease"), "{reason}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert_eq!(
            rw("echo $(git status"),
            LeaseRewrite::None,
            "no heavy build, no refusal"
        );
    }

    #[test]
    fn an_already_leased_command_is_left_alone() {
        let once = rewritten("cd x && cargo test");
        assert_eq!(rw(&once), LeaseRewrite::None, "the rewrite is idempotent");
        assert_eq!(
            rewrite_for_lease("cargo test", &heavy(), "'/opt/my tools/tm' build-lease --"),
            LeaseRewrite::Rewrite("'/opt/my tools/tm' build-lease -- cargo test".into())
        );
    }

    #[test]
    fn light_cargo_verbs_are_left_alone() {
        for input in [
            "cargo fmt --check",
            "cargo metadata --no-deps",
            "cargo --version",
            "cargo tree -p x",
        ] {
            assert_eq!(rw(input), LeaseRewrite::None, "{input}");
        }
        let make_all = vec![("make".to_string(), None)];
        assert_eq!(
            rewrite_for_lease("make -j8", &make_all, "tm build-lease --"),
            LeaseRewrite::Rewrite("tm build-lease -- make -j8".into()),
            "a program-only entry leases every invocation"
        );
    }

    /// 2026-09-25 report (#8261): a heavy verb that compiles nothing — the
    /// read-only checks a `local-ops` dispatch runs — takes no build slot.
    #[test]
    fn non_compiling_invocations_of_heavy_verbs_are_left_alone() {
        for input in [
            "cargo install --list",
            "cargo install --list | grep trusty",
            "cargo build --help",
            "cargo test -h",
            "cargo clippy --version",
            "cargo +1.94 check -V",
            "sh -c 'cargo install --list'",
        ] {
            assert_eq!(rw(input), LeaseRewrite::None, "{input}");
        }
        // Past `--` the words belong to the built program: these compile.
        for input in [
            "cargo run -- --help",
            "cargo test -p x -- --list",
            "cargo install --path x",
        ] {
            assert_eq!(
                rw(input),
                LeaseRewrite::Rewrite(format!("tm build-lease -- {input}")),
                "{input}"
            );
        }
    }

    /// Critic round 1 (CRITICAL): a commit message is data, not a command.
    #[test]
    fn a_quoted_heredoc_body_is_never_rewritten() {
        for input in [
            "git commit -F - <<'EOF'\nfix(x): thing\n\nGate: `cargo test -p x --no-fail-fast` passed.\nEOF",
            "cat > notes.md <<'EOF'\nV=$(cargo build 2>&1)\ncargo test\nEOF",
        ] {
            assert_eq!(rw(input), LeaseRewrite::None, "{input}");
        }
    }

    #[test]
    fn an_unquoted_heredoc_body_is_never_rewritten() {
        for input in [
            "cat > notes.md <<EOF\nRun `cargo test -p x` first.\nEOF",
            "bash <<EOF\ncargo test\nEOF",
        ] {
            assert_eq!(rw(input), LeaseRewrite::None, "{input}");
        }
        // The live command around a body is still leased.
        assert_eq!(
            rw("cargo test <<EOF\ncargo build\nEOF"),
            LeaseRewrite::Rewrite("tm build-lease -- cargo test <<EOF\ncargo build\nEOF".into())
        );
    }

    #[test]
    fn an_unterminated_substitution_in_a_heredoc_is_not_refused() {
        assert_eq!(
            rw("cat > notes.md <<'EOF'\nsee $(cargo build\nEOF"),
            LeaseRewrite::None
        );
    }

    #[test]
    fn the_lease_goes_outside_sudo() {
        assert_eq!(
            rw("sudo cargo install --path x"),
            LeaseRewrite::Rewrite("tm build-lease -- sudo cargo install --path x".into())
        );
        assert_eq!(
            rw("A=1 doas cargo build"),
            LeaseRewrite::Rewrite("A=1 tm build-lease -- doas cargo build".into())
        );
    }

    #[test]
    fn cargo_aliases_and_plugins_are_heavy() {
        for input in [
            "cargo t",
            "cargo b --release",
            "cargo c",
            "cargo r",
            "cargo d",
            "cargo fix",
            "cargo rustc",
            "cargo rustdoc",
            "cargo publish",
            "cargo package",
            "cargo nextest run",
            "cargo llvm-cov",
            "cargo +nightly miri test",
        ] {
            assert_eq!(
                rw(input),
                LeaseRewrite::Rewrite(format!("tm build-lease -- {input}")),
                "{input}"
            );
        }
    }

    /// #8261 round 3 (critic finding 7): wrapper options that take a value,
    /// `find -exec`, `rustup run` and `cargo watch` — one table.
    #[test]
    fn wrappers_with_option_values_are_seen_through() {
        for (input, want) in [
            (
                "env -u NAME cargo test",
                "env -u NAME tm build-lease -- cargo test",
            ),
            (
                "sudo -u builder cargo build",
                "tm build-lease -- sudo -u builder cargo build",
            ),
            (
                "timeout -s KILL 600 cargo test",
                "timeout -s KILL 600 tm build-lease -- cargo test",
            ),
            (
                "timeout --signal=KILL 10m cargo check",
                "timeout --signal=KILL 10m tm build-lease -- cargo check",
            ),
            (
                "nice -n 5 env -C crates/x cargo clippy",
                "nice -n 5 env -C crates/x tm build-lease -- cargo clippy",
            ),
            (
                "find . -name Cargo.toml -execdir cargo test \\;",
                "tm build-lease -- find . -name Cargo.toml -execdir cargo test \\;",
            ),
            (
                "find crates -maxdepth 1 -exec cargo build --manifest-path {}/Cargo.toml +",
                "tm build-lease -- find crates -maxdepth 1 -exec cargo build --manifest-path {}/Cargo.toml +",
            ),
            (
                "rustup run stable cargo build",
                "tm build-lease -- rustup run stable cargo build",
            ),
            (
                "rustup run --install nightly cargo test -p x",
                "tm build-lease -- rustup run --install nightly cargo test -p x",
            ),
            (
                "cargo watch -x test",
                "tm build-lease -- cargo watch -x test",
            ),
        ] {
            assert_eq!(rewritten(input), want, "{input}");
        }
        for input in [
            "find . -name '*.rs' -exec grep -l cargo {} +",
            "env -u CARGO cargo fmt",
            "rustup run stable cargo --version",
        ] {
            assert_eq!(rw(input), LeaseRewrite::None, "{input}");
        }
    }

    /// #8261 round 3 (critic finding 10): `-V`/`--help` mean "no compile" for
    /// cargo only; `mvn -V package` prints its version AND builds.
    #[test]
    fn no_compile_flags_apply_to_cargo_only() {
        let mvn = vec![("mvn".to_string(), None)];
        assert_eq!(
            rewrite_for_lease("mvn -V package", &mvn, "tm build-lease --"),
            LeaseRewrite::Rewrite("tm build-lease -- mvn -V package".into())
        );
        assert_eq!(rw("cargo -V"), LeaseRewrite::None);
    }

    /// #8261 round 3 (critic finding 4): the lease runs only what the hook
    /// would have leased.
    #[test]
    fn a_lease_argv_is_heavy_only_when_the_classifier_says_so() {
        let argv = |s: &str| shlex::split(s).expect("argv");
        for heavy_argv in [
            "cargo test -p x",
            "sh -c 'cd x && cargo build'",
            "sudo cargo install --path x",
        ] {
            assert!(is_heavy_build(&argv(heavy_argv), &heavy()), "{heavy_argv}");
        }
        for light in [
            "rm -f /tmp/x",
            "sh -c 'rm -rf ~'",
            "cargo fmt",
            "cargo --version",
            "echo cargo test",
        ] {
            assert!(!is_heavy_build(&argv(light), &heavy()), "{light}");
        }
    }

    #[test]
    fn a_cargo_inside_a_case_arm_is_leased() {
        assert_eq!(
            rw("case $x in a) cargo test ;; *) cargo build ;; esac"),
            LeaseRewrite::Rewrite(
                "case $x in a) tm build-lease -- cargo test ;; *) tm build-lease -- cargo build ;; esac"
                    .into()
            )
        );
    }
}
