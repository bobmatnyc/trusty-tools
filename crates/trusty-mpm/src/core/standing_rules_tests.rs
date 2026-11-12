//! Tests for [`super`]: the standing-rules read and hook response (#10960).

use super::*;

/// A project dir holding `local/standing-rules.md` with `bytes`.
fn project_with(bytes: &[u8]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("local")).expect("mkdir local");
    std::fs::write(rules_path(dir.path()), bytes).expect("write rules");
    dir
}

#[test]
fn an_absent_file_is_a_silent_no_op() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert_eq!(load(dir.path()), Load::Absent);
    for event in STANDING_RULES_EVENTS {
        assert_eq!(hook_output(event, &Load::Absent), None);
    }
}

#[test]
fn a_blank_file_is_a_silent_no_op() {
    for body in [&b""[..], b"  \n\t\n"] {
        let dir = project_with(body);
        assert_eq!(load(dir.path()), Load::Absent);
    }
}

#[test]
fn rules_under_the_cap_are_injected_verbatim() {
    let body = "- Every reply carries one Fable-use line.\n- Ordinary cases first.\n";
    let dir = project_with(body.as_bytes());
    assert_eq!(
        load(dir.path()),
        Load::Rules {
            text: body.to_owned(),
            warning: None
        }
    );
}

