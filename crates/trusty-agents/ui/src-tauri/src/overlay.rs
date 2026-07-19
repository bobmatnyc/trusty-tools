//! Personalization overlay Tauri commands (#3061, #3223, #3224).
//!
//! Why: The GUI's agent roster and personality/agent editor need direct
//! filesystem access to a user's `~/.trusty-agents/agents/*.md`
//! personalization overlays — there is no REST equivalent, since
//! `GET /api/agents` only lists the project-local `.trusty-agents/agents/*.toml`
//! catalog (#407), not the user's `$HOME` overlay tier. Split out of
//! `main.rs` (#3220 header-consolidation wave; `main.rs` had grown past the
//! workspace's 500-SLOC production cap) so this fixed-`$HOME`, slug-gated
//! file I/O concern is self-contained.
//! What: `read_personalization_overlay` / `write_personalization_overlay`
//! (#3061, unchanged behavior) plus `list_personalization_overlays` (#3223 —
//! feeds the roster switcher) and `delete_personalization_overlay` (#3224 —
//! trivial delete-with-confirm from the agent editor). All four operate
//! ONLY under `$HOME/.trusty-agents/agents/`, gated by the same
//! `[a-zA-Z0-9_-]+` slug check.
//! Test: `personalization_overlay_path_*`, `list_personalization_overlays_*`,
//! `parse_frontmatter_fields_*`, `delete_personalization_overlay_*` below.

use std::collections::HashMap;

use serde::Serialize;

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
#[derive(Debug, Serialize)]
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
    Ok(personalization_overlays_dir()?.join(format!("{name}.md")))
}

/// Resolve `$HOME/.trusty-agents/agents/` — the fixed directory every
/// overlay command in this module reads/writes/lists.
fn personalization_overlays_dir() -> Result<std::path::PathBuf, String> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| "cannot resolve personalization overlay: $HOME is not set".to_string())?;
    Ok(std::path::PathBuf::from(home)
        .join(".trusty-agents")
        .join("agents"))
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
/// Why: This is the Save action for the personality/agent editor — prose-only
/// editing per #3061 (no structured schema); #3224 reuses this same command
/// for BOTH editing an existing overlay and creating a brand-new one (a
/// create is just a write to a `name` that didn't exist yet). Creates the
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

/// Delete a user's `~/.trusty-agents/agents/<name>.md` personalization
/// overlay (#3224).
///
/// Why: The agent editor's Delete action needs a way to remove an overlay
/// the user no longer wants — the frontend confirms with the user first
/// (native `confirm()`, mirroring the existing Sidebar clear-context pattern)
/// since this is destructive and unrecoverable.
/// What: Resolves and validates the path via
/// [`personalization_overlay_path`], then removes the file. A missing file
/// is treated as success (the end state — "this overlay does not exist" —
/// is already achieved), not an error.
/// Test: `delete_personalization_overlay_removes_file`,
/// `delete_personalization_overlay_missing_file_is_ok`.
#[tauri::command]
pub fn delete_personalization_overlay(name: String) -> Result<(), String> {
    let path = personalization_overlay_path(&name)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!(
            "failed to delete personalization overlay {}: {e}",
            path.display()
        )),
    }
}

/// Summary of one `~/.trusty-agents/agents/*.md` overlay for the roster
/// switcher and agent editor (#3223).
///
/// Why: The roster needs to list a user's personalization overlays
/// alongside the project's `GET /api/agents` catalog. There's no REST route
/// for the `$HOME` overlay tier, so this Tauri command fills that gap with
/// just enough metadata (parsed from the YAML frontmatter) to render a
/// roster row and pre-fill the editor's "extends" field.
/// What: `slug` is the file stem (the dispatchable agent name); `name`,
/// `display_name`, `extends` are best-effort frontmatter fields (see
/// [`parse_frontmatter_fields`]) — `name` falls back to `slug` when absent.
#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub struct OverlaySummary {
    pub slug: String,
    pub name: String,
    pub display_name: Option<String>,
    pub extends: Option<String>,
}

