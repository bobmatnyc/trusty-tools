//! Personalization overlay commands (`~/.trusty-agents/agents/<name>.md`).
//!
//! Why: Extracted from `main.rs` (#3270) purely to keep that file under the
//! workspace's 500-SLOC production-file cap; this is a pure code-motion split
//! with no behavior change. The Personality panel needs to read and write a
//! user's prose-only agent overlay file without the frontend ever supplying a
//! raw filesystem path — `name` is validated to a strict slug before any I/O.
//! What: `PersonalizationOverlay` (the read result shape),
//! `is_valid_agent_slug` / `personalization_overlay_path` (shared validation +
//! path resolution), and the `read_personalization_overlay` /
//! `write_personalization_overlay` Tauri commands themselves.
//! Test: unit tests below cover slug validation, path resolution/traversal
//! rejection, and the read/write round trip.

/// Result payload for `read_personalization_overlay`.
///
/// Why: The frontend needs to tell "file exists with this content" apart
/// from "file does not exist yet" so it can offer a starter template instead
/// of rendering a blank/broken editor. A plain `Result<String, String>` would
/// conflate a genuine not-found with a real I/O error.
/// What: `content: None` signals not-found (the UI should offer the starter
/// template); `content: Some(text)` is the file's current contents. Real I/O
/// errors (permission denied, etc.) still surface via the command's `Err`.
/// Test: `personalization_overlay_path_rejects_traversal_and_separators`,
/// manual: delete `~/.trusty-agents/agents/my-assistant.md`, open the panel,
/// confirm the starter-template prompt renders.
#[derive(Debug, serde::Serialize)]
pub struct PersonalizationOverlay {
    content: Option<String>,
    path: String,
}

