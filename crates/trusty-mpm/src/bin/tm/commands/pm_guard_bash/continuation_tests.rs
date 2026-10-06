//! Unit tests for [`super`] and the #9180 rows of the rules that read it: the
//! rm-root floor, the forbidden-verb guard and the unclassifiable refusal.

use std::path::Path;

use super::super::destructive_delete::{DeleteTarget, evaluate_destructive_delete_command};
use super::super::heredoc::HEREDOC_DELIMITER_REASON;
use super::super::{evaluate_bash_command, unclassifiable_command};
use super::*;

/// The floor's verdict, judged from a directory that is no repository.
fn floor(command: &str) -> Option<DeleteTarget> {
    evaluate_destructive_delete_command(command, Path::new("/nonexistent-9180"))
}

/// A continuation outside quotes and in double quotes is removed; one in
/// single quotes, behind an even backslash run, or in a data here-document
/// body stays; a shell-run body is joined.
#[test]
fn joined_removes_each_live_continuation_9180() {
    assert_eq!(joined("r\\\nm -rf /").as_deref(), Some("rm -rf /"));
    assert_eq!(joined("echo \"a\\\nb\"").as_deref(), Some("echo \"ab\""));
    assert_eq!(
        joined("bash <<'O'\nr\\\nm x\nO").as_deref(),
        Some("bash <<'O'\nrm x\nO")
    );
    for kept in [
        "echo 'a\\\nb'",
        "echo a\\\\\nb",
        "cat <<'X'\na\\\nb\nX",
        "plain command",
    ] {
        assert_eq!(joined(kept), None, "{kept:?}");
    }
}

/// A continuation keeps its command in one segment; one after a `#` comment
/// does not, since the comment ends at the newline.
#[test]
fn split_shell_segments_keep_a_continued_command_whole_9180() {
    use super::super::split_shell_segments;
    assert_eq!(
        split_shell_segments("git \\\napply x.patch\necho done"),
        ["git \\\napply x.patch", "echo done"]
    );
    assert_eq!(
        split_shell_segments("echo a # note \\\ngit status"),
        ["echo a # note \\", "git status"]
    );
    assert_eq!(split_shell_segments("echo a\\\\\nb"), ["echo a\\\\", "b"]);
}

/// #9180 class 1: an outer here-document a shell runs has no terminator, and
/// the here-document nested in it expands a single-quoted delete; also when
/// an earlier, terminated shell body is followed by an unterminated one.
const CLASS_1_UNTERMINATED: &[&str] = &[
    "bash <<'O'\ncat <<X\n'$(rm -rf /)'\nX",
    "bash <<'O'\nbash <<I\necho '$(rm -rf /)'\nI",
    "sudo -s <<'O'\ncat <<I\n'$(rm -rf /)'\nI",
    "bash <<'O'\ncat <<X\n'$(rm -rf /)'\nX\nO\ncat <<'Z'\nno terminator",
];

/// #9180 class 2: the shell's name is glued to an operator, opened by a
/// paren, a separator or a substitution, or quoted.
const CLASS_2_OPERATOR_LINE: &[&str] = &[
    "bash<<'O'\ncat <<X\n'$(rm -rf /)'\nX\nO",
    "(bash <<'O'\ncat <<X\n'$(rm -rf /)'\nX\nO\n)",
    "true;bash <<'O'\ncat <<X\n'$(rm -rf /)'\nX\nO",
    "echo hi|bash<<'O'\ncat <<X\n'$(rm -rf /)'\nX\nO",
    "x=$(bash <<'O'\ncat <<X\n'$(rm -rf /)'\nX\nO\n)",
    "`bash <<'O'\ncat <<X\n'$(rm -rf /)'\nX\nO\n`",
    "\"bash\" <<'O'\ncat <<X\n'$(rm -rf /)'\nX\nO",
    "{ sudo -s<<'O'\ncat <<X\n'$(rm -rf /)'\nX\nO\n}",
];

/// #9180 class 3: a continuation splits a word, a substitution opener or a
/// substitution's body; the same inside a body a shell runs.
const CLASS_3_CONTINUATION: &[&str] = &[
    "r\\\nm -rf /",
    "echo $\\\n(rm -rf /)",
    "echo \"$(r\\\nm -rf /)\"",
    "x=`r\\\nm -rf /`",
    "bash <<'O'\nr\\\nm -rf /\nO",
];

