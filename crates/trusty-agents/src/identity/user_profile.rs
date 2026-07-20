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

    /// Zero-config profile capture from the process environment (issue
    /// #3406 follow-up — owner directive: "I don't want to have to set the
    /// credential. You have them, use them.", extended to the profile).
    ///
    /// Why: the first-run interactive interview
    /// (`ctrl::repl::profile::conduct_user_interview`) blocks on stdin every
    /// time `~/.trusty-agents/user.toml` is missing or incomplete — annoying
    /// for a user who already has these values in a `.env.local` (project OR
    /// the user-global `$HOME/.env.local` tier). By the time
    /// `load_or_create_user_profile` calls this, `runtime::startup::
    /// run_startup_init` has already merged BOTH `.env.local` tiers into the
    /// process env via `trusty_common::inference::credentials::
    /// load_env_local_once` (precedence: process env > project `.env.local`
    /// > user `$HOME/.env.local`) — so a plain `std::env::var` read here
    /// transparently sees whichever tier supplied the value, with no
    /// separate file-parsing logic needed.
    /// What: reads `TAGENT_USER_NAME` (required — `None` overall when absent
    /// or blank), `TAGENT_USER_EMAIL` (optional), and `TAGENT_USER_TIMEZONE`
    /// (optional, falling back to the POSIX `TZ` var when unset — a
    /// widely-set convention that's an unambiguous stand-in when the
    /// TAGENT-specific var isn't present). `preferred_model` is always
    /// `None` (no env var maps to it; users needing that use `/model`).
    /// Test: `from_env_reads_name_email_timezone`,
    /// `from_env_falls_back_to_tz_for_timezone`,
    /// `from_env_returns_none_when_name_absent`,
    /// `from_env_treats_blank_name_as_absent`.
    pub fn from_env() -> Option<Self> {
        let name = std::env::var("TAGENT_USER_NAME")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())?;
        let email = std::env::var("TAGENT_USER_EMAIL")
            .ok()
            .filter(|s| !s.is_empty());
        let timezone = std::env::var("TAGENT_USER_TIMEZONE")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| std::env::var("TZ").ok().filter(|s| !s.is_empty()));
        Some(Self {
            name,
            email,
            preferred_model: None,
            timezone,
            created_at: chrono::Utc::now().to_rfc3339(),
        })
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

    /// Clears every var `from_env` reads. Callers must hold
    /// `crate::test_env::ENV_LOCK` for the whole test body.
    fn clear_profile_env() {
        unsafe {
            std::env::remove_var("TAGENT_USER_NAME");
            std::env::remove_var("TAGENT_USER_EMAIL");
            std::env::remove_var("TAGENT_USER_TIMEZONE");
            std::env::remove_var("TZ");
        }
    }

    /// Why: the core zero-config contract — all three vars set must produce
    /// a matching, immediately-complete profile.
    /// Test: itself.
    #[test]
    fn from_env_reads_name_email_timezone() {
        let _g = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_profile_env();
        unsafe {
            std::env::set_var("TAGENT_USER_NAME", "Masa");
            std::env::set_var("TAGENT_USER_EMAIL", "masa@matsuoka.com");
            std::env::set_var("TAGENT_USER_TIMEZONE", "America/New_York");
        }
        let p = UserProfile::from_env().expect("expected a profile from env");
        assert_eq!(p.name, "Masa");
        assert_eq!(p.email.as_deref(), Some("masa@matsuoka.com"));
        assert_eq!(p.timezone.as_deref(), Some("America/New_York"));
        assert!(p.is_complete());
        clear_profile_env();
    }

    /// Why: `TAGENT_USER_TIMEZONE` is the primary var, but a bare `TZ` (a
    /// widely-set POSIX convention) is an unambiguous fallback when only
    /// that is present — avoids forcing a redundant var for users who
    /// already export `TZ`.
    /// Test: itself.
    #[test]
    fn from_env_falls_back_to_tz_for_timezone() {
        let _g = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_profile_env();
        unsafe {
            std::env::set_var("TAGENT_USER_NAME", "Masa");
            std::env::set_var("TZ", "America/New_York");
        }
        let p = UserProfile::from_env().expect("expected a profile from env");
        assert_eq!(p.timezone.as_deref(), Some("America/New_York"));
        clear_profile_env();
    }

    /// Why: `TAGENT_USER_TIMEZONE`, when present, must win over a bare `TZ`
    /// — the TAGENT-specific var is the more deliberate signal.
    /// Test: itself.
    #[test]
    fn from_env_prefers_tagent_timezone_over_tz() {
        let _g = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_profile_env();
        unsafe {
            std::env::set_var("TAGENT_USER_NAME", "Masa");
            std::env::set_var("TAGENT_USER_TIMEZONE", "UTC");
            std::env::set_var("TZ", "America/New_York");
        }
        let p = UserProfile::from_env().expect("expected a profile from env");
        assert_eq!(p.timezone.as_deref(), Some("UTC"));
        clear_profile_env();
    }

    /// Why: no name means no profile — the interactive interview must still
    /// run rather than silently creating a nameless profile.
    /// Test: itself.
    #[test]
    fn from_env_returns_none_when_name_absent() {
        let _g = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_profile_env();
        assert!(UserProfile::from_env().is_none());
    }

    /// Why: a `TAGENT_USER_NAME` set to whitespace-only is effectively
    /// absent, not a valid (blank) name.
    /// Test: itself.
    #[test]
    fn from_env_treats_blank_name_as_absent() {
        let _g = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_profile_env();
        unsafe {
            std::env::set_var("TAGENT_USER_NAME", "   ");
        }
        assert!(UserProfile::from_env().is_none());
        clear_profile_env();
    }
}
