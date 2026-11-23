//! `AssistantInstanceId` validation (#4325).
//!
//! Why: the id becomes a single directory name under the user's home, so every
//! rejection here is a path the home model must never be able to take.
//! What: the accept cases (today's shipped instance names) and each reject rule.
//! Test: this module IS the test surface.

use crate::assistants::{ASSISTANT_ROLE, AssistantError, AssistantInstanceId, is_assistant_role};

/// Why: `assistant` is the TYPE and `izzie`/`cto-assistant` are INSTANCES of
/// it — the discriminator is the role, and it is exact.
#[test]
fn assistant_role_is_the_type_discriminator() {
    assert_eq!(ASSISTANT_ROLE, "assistant");
    assert!(is_assistant_role("assistant"));
    assert!(!is_assistant_role("engineer"));
    assert!(!is_assistant_role("assistant-ish"));
    assert!(!is_assistant_role("Assistant"));
}

#[test]
fn accepts_the_shipped_instance_names() {
    for name in ["assistant", "izzie", "cto-assistant", "bob_kb2", "a.b"] {
        let id = AssistantInstanceId::new(name).expect("should accept");
        assert_eq!(id.as_str(), name);
        assert_eq!(id.to_string(), name);
    }
    // Surrounding whitespace is trimmed, not rejected.
    assert_eq!(
        AssistantInstanceId::new("  izzie ").unwrap().as_str(),
        "izzie"
    );
}

/// Why: an id containing a separator would place the home somewhere other than
/// `<assistants_root>/<id>` — the exact escape the newtype exists to stop.
#[test]
fn rejects_path_separators() {
    for name in ["a/b", "a\\b", "../izzie", "/etc/passwd"] {
        let err = AssistantInstanceId::new(name).expect_err("should reject");
        assert!(
            matches!(err, AssistantError::InvalidInstanceId { .. }),
            "wrong error for {name}: {err}"
        );
    }
}

#[test]
fn rejects_dot_names() {
    for name in [".", ".."] {
        let err = AssistantInstanceId::new(name).expect_err("should reject");
        assert!(err.to_string().contains("not an instance"), "was: {err}");
    }
}

#[test]
fn rejects_blank_and_exotic_characters() {
    assert!(AssistantInstanceId::new("").is_err());
    assert!(AssistantInstanceId::new("   ").is_err());
    // Uppercase: case-insensitive macOS vs case-sensitive Linux would disagree
    // about whether `Izzie` and `izzie` are one home or two.
    assert!(AssistantInstanceId::new("Izzie").is_err());
    assert!(AssistantInstanceId::new("izzie!").is_err());
    assert!(AssistantInstanceId::new("izzie bot").is_err());
    // Punctuation-only has no letter or digit to identify an instance by.
    assert!(AssistantInstanceId::new("--").is_err());
    assert!(AssistantInstanceId::new("x".repeat(65)).is_err());
    assert!(AssistantInstanceId::new("x".repeat(64)).is_ok());
}
