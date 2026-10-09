//! #9360: a prefix-assignment value substitution the scan opened but bash
//! never did (a quoted or escaped `(`, `$(` or backtick) must not leave a
//! shell-run here-document body trusted as data. Rows for
//! [`super::line_reads_as_data`], the rm-root floor and the no-false-deny
//! commit shapes, beside [`super`]'s operator-line predicates.

use std::path::Path;

use super::super::destructive_delete::{DeleteTarget, evaluate_destructive_delete_command};
use super::super::unclassifiable_command;
use super::*;

/// The floor's verdict, judged from a directory that is no repository.
fn floor(command: &str) -> Option<DeleteTarget> {
    evaluate_destructive_delete_command(command, Path::new("/nonexistent-9360"))
}

/// Operator lines whose body bash runs through `.`, each checked against
/// bash 5: the five #9360 shapes, then a redirect ahead of the quoted paren
/// and an escaped or quoted backtick.
const UNCLOSED: &[&str] = &[
    "X=$(echo '(' cat) . /dev/stdin <<'O'",
    "X=$(echo \"(\" true) . /dev/stdin <<'O'",
    "X=$(echo \\( cat) . /dev/stdin <<'O'",
    "X='$(true' . /dev/stdin <<'O'",
    "X=\\$\\(true . /dev/stdin <<'O'",
    "X=$(cat <f '(' x) . /dev/stdin <<'O'",
    "X=\\`true . /dev/stdin <<'O'",
    "X='\\`true' . /dev/stdin <<'O'",
];

/// Bodies the floor must judge as commands once the line is shell-run.
const PAYLOADS: &[&str] = &["cd ~; rm -rf .", "rm -rf ~"];

/// #9360: an open value substitution with a quote or `\` the scan cannot
/// place is no data line.
#[test]
fn unclosed_quoted_value_substitution_lines_are_not_data_9360() {
    for line in UNCLOSED {
        assert!(!line_reads_as_data(line), "{line:?}");
    }
}

/// #9360: their bodies reach the rm-root floor.
#[test]
fn unclosed_quoted_value_substitution_bodies_reach_the_floor_9360() {
    let mut allowed = Vec::new();
    for line in UNCLOSED {
        for payload in PAYLOADS {
            let command = format!("{line}\n{payload}\nO");
            if !floor(&command).is_some_and(DeleteTarget::is_floor) {
                allowed.push(command);
            }
        }
    }
    assert!(allowed.is_empty(), "allowed: {allowed:#?}");
}

/// #9360: an open value substitution whose only quotes are the delimiter's
/// own, or a `"` before `$(`, stays data, and the commit-message flow built
/// on it stays clear of the floor and the unclassifiable refusal.
#[test]
fn delimiter_quoted_value_substitutions_stay_data_9360() {
    for line in [
        "msg=$(cat <<'EOF'",
        "msg=$(cat <<\"EOF\"",
        "msg=$(cat <<\\EOF",
        "msg=$(cat <<-'EOF'",
        "msg=$(cat << 'EOF'",
        "msg=\"$(cat <<'EOF'",
        "x=`cat <<'EOF'",
    ] {
        assert!(line_reads_as_data(line), "{line:?}");
    }
    let command = "msg=$(cat <<'EOF'\nfix: rm stale files\n\nThe floor denies `rm -rf ~`.\nEOF\n)\ngit commit -m \"$msg\"";
    assert_eq!(unclassifiable_command(command), None, "{command:?}");
    assert_eq!(floor(command), None, "{command:?}");
}

/// Operator lines whose prefix assignment holds a `(` or `)` inside a
/// `${…}` parameter expansion, which opens or closes no substitution in
/// bash; each runs its body through `.` under bash 5 (#9360 critic round).
const PARAMETER_PARENS: &[&str] = &[
    "X=${x:-(} . /dev/stdin <<'O'",
    "X=${x/(/} . /dev/stdin <<'O'",
    "X=${x:-( cat } . /dev/stdin <<'O'",
    "X=${x/(/ cat } . /dev/stdin <<'O'",
    "X=$(echo ${y:-) cat }) . /dev/stdin <<'O'",
    // #9360 delta critic: a quoted or escaped `}` does not close the expansion.
    "X=${x:-\"}\"( cat } . /dev/stdin <<'O'",
    "X=${x:-'}'( cat } . /dev/stdin <<'O'",
    "X=${x:-\\}( cat } . /dev/stdin <<'O'",
];

/// #9360 critic round: a paren inside `${…}` neither opens nor closes a
/// substitution, so the line is no data line and its body reaches the floor.
#[test]
fn parameter_expansion_parens_reach_the_floor_9360() {
    let mut allowed = Vec::new();
    for line in PARAMETER_PARENS {
        if line_reads_as_data(line) {
            allowed.push(line.to_string());
        }
        for payload in PAYLOADS {
            let command = format!("{line}\n{payload}\nO");
            if !floor(&command).is_some_and(DeleteTarget::is_floor) {
                allowed.push(command);
            }
        }
    }
    assert!(allowed.is_empty(), "allowed: {allowed:#?}");
}

/// #9360 critic round: a parameter expansion with no paren in it keeps a
/// reader line data.
#[test]
fn plain_parameter_expansions_stay_data_9360() {
    for line in [
        "msg=${x:-default} cat <<'EOF'",
        "cat > \"${OUT:-f}\" <<'EOF'",
    ] {
        assert!(line_reads_as_data(line), "{line:?}");
    }
}