/// Validate an agent-name slug used to derive a personalization overlay path.
///
/// Why: `read_personalization_overlay` / `write_personalization_overlay`
/// build a filesystem path from a frontend-supplied `name`. Without a strict
/// allowlist a name like `../../etc/passwd` or an absolute path could escape
/// `~/.trusty-agents/agents/` entirely — the fs commands must operate ONLY
/// inside that fixed directory. Rejecting anything but `[a-zA-Z0-9_-]+`
/// rules out path separators (`/`, `\`), `.` (so no `..` traversal or hidden
/// dotfiles), and empty names in one check, with no need to reason about
/// platform-specific separator quirks.
/// What: Returns `true` iff `name` is non-empty and every char is an ASCII
/// alphanumeric, `_`, or `-`.
/// Test: `agent_slug_accepts_simple_names`,
/// `agent_slug_rejects_path_separators_and_traversal`.
fn is_valid_agent_slug(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Resolve the on-disk path for a personalization overlay, validating `name`.
///
/// Why: Centralizes both the slug check and the `$HOME`-anchored join so
/// `read_personalization_overlay` and `write_personalization_overlay` can't
/// drift (e.g. one command validating and the other forgetting to). Always
/// resolves under the real `$HOME`, mirroring the backend's own
/// `agents_dir_candidates()` `$HOME` fallback tier (trusty-agents#3061) — this
/// is deliberately the ONLY tier the GUI writes to; it never touches
/// `TAGENT_CONFIG_DIR` or a project-local `.trusty-agents/agents/`.
/// What: Returns `Err` when `name` fails [`is_valid_agent_slug`] or `$HOME`
/// is unset; otherwise `$HOME/.trusty-agents/agents/<name>.md`.
/// Test: `personalization_overlay_path_rejects_traversal_and_separators`,
/// `personalization_overlay_path_joins_home_dir`.
fn personalization_overlay_path(name: &str) -> Result<std::path::PathBuf, String> {
    if !is_valid_agent_slug(name) {
        return Err(format!(
            "invalid agent name '{name}': must match [a-zA-Z0-9_-]+ (no path separators or dots)"
        ));
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| "cannot resolve personalization overlay: $HOME is not set".to_string())?;
    Ok(std::path::PathBuf::from(home)
        .join(".trusty-agents")
        .join("agents")
        .join(format!("{name}.md")))
}

/// Read a user's `~/.trusty-agents/agents/<name>.md` personalization overlay.
///
/// Why: The Personality panel needs the current overlay content (if any) to
/// populate its editor on open, and a clear not-found signal (rather than an
/// error) so the UI can offer a starter template for a first-time user.
/// What: Resolves and validates the path via
/// [`personalization_overlay_path`], then reads it. A missing file yields
/// `Ok(PersonalizationOverlay { content: None, .. })`; any other I/O failure
/// (permissions, etc.) is a genuine `Err`.
/// Test: `personalization_overlay_path_rejects_traversal_and_separators`;
/// manual: create/delete the target file and confirm both branches render
/// correctly in the panel.
#[tauri::command]
pub fn read_personalization_overlay(name: String) -> Result<PersonalizationOverlay, String> {
    let path = personalization_overlay_path(&name)?;
    match std::fs::read_to_string(&path) {
        Ok(content) => Ok(PersonalizationOverlay {
            content: Some(content),
            path: path.display().to_string(),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(PersonalizationOverlay {
            content: None,
            path: path.display().to_string(),
        }),
        Err(e) => Err(format!(
            "failed to read personalization overlay {}: {e}",
            path.display()
        )),
    }
}

/// Write a user's `~/.trusty-agents/agents/<name>.md` personalization overlay.
///
/// Why: This is the Save action for the Personality panel — prose-only
/// editing per #3061 (no structured schema). Creates the
/// `~/.trusty-agents/agents/` directory on first write since a fresh install
/// won't have it yet.
/// What: Resolves and validates the path via
/// [`personalization_overlay_path`], creates the parent directory if
/// missing, then writes `content` verbatim (full overwrite, matching a plain
/// text-editor Save).
/// Test: `personalization_overlay_path_rejects_traversal_and_separators`;
/// manual: edit + Save in the panel, confirm the file on disk matches and
/// that `by_name`/`by_name_async` (trusty-agents#3061 loader fix) dispatch
/// it.
#[tauri::command]
pub fn write_personalization_overlay(name: String, content: String) -> Result<(), String> {
    let path = personalization_overlay_path(&name)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
    }
    std::fs::write(&path, content).map_err(|e| format!("failed to write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Why: `personalization_overlay_path` reads the process-global `$HOME`
    // env var; these are the only tests in this crate that mutate it, but
    // `cargo test` still runs them concurrently on multiple threads within
    // this binary unless serialized.
    static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn agent_slug_accepts_simple_names() {
        assert!(is_valid_agent_slug("my-assistant"));
        assert!(is_valid_agent_slug("my_assistant2"));
        assert!(is_valid_agent_slug("A"));
    }

    #[test]
    fn agent_slug_rejects_path_separators_and_traversal() {
        assert!(!is_valid_agent_slug(""));
        assert!(!is_valid_agent_slug("../etc/passwd"));
        assert!(!is_valid_agent_slug(".."));
        assert!(!is_valid_agent_slug("foo/bar"));
        assert!(!is_valid_agent_slug("foo\\bar"));
        assert!(!is_valid_agent_slug("/etc/passwd"));
        assert!(!is_valid_agent_slug("my-assistant.md"));
        assert!(!is_valid_agent_slug("my assistant"));
    }

    #[test]
    fn personalization_overlay_path_rejects_traversal_and_separators() {
        let err = personalization_overlay_path("../../etc/passwd")
            .expect_err("traversal must be rejected");
        assert!(err.contains("invalid agent name"));
    }

    #[test]
    fn personalization_overlay_path_joins_home_dir() {
        let _guard = HOME_LOCK.lock().expect("lock poisoned");
        let tmp = std::env::temp_dir().join(format!("t3061-home-{}", std::process::id()));
        let prev = std::env::var_os("HOME");
        // SAFETY: guarded by HOME_LOCK; restored before returning.
        unsafe {
            std::env::set_var("HOME", &tmp);
        }
        let path = personalization_overlay_path("my-assistant").expect("valid slug");
        // SAFETY: guarded by HOME_LOCK.
        unsafe {
            match &prev {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
        assert_eq!(
            path,
            tmp.join(".trusty-agents")
                .join("agents")
                .join("my-assistant.md")
        );
    }

    #[test]
    fn read_personalization_overlay_reports_not_found_without_error() {
        let _guard = HOME_LOCK.lock().expect("lock poisoned");
        let tmp = std::env::temp_dir().join(format!("t3061-home-nf-{}", std::process::id()));
        let prev = std::env::var_os("HOME");
        // SAFETY: guarded by HOME_LOCK; restored before returning.
        unsafe {
            std::env::set_var("HOME", &tmp);
        }
        let result = read_personalization_overlay("does-not-exist".to_string());
        // SAFETY: guarded by HOME_LOCK.
        unsafe {
            match &prev {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
        let overlay = result.expect("missing file is Ok(None), not Err");
        assert!(overlay.content.is_none());
    }

    #[test]
    fn write_then_read_personalization_overlay_round_trips() {
        let _guard = HOME_LOCK.lock().expect("lock poisoned");
        let tmp = std::env::temp_dir().join(format!("t3061-home-rw-{}", std::process::id()));
        let prev = std::env::var_os("HOME");
        // SAFETY: guarded by HOME_LOCK; restored before returning.
        unsafe {
            std::env::set_var("HOME", &tmp);
        }
        let write_result = write_personalization_overlay(
            "my-assistant".to_string(),
            "---\nname: my-assistant\n---\n\nBe helpful.\n".to_string(),
        );
        let read_result = read_personalization_overlay("my-assistant".to_string());
        // SAFETY: guarded by HOME_LOCK.
        unsafe {
            match &prev {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);
        write_result.expect("write should succeed, creating the dir");
        let overlay = read_result.expect("read should succeed after write");
        assert_eq!(
            overlay.content.as_deref(),
            Some("---\nname: my-assistant\n---\n\nBe helpful.\n")
        );
    }
}
