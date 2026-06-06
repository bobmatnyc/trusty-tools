//! User profile for the CTRL caller identity (#193).
//!
//! Why: CTRL talks to a real person; injecting their name/email/timezone
//! into the system prompt lets the LLM personalize responses and pick
//! sensible defaults (timezone-aware date math, addressing the user by
//! name). The profile is captured once (first run) and reused.
//! What: `UserProfile` is a small TOML-serialized record stored in
//! `~/.trusty-agents/user.toml`. It is never committed and is created via the
//! interactive interview run from `ctrl::run_ctrl` on first launch.
//! Test: See `tests` — round-trips through TOML, `is_complete` requires
//! a non-empty name.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Persisted profile of the human running CTRL.
///
/// Why: Personalizes the CTRL system prompt and gives downstream tools a
/// stable identity for the user (later: API tokens scoped to the user
/// profile).
/// What: name + optional email/timezone/preferred_model + creation
/// timestamp.
/// Test: `profile_round_trip_through_toml`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserProfile {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(default)]
    pub created_at: String,
}

impl UserProfile {
    /// Path to the user profile (`~/.trusty-agents/user.toml`).
    ///
    /// Why: Keeping the path in one place makes test overrides obvious
    /// (tests that need a different path inject via `save_to`/`load_from`).
    /// What: Resolves the home dir via the `dirs` crate; panics only if
    /// the OS reports no home dir, which is unrecoverable for our use.
    /// Test: Tested indirectly via `profile_round_trip_through_toml`.
    pub fn profile_path() -> PathBuf {
        dirs::home_dir()
            .expect("home dir required")
            .join(".trusty-agents")
            .join("user.toml")
    }

    /// Load the profile from the default path, or `None` when missing.
    pub fn load() -> Option<Self> {
        Self::load_from(&Self::profile_path())
    }

    /// Load the profile from a specific path (useful for tests).
    pub fn load_from(path: &std::path::Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        toml::from_str(&text).ok()
    }

    /// Persist this profile to the default path, creating the parent dir.
    pub fn save(&self) -> anyhow::Result<()> {
        self.save_to(&Self::profile_path())
    }

    /// Persist this profile to an explicit path (useful for tests).
    pub fn save_to(&self, path: &std::path::Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    /// A profile is complete when the user has at least set their name.
    ///
    /// Why: Email/timezone are optional; the name is the one field the
    /// CTRL system prompt actually references, so an empty name means we
    /// must re-run the interview.
    /// What: `!self.name.trim().is_empty()`.
    /// Test: `is_complete_requires_non_empty_name`.
    pub fn is_complete(&self) -> bool {
        !self.name.trim().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_round_trip_through_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("user.toml");
        let profile = UserProfile {
            name: "Ada".to_string(),
            email: Some("ada@example.com".to_string()),
            preferred_model: None,
            timezone: Some("UTC".to_string()),
            created_at: "2026-04-25T00:00:00Z".to_string(),
        };
        profile.save_to(&path).unwrap();
        let loaded = UserProfile::load_from(&path).unwrap();
        assert_eq!(loaded.name, "Ada");
        assert_eq!(loaded.email.as_deref(), Some("ada@example.com"));
        assert_eq!(loaded.timezone.as_deref(), Some("UTC"));
        assert_eq!(loaded.created_at, "2026-04-25T00:00:00Z");
        assert!(loaded.is_complete());
    }

    #[test]
    fn is_complete_requires_non_empty_name() {
        let mut p = UserProfile::default();
        assert!(!p.is_complete());
        p.name = "  ".into();
        assert!(!p.is_complete());
        p.name = "Bob".into();
        assert!(p.is_complete());
    }

    #[test]
    fn load_from_missing_path_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.toml");
        assert!(UserProfile::load_from(&path).is_none());
    }
}
