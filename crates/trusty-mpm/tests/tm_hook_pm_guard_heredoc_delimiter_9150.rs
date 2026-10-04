//! End-to-end proof that a quoted here-document delimiter holding a word-break
//! byte cannot hide a substitution from the guard (#9150).
//!
//! Why: the guard's here-document scanner cut `<<'A B'` at the space and
//! ended the body at a later `A` line, while bash and zsh keep the whole
//! quoted word and end the body at `A B`. The `$(rm -rf /)` between the two
//! terminators then read as quoted body text to the #8735 substitution scan,
//! but runs in the shell, so the #8878 hard floor allowed it.
//! What: spawns `tm hook --pm-guard` through the #8878 fixture and checks that
//! every shape bash and zsh delimit differently from the old scanner denies,
//! under each bypass, while a correctly delimited quoted body that only holds
//! `$(…)` text still allows. Round 2 adds delimiter words outside the
//! `[A-Za-z0-9_.-]` allowlist and a `<<` the shell never reads as an operator
//! (comment, `\<<`, `${…}`) or may not (arithmetic, a same-line `$(…)`). The
//! shell behaviour each row relies on was probed with `/bin/bash` 3.2 and
//! `/bin/zsh` 5.9 (recorded on #9150); every row allowed on 656136c7dc.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_heredoc_delimiter_9150::`.

use crate::tm_hook_pm_guard_trust_anchor_8878::{Fixture, bash_payload, run};

/// The bypass variables, and `None` for no bypass.
const BYPASSES: [Option<(&str, &str)>; 3] = [
    None,
    Some(("TRUSTY_MPM_PM_UNRESTRICTED", "1")),
    Some(("TRUSTY_MPM_DISABLE_HOOKS", "1")),
];

/// Shapes the shell delimits at the whole quoted word, so the `$(…)` after
/// the real terminator runs. Each allowed before the #9150 fix.
const HIDDEN_SUBSTITUTIONS: [&str; 31] = [
    // The #9150 report.
    "cat <<'A B'\nx\nA B\necho \"$(rm -rf /)\"\nA",
    "cat <<'A>B'\nx\nA>B\necho \"$(rm -rf /)\"\nA",
    "cat <<'A<B'\nx\nA<B\necho \"$(rm -rf /)\"\nA",
    "cat <<-'A B'\n\tx\n\tA B\necho \"$(rm -rf /)\"\n\tA",
    "cat <<'A B'\nx\nA B\necho \"$(rm -rf $HOME)\"\nA",
    "cat <<'A B'\nx\nA B\necho \"$(rm -rf ~)\"\nA",
    "cat <<'A\tB'\nx\nA\tB\necho \"$(rm -rf /)\"\nA",
    "cat <<\"A B\"\nx\nA B\necho \"$(rm -rf /)\"\nA",
    "cat <<A\\ B\nx\nA B\necho \"$(rm -rf /)\"\nA",
    // A trailing `\` continues the line: the shell's word is the unquoted
    // `AB`, so the body expands and its `$(…)` runs.
    "cat <<A\\\nB\n$(rm -rf /)\nA\nAB",
    // Round 2, item 1: a substitution or glob group in the word.
    "cat <<'A'$(x)\nx\nA$(x)\necho \"$(rm -rf /)\"\nA$",
    "cat <<'A'$((1 + 2))\nA$((1 + 2))\necho \"$(rm -rf /)\"\nA$",
    "cat <<'A'$[1 + 2]\nx\nA$[1 + 2]\necho \"$(rm -rf /)\"\nA$[1",
    "cat <<'A'(x y)\nx\nA(x y)\necho \"$(rm -rf /)\"\nA",
    "cat <<'A'<(x)\nx\nA<(x)\necho \"$(rm -rf /)\"\nA",
    "cat <<'A'>(x)\nx\nA>(x)\necho \"$(rm -rf /)\"\nA",
    // Round 2, item 2: a `\` or quote the shell keeps in the word. The
    // trailing `: '` rebalances the quotes so the old scan claimed the body.
    "cat <<'A\\B'\nx\nA\\B\necho \"$(rm -rf /)\"\nAB",
    "cat <<\"A\\B\"\nx\nA\\B\necho \"$(rm -rf /)\"\nAB",
    "cat <<A\\\\B\nx\nA\\B\necho \"$(rm -rf /)\"\nAB",
    "cat <<\"A'B\"\nx\nA'B\necho \"$(rm -rf /)\"\nAB\n: '",
    "cat <<'A\"B'\nx\nA\"B\necho \"$(rm -rf /)\"\nAB\n: \"",
    "cat <<A\\'B\nx\nA'B\necho \"$(rm -rf /)\"\nAB\n: '",
    "cat <<A\\\"B\nx\nA\"B\necho \"$(rm -rf /)\"\nAB\n: \"",
    // Round 2, item 3: a `<<` that opens no body, so the next line runs.
    "echo hi # <<'EOF'\necho \"$(rm -rf /)\"\nEOF",
    "echo $((1<<\"2\"))\necho \"$(rm -rf /)\"\n2",
    "(( x = 1 <<\\2 ))\necho \"$(rm -rf /)\"\n2",
    "echo ${x:-<<'EOF'}\necho \"$(rm -rf /)\"\nEOF}",
    "echo \\<<'EOF'\necho \"$(rm -rf /)\"\nEOF",
    "x=$(cat <<'EOF')\necho \"$(rm -rf /)\"\nEOF",
    // Round 3: `A\r` does not end an `A` body, and a CRLF terminator ends
    // only a CRLF word. The first row allowed on a753c016c9.
    "cat <<'A'\nx\nA\r\ncat <<'X'\nA\necho \"$(rm -rf /)\"\nX",
    "cat <<'EOF'\r\nx\r\nEOF\r\necho \"$(rm -rf /)\"\r\n",
];

