//! Pre-seed workspace-trust and renderer-prompt dismissals into `~/.claude.json`.
//!
//! Why (closes #1696): when `tm session start` or the workspace provisioner
//! spawns Claude Code in a user's normal `~/.claude.json` context, two startup
//! prompts block the session until the user dismisses them:
//!
//!   1. "Do you trust the files in this folder?" — keyed to
//!      `projects.<workspace>.hasTrustDialogAccepted` in `~/.claude.json`.
//!   2. "Try the new fullscreen renderer?" — keyed to
//!      `fullscreenUpsellSeenCount` at the top level of `~/.claude.json`.
//!
//! Pre-seeding these keys before the session launches means the session starts
//! without any blocking dialog.
//!
//! IMPORTANT SCOPE: this module writes to `~/.claude.json` — the user's HOME
//! config, NOT the managed CLAUDE_CONFIG_DIR. For the managed/isolated path see
//! `crate::core::standalone::trust_seed::preseed_managed_trust`.
//!
//! What: [`preseed_home_trust`] reads `~/.claude.json` (creates `{}` when absent),
//! ensures the workspace-trust keys and `fullscreenUpsellSeenCount >= 1` are set,
//! and writes back atomically. Idempotent: no write when all fields already match.
//!
//! Test: unit tests in this module cover seeding, idempotency, key preservation,
//! and `fullscreenUpsellSeenCount`.

use std::path::Path;

use anyhow::Context as _;

/// Pre-seed workspace trust and fullscreen-upsell dismissal into `~/.claude.json`.
///
/// Why (closes #1696): writing these keys before Claude Code launches prevents two
/// blocking startup prompts — the workspace-trust dialog and the fullscreen-renderer
/// upsell — from halting an otherwise-unattended session.
/// What: reads `~/.claude.json` (defaults to `{}` when absent; skips gracefully on
/// I/O error), ensures `projects.<workspace>` carries `hasTrustDialogAccepted: true`,
/// `hasCompletedProjectOnboarding: true`, `projectOnboardingSeenCount >= 1`, and
/// ensures the top-level `fullscreenUpsellSeenCount >= 1`. Writes back atomically
/// using `trusty_common::claude_config::write_json_atomic`. All existing keys are
/// preserved. Idempotent: no write when every target field already has the required
/// value.
/// Test: `seeds_trust_dialog_accepted`, `seeds_fullscreen_upsell_count`,
/// `is_idempotent`, `preserves_existing_keys`.
pub fn preseed_home_trust(workspace: &Path) -> anyhow::Result<()> {
    use serde_json::Value;

    let claude_json = home_claude_json()?;

    // Read existing config, defaulting to `{}` when absent. Skip gracefully on
    // I/O or parse errors: the user's home config may contain OAuth state we must
    // not clobber, and a best-effort seed is better than an error that aborts the
    // session launch.
    let mut config: Value = match std::fs::read_to_string(&claude_json) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(val) if val.is_object() => val,
            Ok(_) => {
                tracing::warn!(
                    "skipping home trust pre-seed: {} is valid JSON but not an object",
                    claude_json.display()
                );
                return Ok(());
            }
            Err(err) => {
                tracing::warn!(
                    "skipping home trust pre-seed: {} contains malformed JSON ({err})",
                    claude_json.display()
                );
                return Ok(());
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Object(serde_json::Map::new()),
        Err(err) => {
            tracing::warn!(
                "skipping home trust pre-seed: failed to read {}: {err}",
                claude_json.display()
            );
            return Ok(());
        }
    };

    let workspace_key = workspace.to_string_lossy().to_string();

    // Navigate to (or create) projects.<workspace>.
    let projects = config
        .as_object_mut()
        .expect("config is an object")
        .entry("projects")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !projects.is_object() {
        *projects = Value::Object(serde_json::Map::new());
    }
    let projects = projects.as_object_mut().expect("projects is an object");

    let entry = projects
        .entry(workspace_key)
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !entry.is_object() {
        *entry = Value::Object(serde_json::Map::new());
    }
    let entry = entry.as_object_mut().expect("project entry is an object");

    // Gather top-level config reference to check fullscreenUpsellSeenCount.
    // We derive the idempotency check from the entry above and the root below.
    let project_already_seeded = entry.get("hasTrustDialogAccepted") == Some(&Value::Bool(true))
        && entry.get("hasCompletedProjectOnboarding") == Some(&Value::Bool(true))
        && entry
            .get("projectOnboardingSeenCount")
            .and_then(|v| v.as_i64())
            .is_some_and(|n| n >= 1);

    // Seed project trust keys.
    entry.insert("hasTrustDialogAccepted".to_string(), Value::Bool(true));
    entry.insert(
        "hasCompletedProjectOnboarding".to_string(),
        Value::Bool(true),
    );
    entry
        .entry("projectOnboardingSeenCount")
        .or_insert_with(|| Value::from(1));

    // Now work on the top-level fullscreenUpsellSeenCount.
    let upsell_already_seeded = config
        .get("fullscreenUpsellSeenCount")
        .and_then(|v| v.as_i64())
        .is_some_and(|n| n >= 1);

    config
        .as_object_mut()
        .expect("config is an object")
        .entry("fullscreenUpsellSeenCount")
        .or_insert_with(|| Value::from(1));

    if project_already_seeded && upsell_already_seeded {
        // All fields already have the required values — skip the write.
        return Ok(());
    }

    // Create parent directories if needed (e.g. on a fresh system with no
    // ~/.claude.json yet).
    if let Some(parent) = claude_json.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dir {}", parent.display()))?;
    }

    trusty_common::claude_config::write_json_atomic(&claude_json, &config)
        .with_context(|| format!("write {}", claude_json.display()))?;

    Ok(())
}