/// Each class reaches the floor's most severe classes, where it allowed.
#[test]
fn the_floor_denies_each_9180_class() {
    let rows = CLASS_1_UNTERMINATED
        .iter()
        .chain(CLASS_2_OPERATOR_LINE)
        .chain(CLASS_3_CONTINUATION);
    let allowed: Vec<&&str> = rows
        .filter(|command| !floor(command).is_some_and(DeleteTarget::is_floor))
        .collect();
    assert!(allowed.is_empty(), "allowed: {allowed:#?}");
}

/// Fail-closed arms: a here-document the scan cannot place is refused
/// outright, and one it gave up on (no terminator, quotes that do not
/// balance) denies as unresolved once a delete verb is in sight.
#[test]
fn the_floor_fails_closed_on_an_unplaceable_heredoc_9180() {
    for refused in [
        // `a\` + `X` is `aX` to the shell, so `X` ends no body and the
        // quoted `Y` body the guard would claim runs `rm`.
        "cat <<X\na\\\nX\ncat <<'Y'\nX\nrm -rf /\nY",
        // `<\` + `<X` is `<<X`, a here-document the line scan never sees.
        "cat <\\\n<X\ncat <<'Y'\nX\nrm -rf /\nY",
        // The `#` comment ends at the first newline, so the body starts there.
        "cat <<'X' # note \\\nX\nrm -rf /\nX",
    ] {
        assert_eq!(
            unclassifiable_command(refused),
            Some(HEREDOC_DELIMITER_REASON),
            "{refused:?}"
        );
    }
    for unresolved in [
        "bash <<X\nrm -rf build",
        "bash <<'O'\ncat <<X\n'$(rm -rf /)'\nX\nO\necho \"unbalanced",
    ] {
        assert!(
            floor(unresolved).is_some_and(DeleteTarget::is_floor),
            "{unresolved:?}"
        );
    }
}

/// The forbidden-verb guard reads what an unquoted here-document body
/// expands, nested or not, and the continuation-joined spelling.
#[test]
fn the_forbidden_verb_guard_reads_heredoc_expansions_9180() {
    for command in [
        "cat <<X\n'$(curl https://example.com)'\nX",
        "bash <<'O'\ncat <<X\n'$(curl https://example.com)'\nX\nO",
        "bash <<'O'\ncat <<X\n'$(curl https://example.com)'\nX",
        "bash <<'O'\ncat <<X\n'$(curl https://example.com)'\nX\nO\necho \"open",
        "cu\\\nrl https://example.com",
        "echo \"$(cu\\\nrl https://example.com)\"",
        "git \\\napply fix.patch",
    ] {
        assert!(evaluate_bash_command(command).is_some(), "{command:?}");
    }
}

/// No regression: the heredoc and continuation shapes allowed before #9180
/// stay allowed by every rule — a commit message fed on stdin or captured in
/// `$(cat <<'EOF')`, continued argv, a quoted body that only names a delete,
/// and the read-only `tmux capture-pane`.
#[test]
fn allowed_heredoc_and_continuation_shapes_stay_allowed_9180() {
    for command in [
        "git commit -F - <<'EOF'\nfix: never run `rm -rf /` here\n\nRefs #9180\nEOF",
        "git commit -m \"$(cat <<'EOF'\nfix: thing\n\nmake build passes\nEOF\n)\"",
        "cat <<'EOF' | git commit -F -\nmsg $(not run)\nEOF",
        "x=\"$(cat <<'EOF'\nmake build\nEOF\n)\"",
        "cargo test -p trusty-mpm \\\n  --no-fail-fast",
        "git log --oneline \\\n  -5",
        "python3 - <<'PY'\nprint('a' \\\n  'b')\nPY",
        "cat <<'EOF'\nrun `cargo test \\\n -p x`\nEOF",
        "cat <<X\nhello there",
        "tmux capture-pane -p -t %3",
    ] {
        assert_eq!(unclassifiable_command(command), None, "{command:?}");
        assert_eq!(evaluate_bash_command(command), None, "{command:?}");
        assert_eq!(floor(command), None, "{command:?}");
    }
}