/// List every `~/.trusty-agents/agents/*.md` personalization overlay (#3223).
///
/// Why: Feeds the roster switcher's "your custom agents" section and the
/// agent editor's "pick an overlay to edit" list. Returns an empty list
/// (not an error) when the directory doesn't exist yet — a fresh install
/// has no overlays, which is a normal, not exceptional, state.
/// What: Reads every `*.md` entry in `$HOME/.trusty-agents/agents/`, parses
/// its YAML frontmatter via [`parse_frontmatter_fields`], and returns one
/// [`OverlaySummary`] per file, sorted by slug. Unreadable or non-UTF8 files
/// are skipped rather than failing the whole list (mirrors the backend's
/// own `scan_agents_dir` tolerance for malformed entries).
/// Test: `list_personalization_overlays_empty_dir_returns_empty`,
/// `list_personalization_overlays_parses_frontmatter`.
#[tauri::command]
pub fn list_personalization_overlays() -> Result<Vec<OverlaySummary>, String> {
    let dir = personalization_overlays_dir()?;
    list_overlays_in(&dir)
}

/// Directory-scanning body for [`list_personalization_overlays`], extracted
/// so tests can point it at a temp dir instead of mutating `$HOME`.
fn list_overlays_in(dir: &std::path::Path) -> Result<Vec<OverlaySummary>, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("failed to read {}: {e}", dir.display())),
    };

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let Some(slug) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let fm = parse_frontmatter_fields(&content);
        out.push(OverlaySummary {
            slug: slug.to_string(),
            name: fm.get("name").cloned().unwrap_or_else(|| slug.to_string()),
            display_name: fm.get("display_name").cloned(),
            extends: fm.get("extends").cloned(),
        });
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    Ok(out)
}