#[test]
fn the_response_injects_on_both_events() {
    let load = Load::Rules {
        text: "- rule one\n".to_owned(),
        warning: None,
    };
    for event in STANDING_RULES_EVENTS {
        let out = hook_output(event, &load).expect("a response");
        assert_eq!(out["hookSpecificOutput"]["hookEventName"], event);
        let context = out["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .expect("context");
        assert!(context.ends_with("- rule one\n"), "{context}");
        assert_eq!(context, format!("{STANDING_RULES_HEADER}- rule one\n"));
        assert!(STANDING_RULES_HEADER.contains(STANDING_RULES_REL_PATH));
        assert!(out.get("systemMessage").is_none(), "{out}");
        assert!(out.get("decision").is_none() && out.get("continue").is_none());
    }
}

#[test]
fn an_unknown_event_prints_nothing() {
    let load = Load::Rules {
        text: "- rule\n".to_owned(),
        warning: None,
    };
    assert_eq!(hook_output("PreToolUse", &load), None);
    assert_eq!(hook_output("", &load), None);
}

/// Split an over-cap `text` into its rule lines and its last line, the notice.
fn split_notice(text: &str) -> (&str, &str) {
    let body = text.strip_suffix('\n').unwrap_or(text);
    body.rsplit_once('\n')
        .map_or(("", body), |(rules, notice)| {
            (&text[..rules.len() + 1], notice)
        })
}

/// Assert `notice` is the in-band cut notice for `path` and `cap`.
fn assert_cut_notice(notice: &str, path: &Path, cap: usize) {
    assert!(
        notice.contains(&path.display().to_string())
            && notice.contains(&format!("{cap}-byte cap"))
            && notice.contains("incomplete"),
        "no in-band cut notice naming the file and cap: {notice:?}"
    );
}

#[test]
fn an_over_cap_file_keeps_whole_lines_and_warns() {
    let dir = project_with(&b"aaaa\nbbbb\ncccc\n".repeat(20));
    let Load::Rules { text, warning } = load_from(&rules_path(dir.path()), 250) else {
        panic!("over-cap rules still load");
    };
    let warning = warning.expect("an over-cap warning");
    assert!(
        warning.contains("300 bytes") && warning.contains("250-byte cap"),
        "{warning}"
    );
    let out = hook_output(
        "UserPromptSubmit",
        &Load::Rules {
            text,
            warning: Some(warning.clone()),
        },
    )
    .expect("a response");
    assert_eq!(out["systemMessage"], warning.as_str());
    assert!(out["hookSpecificOutput"]["additionalContext"].is_string());
}

#[test]
fn an_over_cap_file_keeps_whole_lines_and_an_in_band_notice() {
    let body = (0..100)
        .map(|i| format!("rule {i:02}\n"))
        .collect::<String>();
    let dir = project_with(body.as_bytes());
    let path = rules_path(dir.path());
    let cap = 400;
    let Load::Rules { text, .. } = load_from(&path, cap) else {
        panic!("over-cap rules still load");
    };
    let whole = STANDING_RULES_HEADER.len() + text.len();
    assert!(whole <= cap, "{whole} bytes over the {cap} cap");
    let (rules, notice) = split_notice(&text);
    assert_cut_notice(notice, &path, cap);
    assert!(!rules.is_empty() && rules.ends_with('\n'), "{rules:?}");
    assert!(body.starts_with(rules), "whole lines only: {rules:?}");
}

/// The `additionalContext` [`hook_output`] builds for `load`.
fn context_of(load: &Load) -> String {
    let out = hook_output("UserPromptSubmit", load).expect("a response");
    out["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("additionalContext")
        .to_owned()
}

#[test]
fn the_cap_boundary_is_verbatim_at_the_cap_and_noticed_one_byte_over() {
    // #10960: the cap bounds header plus rules, so the file's room is smaller.
    let room = STANDING_RULES_CAP_BYTES - STANDING_RULES_HEADER.len();
    let line = format!("{}\n", "x".repeat(39));
    let at_cap = format!("{}{}", line.repeat(room / 40), "\n".repeat(room % 40));
    assert_eq!(at_cap.len(), room);
    let dir = project_with(at_cap.as_bytes());
    assert_eq!(
        context_of(&load(dir.path())).len(),
        STANDING_RULES_CAP_BYTES,
        "a file filling the room arrives whole, exactly at the cap"
    );
    assert_eq!(
        load(dir.path()),
        Load::Rules {
            text: at_cap.clone(),
            warning: None
        }
    );

    let over = format!("{at_cap}z");
    let dir = project_with(over.as_bytes());
    let Load::Rules { text, warning } = load(dir.path()) else {
        panic!("over-cap rules still load");
    };
    assert!(warning.is_some(), "the operator warning stays");
    let context = context_of(&Load::Rules {
        text: text.clone(),
        warning,
    });
    assert!(
        context.len() <= STANDING_RULES_CAP_BYTES,
        "the whole context is {} bytes",
        context.len()
    );
    let (rules, notice) = split_notice(&text);
    assert_cut_notice(notice, &rules_path(dir.path()), STANDING_RULES_CAP_BYTES);
    assert!(at_cap.starts_with(rules), "whole lines only: {rules:?}");
}

#[test]
fn an_over_long_first_line_yields_the_notice_alone() {
    let body = format!("{}\n- b\n", "a".repeat(3000));
    let dir = project_with(body.as_bytes());
    let Load::Rules { text, .. } = load(dir.path()) else {
        panic!("over-cap rules still load");
    };
    assert!(
        STANDING_RULES_HEADER.len() + text.len() <= STANDING_RULES_CAP_BYTES,
        "{} bytes",
        text.len()
    );
    let (rules, notice) = split_notice(&text);
    assert_eq!(rules, "", "no partial first line");
    assert_cut_notice(notice, &rules_path(dir.path()), STANDING_RULES_CAP_BYTES);
    let out = hook_output("SessionStart", &load(dir.path())).expect("a response");
    let context = out["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("the notice reaches the model");
    assert!(context.contains("incomplete"), "{context}");
}

#[test]
fn an_over_cap_first_line_injects_nothing_but_warns() {
    let dir = project_with("é".repeat(10).as_bytes());
    let Load::Rules { text, warning } = load_from(&rules_path(dir.path()), 5) else {
        panic!("over-cap rules still load");
    };
    assert_eq!(text, "");
    let out = hook_output("SessionStart", &Load::Rules { text, warning }).expect("a warning");
    assert!(out.get("hookSpecificOutput").is_none(), "{out}");
    assert!(out["systemMessage"].is_string());
}

#[test]
fn a_file_exactly_at_the_cap_is_not_over() {
    let dir = project_with(b"abc\n");
    assert_eq!(
        load_from(&rules_path(dir.path()), STANDING_RULES_HEADER.len() + 4),
        Load::Rules {
            text: "abc\n".to_owned(),
            warning: None
        }
    );
}

#[test]
fn invalid_utf8_is_unreadable_and_injects_nothing() {
    let dir = project_with(b"- rule\n\xff\xfe broken\n");
    let load = load(dir.path());
    let Load::Unreadable(warning) = &load else {
        panic!("invalid UTF-8 must not load: {load:?}");
    };
    assert!(warning.contains("not valid UTF-8"), "{warning}");
    let out = hook_output("UserPromptSubmit", &load).expect("a warning");
    assert!(out.get("hookSpecificOutput").is_none(), "{out}");
    assert_eq!(out["systemMessage"], warning.as_str());
}

#[test]
fn a_directory_at_the_path_is_unreadable() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(rules_path(dir.path())).expect("mkdir at the rules path");
    assert!(matches!(load(dir.path()), Load::Unreadable(_)));
}

#[cfg(unix)]
#[test]
fn an_unreadable_file_warns_and_injects_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let dir = project_with(b"- rule\n");
    let path = rules_path(dir.path());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    // Root reads a mode-000 file; the arm is then unreachable here.
    if std::fs::read(&path).is_ok() {
        return;
    }
    let load = load(dir.path());
    assert!(matches!(load, Load::Unreadable(_)), "{load:?}");
    let out = hook_output("SessionStart", &load).expect("a warning");
    assert!(out.get("hookSpecificOutput").is_none(), "{out}");
}
