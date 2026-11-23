//! `ta handoff` argument parsing (#11650 item 1).

use super::super::{HandoffArgs, HandoffError, Target, parse_args};

fn words(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| p.to_string()).collect()
}

/// Why: item 1 — the target is always explicit; a missing or unknown one is
/// a clear error, never a default.
/// Test: itself.
#[test]
fn the_target_is_required_and_checked() {
    let err = parse_args(&words(&["izzie", "do", "it"])).expect_err("no --to");
    assert!(matches!(err, HandoffError::NoTarget), "{err:?}");
    assert!(err.to_string().contains("no default target"), "{err}");

    let err = parse_args(&words(&["izzie", "--to", "tx", "do"])).expect_err("bad --to");
    assert!(
        matches!(&err, HandoffError::UnknownTarget(t) if t == "tx"),
        "{err:?}"
    );
    assert!(err.to_string().contains("expected `tm` or `tc`"), "{err}");

    let err = parse_args(&words(&["izzie", "--to"])).expect_err("dangling --to");
    assert!(matches!(err, HandoffError::MissingValue("--to")), "{err:?}");
    assert!(matches!(parse_args(&[]), Err(HandoffError::Usage)));

    assert_eq!(
        parse_args(&words(&["izzie", "--to", "tm", "do", "it"])).expect("tm"),
        HandoffArgs {
            instance: "izzie".into(),
            target: Target::Tm,
            key: None,
            task: "do it".into(),
        }
    );
    let tc = parse_args(&words(&["izzie", "--key=k1", "--to=tc", "x"])).expect("tc");
    assert_eq!((tc.target, tc.key.as_deref()), (Target::Tc, Some("k1")));
}

/// Why: item 1 — the task text is passed through, never interpreted; flag
/// spellings inside it stay task text.
/// Test: itself.
#[test]
fn task_words_after_the_flags_are_kept_verbatim() {
    let args = parse_args(&words(&[
        "izzie",
        "--to",
        "tm",
        "--",
        "--to",
        "tc",
        "  two  spaces\nand a newline \"quoted\"",
    ]))
    .expect("parse");
    assert_eq!(args.target, Target::Tm);
    assert_eq!(args.task, "--to tc   two  spaces\nand a newline \"quoted\"");

    let args = parse_args(&words(&["izzie", "--to", "tc", "fix", "--key", "x"])).expect("parse");
    assert_eq!((args.key, args.task.as_str()), (None, "fix --key x"));
}
