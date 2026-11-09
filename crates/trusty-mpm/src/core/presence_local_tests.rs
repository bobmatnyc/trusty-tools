//! Tests for the local presence prompt (`presence_local.rs`), #8878 PR2a.

use std::ffi::OsString;

use super::*;

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
    let pairs: Vec<(String, OsString)> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
        .collect();
    move |name| {
        pairs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }
}

#[test]
fn an_ssh_caller_is_headless() {
    assert!(headless_reason(env(&[("SSH_CONNECTION", "10.0.0.1 22 10.0.0.2 22")])).is_some());
    assert!(headless_reason(env(&[("SSH_TTY", "/dev/ttys004")])).is_some());
    assert!(
        headless_reason(env(&[("SSH_CLIENT", "")])).is_none(),
        "empty is unset"
    );
    assert!(headless_reason(env(&[("TERM", "xterm")])).is_none());
}

/// Touches the OS: the class loads and answers both selectors this module
/// sends, so a renamed selector fails here and not at Bob's prompt. Shows
/// no prompt (`canEvaluatePolicy:error:` only).
#[cfg(target_os = "macos")]
#[test]
#[ignore = "touches LocalAuthentication; run with --include-ignored on a Mac"]
fn local_authentication_class_answers_both_selectors() {
    use objc2::runtime::{AnyClass, Sel};
    let class = AnyClass::get(c"LAContext").expect("LAContext loads");
    for name in [
        c"canEvaluatePolicy:error:",
        c"evaluatePolicy:localizedReason:reply:",
    ] {
        assert!(
            class.instance_method(Sel::register(name)).is_some(),
            "{name:?}"
        );
    }
    let context = platform::context().expect("a context");
    // Either answer is fine; the call must not panic on its encoding.
    let _ = platform::can_evaluate(&context);
}
