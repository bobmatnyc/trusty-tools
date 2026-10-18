//! Tests for `inside_claude` (#10033): the relaunch refusal inside a Claude
//! Code session, over an injected environment.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;

use super::{Reason, SESSION_MARKERS, present_markers, refuse_inside_claude_code};

/// An environment holding exactly `vars`.
fn env_of(vars: Vec<(&'static str, OsString)>) -> impl Fn(&str) -> Option<OsString> {
    move |name| {
        vars.iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.clone())
    }
}

/// The values a Claude Code Bash tool child inherits.
fn claude_code_env() -> Vec<(&'static str, OsString)> {
    vec![
        ("CLAUDECODE", "1".into()),
        (
            "CLAUDE_CODE_SESSION_ID",
            "0b6f5c1e-secret-session-id".into(),
        ),
        ("CLAUDE_CODE_CHILD_SESSION", "1".into()),
        ("CLAUDE_PID", "4242".into()),
    ]
}

#[test]
fn session_markers_refuse_and_name_no_value() {
    let present = present_markers(env_of(claude_code_env()));
    let expected: Vec<_> = SESSION_MARKERS
        .iter()
        .map(|&n| (n, Reason::Session))
        .collect();
    assert_eq!(present, expected);

    // One marker alone refuses: `unset` of the others is no bypass.
    for (name, value) in claude_code_env() {
        let err = refuse_inside_claude_code(env_of(vec![(name, value)]))
            .expect_err("a single marker refuses")
            .to_string();
        assert!(
            err.contains(&format!("`{name}` is set, as Claude Code")),
            "{err}"
        );
        assert!(err.contains("operator terminal"), "{err}");
    }
    let err = refuse_inside_claude_code(env_of(claude_code_env()))
        .expect_err("the full marker set refuses")
        .to_string();
    for value in ["secret-session-id", "4242"] {
        assert!(!err.contains(value), "a marker value leaked: {err}");
    }
}

#[test]
fn an_unreadable_or_ambiguous_marker_refuses_and_names_the_reason() {
    let unreadable = OsString::from_vec(vec![0xff, 0xfe]);
    let cases: Vec<(&'static str, OsString, Reason, &str)> = vec![
        (
            "CLAUDECODE",
            unreadable.clone(),
            Reason::Unreadable,
            "not UTF-8",
        ),
        ("CLAUDE_PID", unreadable, Reason::Unreadable, "not UTF-8"),
        ("CLAUDECODE", "".into(), Reason::Ambiguous, "ambiguous"),
        ("CLAUDECODE", "0".into(), Reason::Ambiguous, "ambiguous"),
        (
            "CLAUDE_CODE_CHILD_SESSION",
            "yes".into(),
            Reason::Ambiguous,
            "ambiguous",
        ),
        (
            "CLAUDE_PID",
            "not-a-pid".into(),
            Reason::Ambiguous,
            "ambiguous",
        ),
        ("CLAUDE_PID", "0".into(), Reason::Ambiguous, "ambiguous"),
        (
            "CLAUDE_CODE_SESSION_ID",
            " ".into(),
            Reason::Ambiguous,
            "ambiguous",
        ),
    ];
    for (name, value, reason, words) in cases {
        let env = vec![(name, value)];
        assert_eq!(present_markers(env_of(env.clone())), vec![(name, reason)]);
        let err = refuse_inside_claude_code(env_of(env))
            .expect_err("an unreadable or ambiguous marker refuses")
            .to_string();
        assert!(
            err.contains(&format!("`{name}`")) && err.contains(words),
            "{err}"
        );
        assert!(err.contains("Nothing was stopped"), "{err}");
    }
}

#[test]
fn no_marker_lets_relaunch_run() {
    // Variables Claude Code also sets, but that an operator shell may carry
    // too, are not markers.
    let operator = vec![
        ("CLAUDE_CONFIG_DIR", OsString::from("/Users/op/.claude")),
        ("CLAUDE_CODE_ENTRYPOINT", "cli".into()),
        ("PATH", "/usr/bin".into()),
    ];
    assert!(present_markers(env_of(operator.clone())).is_empty());
    refuse_inside_claude_code(env_of(operator)).expect("an operator shell relaunches");
    refuse_inside_claude_code(env_of(Vec::new())).expect("an empty environment relaunches");
}
