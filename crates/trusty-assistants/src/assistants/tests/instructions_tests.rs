//! Reading and role-seeding `instructions.md` (#11649).
//!
//! Why: slice 3 splices this text into every turn, so a broken file must
//! surface as a typed error. Each error arm is pinned, and each checks the
//! file was not rewritten.
//! What: role parsing, seed-once, never-overwrite, the read path, and one
//! test per error arm.
//! Test: this module IS the test surface.

use super::home_tests::temp_home;
use crate::assistants::{
    AssistantError, AssistantRole, MAX_INSTRUCTIONS_BYTES, seed_role_instructions,
};

#[test]
fn roles_parse_and_reject_unknown_names() {
    assert_eq!(
        AssistantRole::parse("personal").unwrap(),
        AssistantRole::Personal
    );
    assert_eq!(AssistantRole::parse(" cto ").unwrap(), AssistantRole::Cto);
    for role in [AssistantRole::Personal, AssistantRole::Cto] {
        assert_eq!(AssistantRole::parse(role.as_str()).unwrap(), role);
    }
    for bad in ["", "CTO", "assistant", "personal-assistant"] {
        assert!(
            matches!(
                AssistantRole::parse(bad),
                Err(AssistantError::InvalidRole { .. })
            ),
            "{bad:?}"
        );
    }
}

#[test]
fn seeding_writes_the_role_template_once() {
    let (_tmp, home) = temp_home();
    assert!(seed_role_instructions(&home, AssistantRole::Cto).unwrap());
    let body = std::fs::read_to_string(home.instructions_path()).unwrap();
    assert_eq!(body, AssistantRole::Cto.template());
    assert!(body.starts_with("# CTO assistant"), "{body}");
    assert!(!seed_role_instructions(&home, AssistantRole::Personal).unwrap());
    assert_eq!(
        std::fs::read_to_string(home.instructions_path()).unwrap(),
        AssistantRole::Cto.template()
    );
    assert!(
        AssistantRole::Personal
            .template()
            .starts_with("# Personal assistant")
    );
}

#[test]
fn seeding_never_overwrites_an_edited_file() {
    let (_tmp, home) = temp_home();
    home.ensure().unwrap();
    std::fs::write(home.instructions_path(), "my own words\n").unwrap();
    assert!(!seed_role_instructions(&home, AssistantRole::Personal).unwrap());
    assert_eq!(
        std::fs::read_to_string(home.instructions_path()).unwrap(),
        "my own words\n"
    );
}

#[test]
fn reads_the_seeded_text() {
    let (_tmp, home) = temp_home();
    seed_role_instructions(&home, AssistantRole::Personal).unwrap();
    assert_eq!(
        home.read_instructions().unwrap(),
        AssistantRole::Personal.template()
    );
}

/// Fail-open check: missing is an error, and reading creates nothing.
#[test]
fn a_missing_file_is_a_typed_error() {
    let (_tmp, home) = temp_home();
    home.ensure().unwrap();
    std::fs::remove_file(home.instructions_path()).unwrap();
    let err = home.read_instructions().unwrap_err();
    assert!(
        matches!(err, AssistantError::InstructionsMissing { .. }),
        "{err}"
    );
    assert!(!home.instructions_path().exists());
}

#[test]
fn a_non_utf8_file_is_a_typed_error() {
    let (_tmp, home) = temp_home();
    home.ensure().unwrap();
    let bytes = [b'#', b' ', 0xff, 0xfe, b'\n'];
    std::fs::write(home.instructions_path(), bytes).unwrap();
    let err = home.read_instructions().unwrap_err();
    assert!(
        matches!(err, AssistantError::InstructionsNotUtf8 { .. }),
        "{err}"
    );
    assert_eq!(std::fs::read(home.instructions_path()).unwrap(), bytes);
}

#[test]
fn an_oversized_file_is_a_typed_error() {
    let (_tmp, home) = temp_home();
    home.ensure().unwrap();
    let cap = usize::try_from(MAX_INSTRUCTIONS_BYTES).unwrap();
    std::fs::write(home.instructions_path(), "a".repeat(cap)).unwrap();
    assert_eq!(home.read_instructions().unwrap().len(), cap);
    std::fs::write(home.instructions_path(), "a".repeat(cap + 1)).unwrap();
    let err = home.read_instructions().unwrap_err();
    assert!(
        matches!(err, AssistantError::InstructionsTooLarge { cap: c, .. } if c == MAX_INSTRUCTIONS_BYTES),
        "{err}"
    );
}

