//! Unit tests for the #9001 ANSI-C decoder (`ansi_c_decode.rs`).

use super::*;

fn text(command: &str) -> String {
    match decode_ansi_c(command) {
        Decoded::Text(t) => t,
        other => panic!("{command}: {other:?}"),
    }
}

/// Each supported escape decodes to the byte bash produces, re-quoted.
#[test]
fn decodes_each_supported_escape() {
    assert_eq!(text(r"grep -c $'\x1b' log"), "grep -c '\u{1b}' log");
    assert_eq!(text(r"$'\x74mux' kill-server"), "'tmux' kill-server");
    assert_eq!(text(r"$'\164mux' ls"), "'tmux' ls");
    assert_eq!(text(r"echo $'a\tb\e'"), "echo 'a\tb\u{1b}'");
    assert_eq!(text(r"echo $'it\'s'"), r"echo 'it'\''s'");
    assert_eq!(text(r"echo x$'\x2eenv'y"), "echo x'.env'y");
    assert_eq!(decode_ansi_c("grep -c 'x' log"), Decoded::Plain);
}

/// What bash decodes differently, or to a byte that moves segment cuts,
/// stays undecodable and is named.
#[test]
fn refuses_what_it_cannot_decode() {
    for (command, token) in [
        (r"echo $'tmux'", r"$'tmux'"),
        (r"echo $'\cA'", r"$'\cA'"),
        (r"echo $'\q'", r"$'\q'"),
        (r"echo $'\0'", r"$'\0'"),
        (r"echo $'\xff'", r"$'\xff'"),
        (r"echo $'a\nb'", r"$'a\nb'"),
        (r#"echo $"x""#, r#"$"x""#),
        (r"echo $'open", r"$'open"),
    ] {
        assert_eq!(
            decode_ansi_c(command),
            Decoded::Undecodable(token.into()),
            "{command}"
        );
    }
}

/// A `$'` inside quotes, after a backslash, or after `$$` is no quote.
#[test]
fn leaves_a_quoted_dollar_quote_alone() {
    for command in [r#"echo "cost $'5'""#, r"echo '$'", r"echo \$'x'"] {
        assert_eq!(decode_ansi_c(command), Decoded::Plain, "{command}");
    }
    assert_eq!(text(r"echo $$ $'\x41'"), "echo $$ 'A'");
}

/// The #6660 refusal names the token; other unclassifiable reasons are
/// unchanged; a classifiable command has none.
#[test]
fn the_refusal_names_the_token_it_cannot_decode() {
    let reason = unclassifiable_reason(r"grep -c $'\x1b' log").expect("refused");
    assert!(reason.starts_with(ANSI_C_QUOTING_REASON), "{reason}");
    assert!(reason.contains(r"`$'\x1b'`"), "{reason}");
    assert!(reason.contains("printf"), "{reason}");
    let reason = unclassifiable_reason(r"echo $'t'").expect("refused");
    assert!(reason.contains(r"`$'t'`"), "{reason}");
    assert_eq!(unclassifiable_reason("grep -c x log"), None);
    let wrapper = "sh -c \"git worktree remove 'unterminated\"";
    assert_eq!(
        unclassifiable_reason(wrapper).as_deref(),
        unclassifiable_command(wrapper)
    );
}
