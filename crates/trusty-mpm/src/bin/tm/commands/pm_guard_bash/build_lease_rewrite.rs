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
//! inside `sh -c '…'`, `bash -c`, `env -S` or `xargs` leases the whole wrapper.
//! Quoted DATA is never matched: `git commit -m "fix cargo test"` is not a build.
//! Test: the `#[cfg(test)]` suite below.

use super::heredoc::HeredocBodies;
use super::shell_lex::{QuoteScan, WrappedCommand, wrapped_command};
use super::{paren_substitution_live_at, split_shell_segments, split_shell_segments_raw};
use crate::commands::hook_rewrite::{COMMAND_WRAPPERS, is_env_assignment};

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

/// Shell words that may precede a command without being it.
const LEADING_KEYWORDS: &[&str] = &[
    "!", "{", "(", "if", "then", "else", "elif", "do", "while", "until",
];

/// Cargo global options that take a value before the subcommand.
const CARGO_VALUE_FLAGS: &[&str] = &["-C", "-Z", "--config", "--color"];

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
            match wrapped_command(seg.trim()) {
                WrappedCommand::Inner(inner) if hides_a_heavy_build(&inner, heavy) => {
                    command_start_offset(seg).map(|off| seg_start + off)
                }
                _ => None,
            }
        })
        .collect()
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

/// One word of a segment: its byte span and its unquoted text.
struct Word {
    start: usize,
    text: String,
}

/// Split a segment into words, honouring quotes and backslashes.
fn words(seg: &str) -> Vec<Word> {
    let bytes = seg.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let start = i;
        let mut quote: Option<u8> = None;
        while i < bytes.len() {
            let b = bytes[i];
            match quote {
                Some(q) if b == q => quote = None,
                Some(b'"') if b == b'\\' => i += 1,
                Some(_) => {}
                None if b.is_ascii_whitespace() => break,
                None if b == b'\'' || b == b'"' => quote = Some(b),
                None if b == b'\\' => i += 1,
                None => {}
            }
            i += 1;
        }
        let raw = &seg[start..i.min(bytes.len())];
        let text = shlex::split(raw)
            .map(|parts| parts.concat())
            .unwrap_or_else(|| raw.to_string());
        out.push(Word { start, text });
    }
    out
}

/// The byte offset of the heavy program word in `seg`, if `seg` is a heavy build.
///
/// What: skips leading keywords and `(`, env assignments, and command wrappers
/// with their flags and numeric arguments (`timeout 600`, `nice -n 10`); then
/// matches the program's basename and, past `+toolchain` and cargo's global
/// options, its subcommand.
fn heavy_program_offset(seg: &str, heavy: &[(String, Option<String>)]) -> Option<usize> {
    let words = words(seg);
    let mut idx = 0;
    let mut after_wrapper = false;
    // #8261 critic round 1 (LOW): the lease goes OUTSIDE `sudo`/`doas`, so it
    // runs as the caller and locks the caller's slot directory.
    let mut outside: Option<usize> = None;
    while let Some(word) = words.get(idx) {
        let text = word.text.as_str();
        let bare = text.trim_start_matches('(');
        let lead = text.len() - bare.len();
        if text == "case" {
            // `case WORD in` — the pattern label that follows is skipped below.
            idx = words
                .iter()
                .skip(idx)
                .position(|w| w.text == "in")
                .map_or(words.len(), |p| idx + p + 1);
            continue;
        }
        if LEADING_KEYWORDS.contains(&text) || bare.is_empty() || is_case_label(bare) {
            idx += 1;
            continue;
        }
        if is_env_assignment(bare) || COMMAND_WRAPPERS.contains(&bare) {
            if (bare == "sudo" || bare == "doas") && outside.is_none() {
                outside = Some(word.start + lead);
            }
            after_wrapper |= COMMAND_WRAPPERS.contains(&bare);
            idx += 1;
            continue;
        }
        if after_wrapper
            && (bare.starts_with('-') || bare.parse::<f64>().is_ok() || is_duration(bare))
        {
            idx += 1;
            continue;
        }
        break;
    }
    let word = words.get(idx)?;
    let lead = word.text.len() - word.text.trim_start_matches('(').len();
    let program = word.text.trim_start_matches('(');
    let program = program.rsplit('/').next().unwrap_or(program);
    let sub = subcommand(&words[idx + 1..]);
    let is_heavy = heavy
        .iter()
        .any(|(p, s)| p == program && s.as_deref().is_none_or(|s| Some(s) == sub.as_deref()));
    (is_heavy && !invokes_no_compiler(sub.as_deref(), &words[idx + 1..]))
        .then_some(outside.unwrap_or(word.start + lead))
}

/// Flags that make any heavy verb print and exit without compiling.
const NO_COMPILE_FLAGS: &[&str] = &["--help", "-h", "--version", "-V"];

/// Whether a heavy verb's own arguments make it run no compiler (#8261).
///
/// Why: admission follows whether the command compiles, never the agent's
/// role (2026-09-25 report: a read-only `local-ops` dispatch refused by the
/// builder cap). `cargo build --help` or `cargo install --list` compiles
/// nothing, so it must not wait for — or be refused — a build slot.
/// What: `true` when a [`NO_COMPILE_FLAGS`] word, or `--list` after
/// `install`, appears before any `--` (past `--`, words belong to the built
/// program: `cargo run -- --help` still compiles).
/// Test: `non_compiling_invocations_of_heavy_verbs_are_left_alone`.
fn invokes_no_compiler(sub: Option<&str>, rest: &[Word]) -> bool {
    rest.iter()
        .map(|w| w.text.as_str())
        .take_while(|w| *w != "--")
        .any(|w| NO_COMPILE_FLAGS.contains(&w) || (sub == Some("install") && w == "--list"))
}

/// A `case` arm's pattern label: `a)`, `(a)`, `*)`, `x|y)`.
fn is_case_label(word: &str) -> bool {
    word.ends_with(')') && !word.starts_with('$')
}

/// The subcommand after a program word, skipping `+toolchain` and global options.
fn subcommand(rest: &[Word]) -> Option<String> {
    let mut iter = rest.iter();
    while let Some(word) = iter.next() {
        let text = word.text.as_str();
        if text.starts_with('+') {
            continue;
        }
        if CARGO_VALUE_FLAGS.contains(&text) {
            iter.next();
            continue;
        }
        if text.starts_with('-') {
            continue;
        }
        return Some(text.trim_end_matches([')', ';', '}']).to_string());
    }
    None
}

/// `30s`, `5m`, `1h` — a `timeout` duration.
fn is_duration(word: &str) -> bool {
    word.strip_suffix(['s', 'm', 'h', 'd'])
        .is_some_and(|n| n.parse::<f64>().is_ok())
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
            rewrite_for_lease("make --help", &make_all, "tm build-lease --"),
            LeaseRewrite::None,
            "a help flag compiles nothing, whatever the program"
        );
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