/// A directory where the file belongs cannot be read as text.
#[test]
fn an_unreadable_file_is_a_typed_error() {
    let (_tmp, home) = temp_home();
    home.ensure().unwrap();
    std::fs::remove_file(home.instructions_path()).unwrap();
    std::fs::create_dir(home.instructions_path()).unwrap();
    let err = home.read_instructions().unwrap_err();
    assert!(
        matches!(err, AssistantError::InstructionsUnreadable { .. }),
        "{err}"
    );
    assert!(home.instructions_path().is_dir());
}

/// Fail-open check: whitespace is an error, never an empty prompt.
#[test]
fn an_empty_file_is_a_typed_error() {
    let (_tmp, home) = temp_home();
    home.ensure().unwrap();
    std::fs::write(home.instructions_path(), " \n\t\n").unwrap();
    let err = home.read_instructions().unwrap_err();
    assert!(
        matches!(err, AssistantError::InstructionsEmpty { .. }),
        "{err}"
    );
}

/// Verbs that order a write or a send. The assistant's tools are read-only,
/// so no template clause may start with one, or say "you <verb>".
const WRITE_VERBS: &[&str] = &[
    "record", "store", "save", "remember", "write", "send", "post", "file", "email", "delegate",
    "dispatch", "persist", "log", "hand", "forward", "publish", "submit", "commit",
];

/// Clauses whose first word, or whose `you <word>`, is a [`WRITE_VERBS`] entry.
fn write_instructions(template: &str) -> Vec<String> {
    let joined = template.replace("\n- ", ". ").replace('\n', " ");
    let mut hits = Vec::new();
    for clause in joined
        .split(['.', ';', ':', '(', ')', '!', '?'])
        .flat_map(|c| c.split(" and "))
    {
        let words: Vec<String> = clause
            .split_whitespace()
            .map(|w| {
                w.trim_matches(|c: char| !c.is_alphanumeric())
                    .to_lowercase()
            })
            .filter(|w| !w.is_empty())
            .collect();
        let starts = words
            .first()
            .is_some_and(|w| WRITE_VERBS.contains(&w.as_str()));
        let you_verb = words
            .windows(2)
            .any(|p| p[0] == "you" && WRITE_VERBS.contains(&p[1].as_str()));
        if starts || you_verb {
            hits.push(clause.trim().to_string());
        }
    }
    hits
}

/// #11649 review: the assistant has read-only tools, so its templates may
/// propose a write to the owner but never order one, and never carry the
/// "unless an earlier ruling covers it" escape on the send limit.
#[test]
fn role_templates_never_instruct_a_write_or_send() {
    for role in [AssistantRole::Personal, AssistantRole::Cto] {
        let template = role.template();
        assert_eq!(
            write_instructions(template),
            Vec::<String>::new(),
            "{role:?}"
        );
        assert!(
            template.contains(
                "Your tools are read-only; never claim you stored, sent or filed anything."
            ),
            "{role:?}"
        );
        assert!(!template.contains("unless an earlier ruling"), "{role:?}");
    }
    // The checker itself catches the shapes the first templates used.
    for bad in [
        "- **Record decisions.** Store each ruling.",
        "a task (record it)",
        "When you hand work on, state the goal.",
        "- **Delegate reads.** Send specifications to research.",
    ] {
        assert!(!write_instructions(bad).is_empty(), "{bad}");
    }
}

/// #11649 review: a write that fails part-way leaves neither a truncated
/// target nor its temp file. The writer is injected because a real
/// `write_all` failure (a full disk) cannot be produced in a test.
#[test]
fn a_failed_write_leaves_no_partial_file() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("agent.toml");

    let err =
        crate::assistants::instructions::write_new_via(&target, b"full body", |file, body| {
            use std::io::Write;
            file.write_all(&body[..4])?;
            Err(std::io::Error::other("disk full"))
        })
        .unwrap_err();

    assert!(matches!(err, AssistantError::Io { .. }), "{err}");
    assert!(!target.exists());
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);

    assert!(crate::assistants::instructions::write_new(&target, b"full body").unwrap());
    assert_eq!(std::fs::read(&target).unwrap(), b"full body");
    assert!(!crate::assistants::instructions::write_new(&target, b"other").unwrap());
    assert_eq!(std::fs::read(&target).unwrap(), b"full body");
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1);
}