/// Resolve the path to `~/.claude.json`.
///
/// Why: centralises the home-dir lookup so tests can call this with a known
/// path (they redirect HOME before calling `preseed_home_trust`).
/// What: `dirs::home_dir().join(".claude.json")` or an error when home is unavailable.
/// Test: exercised indirectly by every `preseed_home_trust` test.
fn home_claude_json() -> anyhow::Result<std::path::PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    Ok(home.join(".claude.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Redirect $HOME to a temp dir, call the function, and return the temp dir
    /// (keep it alive so the files persist for assertions).
    fn with_fake_home<F: FnOnce(&TempDir)>(test: F) {
        let fake_home = TempDir::new().unwrap();
        let prev_home = std::env::var("HOME").ok();

        // SAFETY: tests using this helper must NOT run in parallel — each test
        // must be in its own serial group or the HOME mutation may race. All
        // tests in this module are marked #[serial_test::serial].
        unsafe { std::env::set_var("HOME", fake_home.path()) };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            test(&fake_home);
        }));
        // Restore HOME regardless of panics.
        match prev_home {
            Some(p) => unsafe { std::env::set_var("HOME", p) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    // Verify that preseed_home_trust writes hasTrustDialogAccepted = true for
    // the given workspace path.
    #[serial_test::serial]
    #[test]
    fn seeds_trust_dialog_accepted() {
        with_fake_home(|home| {
            let workspace = home.path().join("projects").join("my-repo");
            std::fs::create_dir_all(&workspace).unwrap();

            preseed_home_trust(&workspace).unwrap();

            let text = std::fs::read_to_string(home.path().join(".claude.json")).unwrap();
            let val: serde_json::Value = serde_json::from_str(&text).unwrap();

            let key = workspace.to_string_lossy().to_string();
            let proj = val["projects"][&key].as_object().unwrap();
            assert_eq!(
                proj.get("hasTrustDialogAccepted"),
                Some(&serde_json::Value::Bool(true)),
                "hasTrustDialogAccepted must be true"
            );
            assert_eq!(
                proj.get("hasCompletedProjectOnboarding"),
                Some(&serde_json::Value::Bool(true)),
                "hasCompletedProjectOnboarding must be true"
            );
            assert!(
                proj.get("projectOnboardingSeenCount")
                    .and_then(|v| v.as_i64())
                    .is_some_and(|n| n >= 1),
                "projectOnboardingSeenCount must be >= 1"
            );
        });
    }

    // Verify that preseed_home_trust seeds fullscreenUpsellSeenCount >= 1 at the
    // top level so the renderer upsell dialog is suppressed.
    #[serial_test::serial]
    #[test]
    fn seeds_fullscreen_upsell_count() {
        with_fake_home(|home| {
            let workspace = home.path().join("repo");
            std::fs::create_dir_all(&workspace).unwrap();

            preseed_home_trust(&workspace).unwrap();

            let text = std::fs::read_to_string(home.path().join(".claude.json")).unwrap();
            let val: serde_json::Value = serde_json::from_str(&text).unwrap();
            let count = val
                .get("fullscreenUpsellSeenCount")
                .and_then(|v| v.as_i64())
                .expect("fullscreenUpsellSeenCount must be present");
            assert!(
                count >= 1,
                "fullscreenUpsellSeenCount must be >= 1, got {count}"
            );
        });
    }

    // Verify idempotency: two calls produce identical output (no duplicate keys,
    // no count increment on second call).
    #[serial_test::serial]
    #[test]
    fn is_idempotent() {
        with_fake_home(|home| {
            let workspace = home.path().join("repo");
            std::fs::create_dir_all(&workspace).unwrap();
            let claude_json = home.path().join(".claude.json");

            preseed_home_trust(&workspace).unwrap();
            let after_first = std::fs::read_to_string(&claude_json).unwrap();

            preseed_home_trust(&workspace).unwrap();
            let after_second = std::fs::read_to_string(&claude_json).unwrap();

            assert_eq!(
                after_first, after_second,
                "preseed_home_trust must be idempotent"
            );
        });
    }

    // Verify that existing unrelated keys in ~/.claude.json are preserved.
    #[serial_test::serial]
    #[test]
    fn preserves_existing_keys() {
        with_fake_home(|home| {
            let workspace = home.path().join("repo");
            std::fs::create_dir_all(&workspace).unwrap();
            let claude_json = home.path().join(".claude.json");

            // Pre-populate with OAuth-like data.
            std::fs::write(&claude_json, r#"{"oauthToken":"keep-me","someCount":42}"#).unwrap();

            preseed_home_trust(&workspace).unwrap();

            let text = std::fs::read_to_string(&claude_json).unwrap();
            let val: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(
                val.get("oauthToken").and_then(|v| v.as_str()),
                Some("keep-me"),
                "oauthToken must be preserved"
            );
            assert_eq!(
                val.get("someCount").and_then(|v| v.as_i64()),
                Some(42),
                "someCount must be preserved"
            );
        });
    }
}