/// Best-effort parse of a `.md` agent file's flat YAML frontmatter.
///
/// Why: `src-tauri` deliberately has no YAML dependency (keeps this
/// small desktop-shell crate's build lean); every overlay this app writes
/// has flat, single-line `key: value` frontmatter (see the starter template
/// in `PersonalityPanel.svelte` / the DOC-41 §2.5.1 worked example), so a
/// simple line scan covers the real format without pulling in `serde_yaml`.
/// Multi-line/nested YAML values are not supported — a file using them
/// simply won't have that field populated in the returned map, which is a
/// safe degradation (the roster falls back to the file's slug as its name).
/// What: Returns the key→value map between the first `---` line and the
/// next `---` line (or end of file). Quoted values have their surrounding
/// `"` stripped; blank values are omitted from the map.
/// Test: `parse_frontmatter_fields_extracts_known_keys`,
/// `parse_frontmatter_fields_ignores_content_without_leading_delimiter`.
fn parse_frontmatter_fields(content: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut lines = content.lines();
    if lines.next() != Some("---") {
        return map;
    }
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            let value = value.trim().trim_matches('"').to_string();
            if !value.is_empty() {
                map.insert(key.trim().to_string(), value);
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    // Why: `personalization_overlay_path` reads the process-global `$HOME`
    // env var; these are the only tests in this crate that mutate it, but
    // `cargo test` still runs them concurrently on multiple threads within
    // this binary unless serialized.
    static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Run `f` with `$HOME` temporarily pointed at a fresh temp dir (unique
    /// per call via `label` + pid), restoring the previous value afterward.
    /// Centralizes the lock/set/restore dance so every HOME-mutating test
    /// below is a one-liner instead of repeating the unsafe block.
    fn with_temp_home<T>(label: &str, f: impl FnOnce(&std::path::Path) -> T) -> T {
        let _guard = HOME_LOCK.lock().expect("lock poisoned");
        let tmp = std::env::temp_dir().join(format!("t3223-home-{label}-{}", std::process::id()));
        let prev = std::env::var_os("HOME");
        // SAFETY: guarded by HOME_LOCK; restored before returning.
        unsafe {
            std::env::set_var("HOME", &tmp);
        }
        let result = f(&tmp);
        // SAFETY: guarded by HOME_LOCK.
        unsafe {
            match &prev {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);
        result
    }

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
        with_temp_home("path-join", |tmp| {
            let path = personalization_overlay_path("my-assistant").expect("valid slug");
            assert_eq!(
                path,
                tmp.join(".trusty-agents")
                    .join("agents")
                    .join("my-assistant.md")
            );
        });
    }

    #[test]
    fn read_personalization_overlay_reports_not_found_without_error() {
        with_temp_home("read-nf", |_tmp| {
            let result = read_personalization_overlay("does-not-exist".to_string());
            let overlay = result.expect("missing file is Ok(None), not Err");
            assert!(overlay.content.is_none());
        });
    }

    #[test]
    fn write_then_read_personalization_overlay_round_trips() {
        with_temp_home("write-read", |_tmp| {
            let write_result = write_personalization_overlay(
                "my-assistant".to_string(),
                "---\nname: my-assistant\n---\n\nBe helpful.\n".to_string(),
            );
            let read_result = read_personalization_overlay("my-assistant".to_string());
            write_result.expect("write should succeed, creating the dir");
            let overlay = read_result.expect("read should succeed after write");
            assert_eq!(
                overlay.content.as_deref(),
                Some("---\nname: my-assistant\n---\n\nBe helpful.\n")
            );
        });
    }

    #[test]
    fn delete_personalization_overlay_removes_file() {
        with_temp_home("delete", |_tmp| {
            write_personalization_overlay(
                "temp-agent".to_string(),
                "---\nname: temp-agent\n---\n\nHi.\n".to_string(),
            )
            .expect("write should succeed");
            delete_personalization_overlay("temp-agent".to_string())
                .expect("delete should succeed");
            let read_result = read_personalization_overlay("temp-agent".to_string())
                .expect("read after delete should be Ok(None), not Err");
            assert!(read_result.content.is_none());
        });
    }

    #[test]
    fn delete_personalization_overlay_missing_file_is_ok() {
        with_temp_home("delete-missing", |_tmp| {
            delete_personalization_overlay("never-existed".to_string())
                .expect("deleting a nonexistent overlay is a no-op success");
        });
    }

    #[test]
    fn list_personalization_overlays_empty_dir_returns_empty() {
        let dir = std::env::temp_dir().join(format!("t3223-list-empty-{}", std::process::id()));
        let result = list_overlays_in(&dir).expect("missing dir yields empty, not Err");
        assert!(result.is_empty());
    }

    #[test]
    fn list_personalization_overlays_parses_frontmatter() {
        let dir = std::env::temp_dir().join(format!("t3223-list-parse-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::write(
            dir.join("my-cto.md"),
            "---\nname: my-cto\nextends: cto-assistant\ndisplay_name: My CTO\n---\n\nBe terse.\n",
        )
        .expect("write overlay");
        std::fs::write(dir.join("not-an-overlay.txt"), "ignored").expect("write non-md file");

        let result = list_overlays_in(&dir).expect("list should succeed");
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(result.len(), 1, "the .txt file must be skipped");
        assert_eq!(result[0].slug, "my-cto");
        assert_eq!(result[0].name, "my-cto");
        assert_eq!(result[0].display_name.as_deref(), Some("My CTO"));
        assert_eq!(result[0].extends.as_deref(), Some("cto-assistant"));
    }

    #[test]
    fn parse_frontmatter_fields_extracts_known_keys() {
        let content =
            "---\nname: izzie\nextends: assistant\ndisplay_name: \"Izzie\"\n---\n\nBody text.\n";
        let fm = parse_frontmatter_fields(content);
        assert_eq!(fm.get("name").map(String::as_str), Some("izzie"));
        assert_eq!(fm.get("extends").map(String::as_str), Some("assistant"));
        assert_eq!(fm.get("display_name").map(String::as_str), Some("Izzie"));
    }

    #[test]
    fn parse_frontmatter_fields_ignores_content_without_leading_delimiter() {
        let content = "No frontmatter here.\nname: not-really-a-field\n";
        let fm = parse_frontmatter_fields(content);
        assert!(fm.is_empty());
    }
}
