//! `tm doctor` output-style verification probe (DOC-28 R4(a)).
//!
//! Why: the self-awareness incident's root failure (F4) was a stale global
//! `outputStyle` value (`"claude_mpm"`) that resolves to no bundled style
//! file, so Claude Code silently fell back to its plain default system
//! prompt — and nothing in the framework detected or reported the gap. This
//! probe closes that gap deterministically by inspecting the *configuration*
//! (the effective `outputStyle` string and whether a matching file exists on
//! disk) rather than asking the model, which is the only way to catch total
//! instruction-load omission (see `docs/specs/trusty-mpm-self-awareness.md`
//! §6, §9).
//! What: [`check_output_style`] resolves the effective `.claude/settings.json`
//! (project-level if present, else the global one under `home`), reads its
//! `outputStyle` key, and validates it against [`OUTPUT_STYLES`] and the
//! deployed style files under `<home>/.claude/output-styles/`.
//! Test: `output_style_ok_when_style_resolves`,
//! `output_style_fail_when_id_unknown`, `output_style_warn_when_key_absent`,
//! `output_style_fail_when_file_missing`, `output_style_fail_on_malformed_json`,
//! `output_style_prefers_project_over_global`.

use std::path::{Path, PathBuf};

use crate::core::bundle::OUTPUT_STYLES;
use crate::core::doctor::{CheckStatus, DoctorCheck};

/// Probe whether the effective `outputStyle` setting resolves to a real,
/// on-disk trusty-mpm style file.
///
/// Why: this is the deterministic, configuration-level check that catches the
/// exact incident condition (an `outputStyle` value that does not exist),
/// which no session-side "please confirm you loaded" instruction can ever
/// catch, because that instruction itself would be part of what failed to
/// load.
/// What: reads [`effective_settings_path`]; `Warn` when the file is absent or
/// has no `outputStyle` key (Claude Code will use its own default, a valid if
/// unconfigured state); `Fail` when the file is unreadable/malformed JSON, the
/// configured id does not match any [`OUTPUT_STYLES`] entry, or the matching
/// file under `<home>/.claude/output-styles/` is missing/empty; `Ok` when the
/// id is known and its file exists with content.
/// Test: `output_style_ok_when_style_resolves`, `output_style_fail_when_id_unknown`,
/// `output_style_warn_when_key_absent`, `output_style_fail_when_file_missing`.
pub(crate) fn check_output_style(project_dir: Option<&Path>, home: &Path) -> DoctorCheck {
    let settings_path = effective_settings_path(project_dir, home);

    let text = match std::fs::read_to_string(&settings_path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return DoctorCheck::new(
                "output_style",
                CheckStatus::Warn,
                format!(
                    "{} not found — outputStyle unconfigured; Claude Code will use its own default",
                    settings_path.display()
                ),
            );
        }
        Err(e) => {
            return DoctorCheck::new(
                "output_style",
                CheckStatus::Fail,
                format!("{} unreadable: {e}", settings_path.display()),
            );
        }
    };

    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            return DoctorCheck::new(
                "output_style",
                CheckStatus::Fail,
                format!("{} is not valid JSON: {e}", settings_path.display()),
            );
        }
    };

    let Some(style_id) = value.get("outputStyle").and_then(|v| v.as_str()) else {
        return DoctorCheck::new(
            "output_style",
            CheckStatus::Warn,
            format!(
                "{} has no outputStyle key — Claude Code will use its own default",
                settings_path.display()
            ),
        );
    };

    let known_ids: Vec<&str> = OUTPUT_STYLES.iter().map(|s| s.id).collect();
    let Some(style) = OUTPUT_STYLES.iter().find(|s| s.id == style_id) else {
        return DoctorCheck::new(
            "output_style",
            CheckStatus::Fail,
            format!(
                "outputStyle {style_id:?} is not a known trusty-mpm style (valid: {}) — \
                 run `tm run`/`tm load` to rewrite it correctly",
                known_ids.join(", ")
            ),
        );
    };

    let style_path = home
        .join(".claude")
        .join("output-styles")
        .join(style.file_name);
    match std::fs::metadata(&style_path) {
        Ok(meta) if meta.len() > 0 => DoctorCheck::new(
            "output_style",
            CheckStatus::Ok,
            format!(
                "outputStyle {style_id:?} resolves to {}",
                style_path.display()
            ),
        ),
        _ => DoctorCheck::new(
            "output_style",
            CheckStatus::Fail,
            format!(
                "outputStyle {style_id:?} is a known id but {} is missing or empty — \
                 run `tm install` to redeploy styles",
                style_path.display()
            ),
        ),
    }
}

