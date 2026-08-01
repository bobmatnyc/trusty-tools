//! Assistant as a TYPE, and the ids that name its INSTANCES (#4325).
//!
//! Why: The owner's 2026-07-30 product model is that "Assistant" is a TYPE and
//! `izzie` / `cto-assistant` are INSTANCES of it — NOT distinct agent types.
//! The repo already encodes half of that (`[agent] role = "assistant"` on the
//! base, `extends = "assistant"` on both named personas) but nothing NAMED the
//! other half, so every surface that wanted "which assistant is this?" reached
//! for the raw agent name — an unvalidated string that is about to become a
//! DIRECTORY NAME under the user's home (see [`super::home`]). A raw name is
//! not safe in that position: `../..` or `a/b` would place an instance's home
//! outside the assistants root.
//! What: [`ASSISTANT_ROLE`] is the type discriminator, [`is_assistant_role`]
//! the one predicate that reads it, and [`AssistantInstanceId`] is a validated
//! instance name — the only thing [`super::home::AssistantHome`] accepts.
//! Validation is REJECTION, never silent slugging: an instance whose home
//! silently landed under a different name than its `agent.toml` says would be
//! exactly the "naming coincidence" failure `super::super::stores::binding`
//! was written to eliminate.
//! Test: `super::tests::instance_tests` — the whole module.

use std::fmt;

use super::error::AssistantError;

/// The `[agent] role` value that marks a config as an Assistant-TYPE instance.
///
/// Why: instance isolation is scoped to assistants; an `engineer` or `ctrl`
/// agent gets no per-assistant home. Pinning the discriminator in one constant
/// keeps that scope from drifting into a second hardcoded `"assistant"`.
/// What: the literal role string used by `assistant/agent.toml` and inherited
/// by every `extends = "assistant"` overlay.
/// Test: `super::tests::instance_tests::assistant_role_is_the_type_discriminator`.
pub const ASSISTANT_ROLE: &str = "assistant";

/// Whether an agent's `[agent] role` makes it an instance of the Assistant type.
///
/// Why/What: see [`ASSISTANT_ROLE`]. Comparison is exact — a role of
/// `assistant-ish` is a different type, not a sloppy spelling of this one.
/// Test: `super::tests::instance_tests::assistant_role_is_the_type_discriminator`.
pub fn is_assistant_role(role: &str) -> bool {
    role == ASSISTANT_ROLE
}

/// A validated name for one INSTANCE of the Assistant type.
///
/// Why: this value becomes a single directory name under the assistants root,
/// so it must be safe in that position before any filesystem call sees it — see
/// this module's doc comment for why rejection beats slugging here.
/// What: a newtype over a `String` that is non-blank, contains only
/// `[a-z0-9._-]` with at least one alphanumeric, is not `.`/`..`, and holds no
/// path separator. Construct with [`AssistantInstanceId::new`]; there is no
/// unchecked constructor, which is what makes the invariant hold everywhere.
/// Test: `super::tests::instance_tests` — the whole module.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AssistantInstanceId(String);

impl AssistantInstanceId {
    /// Validate `raw` as an instance id.
    ///
    /// Why: see the type's doc comment — this is the single gate every home
    /// path goes through.
    /// What: `Ok(id)` for an acceptable name; otherwise
    /// [`AssistantError::InvalidInstanceId`] carrying the reason verbatim, so
    /// the concierge can tell the user WHICH rule the name broke rather than
    /// "invalid name".
    /// Test: `super::tests::instance_tests::accepts_the_shipped_instance_names`,
    /// `super::tests::instance_tests::rejects_path_separators`,
    /// `super::tests::instance_tests::rejects_dot_names`,
    /// `super::tests::instance_tests::rejects_blank_and_exotic_characters`.
    pub fn new(raw: impl Into<String>) -> Result<Self, AssistantError> {
        let raw: String = raw.into();
        let name = raw.trim();
        let invalid = |reason: &str| AssistantError::InvalidInstanceId {
            raw: raw.clone(),
            reason: reason.to_string(),
        };

        if name.is_empty() {
            return Err(invalid("it is blank"));
        }
        if name.len() > MAX_ID_LEN {
            return Err(invalid(&format!(
                "it is longer than {MAX_ID_LEN} characters"
            )));
        }
        if name == "." || name == ".." {
            return Err(invalid(
                "`.` and `..` name a directory's parent or itself, not an instance",
            ));
        }
        if name.contains('/') || name.contains('\\') {
            return Err(invalid(
                "an instance id is ONE directory name, so it may not contain a path separator",
            ));
        }
        if let Some(bad) = name.chars().find(|c| !is_allowed(*c)) {
            return Err(invalid(&format!(
                "`{bad}` is not allowed; use lowercase letters, digits, `-`, `_` or `.`"
            )));
        }
        if !name.chars().any(|c| c.is_ascii_alphanumeric()) {
            return Err(invalid("it contains no letter or digit"));
        }
        Ok(Self(name.to_string()))
    }

    /// The id as a string slice. Test: `super::tests::instance_tests::accepts_the_shipped_instance_names`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AssistantInstanceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Longest accepted instance id, well under every filesystem's name limit.
const MAX_ID_LEN: usize = 64;

/// Characters an instance id may contain.
///
/// Why: uppercase is excluded because macOS is case-INSENSITIVE while Linux is
/// not — `Izzie` and `izzie` would be one home on one machine and two on the
/// other, which is exactly the silent-divergence class this newtype exists to
/// prevent.
fn is_allowed(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.')
}
