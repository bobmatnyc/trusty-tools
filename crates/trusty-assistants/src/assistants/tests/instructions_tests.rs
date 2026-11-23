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