/// Resolve the effective `.claude/settings.json` a launched session would see.
///
/// Why: `write_output_style` only ever writes the project-local settings file
/// (`crates/trusty-mpm/src/core/session_launch/settings.rs`); Claude Code
/// itself falls back to the global `~/.claude/settings.json` when no
/// project-level file exists, so the probe must mirror that precedence to
/// inspect the value a session would actually see.
/// What: returns `<project_dir>/.claude/settings.json` when `project_dir` is
/// given and that file exists, else `<home>/.claude/settings.json`.
/// Test: `output_style_prefers_project_over_global`.
fn effective_settings_path(project_dir: Option<&Path>, home: &Path) -> PathBuf {
    if let Some(dir) = project_dir {
        let project_settings = dir.join(".claude").join("settings.json");
        if project_settings.exists() {
            return project_settings;
        }
    }
    home.join(".claude").join("settings.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write `<dir>/.claude/settings.json` with the given raw JSON text.
    fn write_settings(dir: &Path, json: &str) {
        let claude_dir = dir.join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(claude_dir.join("settings.json"), json).unwrap();
    }

    /// Deploy the default trusty-mpm style file under `<home>/.claude/output-styles/`.
    fn deploy_default_style(home: &Path) {
        let styles_dir = home.join(".claude").join("output-styles");
        std::fs::create_dir_all(&styles_dir).unwrap();
        let default = OUTPUT_STYLES[0];
        std::fs::write(styles_dir.join(default.file_name), default.content).unwrap();
    }

    #[test]
    fn output_style_ok_when_style_resolves() {
        let home = tempfile::tempdir().unwrap();
        deploy_default_style(home.path());
        write_settings(home.path(), r#"{"outputStyle": "trusty-mpm"}"#);
        let check = check_output_style(None, home.path());
        assert_eq!(check.status, CheckStatus::Ok);
    }

    #[test]
    fn output_style_fail_when_id_unknown() {
        // This is the exact incident condition: a stale/foreign style id.
        let home = tempfile::tempdir().unwrap();
        write_settings(home.path(), r#"{"outputStyle": "claude_mpm"}"#);
        let check = check_output_style(None, home.path());
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.message.contains("claude_mpm"));
        assert!(
            check.message.contains("trusty-mpm"),
            "valid id list must be present"
        );
    }

    #[test]
    fn output_style_warn_when_key_absent() {
        let home = tempfile::tempdir().unwrap();
        write_settings(home.path(), r#"{}"#);
        let check = check_output_style(None, home.path());
        assert_eq!(check.status, CheckStatus::Warn);
    }

    #[test]
    fn output_style_warn_when_settings_missing() {
        let home = tempfile::tempdir().unwrap();
        let check = check_output_style(None, home.path());
        assert_eq!(check.status, CheckStatus::Warn);
    }

    #[test]
    fn output_style_fail_when_file_missing() {
        // A known id whose deployed file was never written (or was deleted).
        let home = tempfile::tempdir().unwrap();
        write_settings(home.path(), r#"{"outputStyle": "trusty-mpm"}"#);
        let check = check_output_style(None, home.path());
        assert_eq!(check.status, CheckStatus::Fail);
    }

    #[test]
    fn output_style_fail_on_malformed_json() {
        let home = tempfile::tempdir().unwrap();
        write_settings(home.path(), "{not valid json");
        let check = check_output_style(None, home.path());
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.message.contains("not valid JSON"));
    }

    #[test]
    fn output_style_prefers_project_over_global() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        deploy_default_style(home.path());
        // Global settings carry the bad incident value...
        write_settings(home.path(), r#"{"outputStyle": "claude_mpm"}"#);
        // ...but the project-local settings (written by `prepare_session`)
        // carry the correct one and must take precedence.
        write_settings(project.path(), r#"{"outputStyle": "trusty-mpm"}"#);
        let check = check_output_style(Some(project.path()), home.path());
        assert_eq!(check.status, CheckStatus::Ok);
    }
}