/// #9150: every shape above denies, under each bypass.
#[test]
fn a_quoted_heredoc_delimiter_with_a_word_break_denies() {
    let fx = Fixture::new();
    let mut allowed = Vec::new();
    for bypass in BYPASSES {
        let env: Vec<(&str, &str)> = bypass.into_iter().collect();
        for command in HIDDEN_SUBSTITUTIONS {
            let out = run(&fx, &bash_payload(&fx, command), &env, Some("pm"));
            if !out.contains("\"deny\"") {
                allowed.push(format!("{bypass:?} {command:?}: {out:?}"));
            }
        }
    }
    assert!(allowed.is_empty(), "allowed:\n{}", allowed.join("\n"));
}

/// #9150 no-false-deny: a correctly delimited quoted body is data, so `$(…)`
/// text in it stays inert and a benign command after it still allows; so do
/// an unquoted body, a plain comment, arithmetic and a multi-line `$(…)`.
#[test]
fn a_plain_quoted_heredoc_with_substitution_text_still_allows() {
    let fx = Fixture::new();
    for command in [
        "cat <<'EOF'\n$(rm -rf /)\nEOF\necho benign",
        "cat <<-'EOF'\n\t$(rm -rf /)\n\tEOF\necho benign",
        "cat <<EOF\nhello\nEOF\necho benign",
        "cat <<-EOF\n\thello\n\tEOF\necho benign",
        "# list files\nls -la",
        "cat <<'EOF' # note\n$(rm -rf /)\nEOF\necho benign",
        "x=$(cat <<'EOF'\n$(rm -rf /)\nEOF\n)\necho \"$x\"",
        "(cat <<'EOF')\n$(rm -rf /)\nEOF\necho benign",
        "echo $((1 << 3))\necho benign",
        "cat <<'END_OF-file.1'\n$(rm -rf /)\nEND_OF-file.1\necho benign",
        // Round 3: a CRLF body is data like its LF form (denied on a753c016c9).
        "cat <<'EOF'\r\n$(rm -rf /)\r\nEOF\r\necho benign\r\n",
    ] {
        let out = run(&fx, &bash_payload(&fx, command), &[], Some("pm"));
        assert!(!out.contains("\"deny\""), "{command:?}: {out}");
    }
}

/// #7833: `cat` writing a quoted body to a file is data, but only behind a
/// delimiter the #9150 scan reads. A lone `cat > out.txt` behind a refused
/// delimiter still denies, under each bypass, so the #7833 allowance cannot
/// reopen #9150.
#[test]
fn a_cat_write_behind_a_refused_delimiter_denies_7833() {
    let fx = Fixture::new();
    for bypass in BYPASSES {
        let env: Vec<(&str, &str)> = bypass.into_iter().collect();
        for command in [
            "cat > out.txt <<'A B'\nx\nA B\n$(rm -rf /)\nA",
            "cat > out.txt <<'A B'\nx\nA B\necho \"$(rm -rf /)\"\nA",
        ] {
            let out = run(&fx, &bash_payload(&fx, command), &env, Some("pm"));
            assert!(out.contains("\"deny\""), "{bypass:?} {command:?}: {out}");
        }
    }
}

/// #9150 round 3: a delimiter word the scanner refuses denies under each
/// bypass even when its "body" holds a credential-printing command, because
/// the unclassifiable refusal runs before the credential-print rule and the
/// walk behind that rule strips no body for such a word.
#[test]
fn a_refused_delimiter_denies_a_credential_command_in_its_body() {
    let fx = Fixture::new();
    for bypass in BYPASSES {
        let env: Vec<(&str, &str)> = bypass.into_iter().collect();
        for command in [
            "cat <<'x$y'\ngcloud auth print-access-token\nx$y",
            "cat <<x$y\ngcloud auth print-access-token\nx$y",
        ] {
            let out = run(&fx, &bash_payload(&fx, command), &env, Some("pm"));
            assert!(out.contains("\"deny\""), "{bypass:?} {command:?}: {out}");
        }
    }
}
