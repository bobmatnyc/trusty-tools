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
//! What: [`check_output_style`] resolves Claude Code's EFFECTIVE `outputStyle`
//! — the project-level `.claude/settings.json` value when that file exists
//! AND carries the key, otherwise the global `~/.claude/settings.json` value
//! (issue #1863: a project settings file that exists but is silent on
//! `outputStyle` must NOT shadow a bad global value, since project settings
//! only override keys they actually set) — and validates it against
//! [`OUTPUT_STYLES`] and the deployed style files under
//! `<home>/.claude/output-styles/`. The report names which scope
//! ("project"/"global") the resolved value came from.
//! Test: `output_style_ok_when_style_resolves`,
//! `output_style_fail_when_id_unknown`, `output_style_warn_when_key_absent`,
//! `output_style_fail_when_file_missing`, `output_style_fail_on_malformed_json`,
//! `output_style_prefers_project_over_global`,
//! `output_style_falls_back_to_global_when_project_silent`,
//! `output_style_falls_back_to_global_when_project_missing`.

use std::path::Path;

use crate::core::bundle::OUTPUT_STYLES;
use crate::core::doctor::{CheckStatus, DoctorCheck};

/// Result of reading the `outputStyle` key out of one settings file.
///
/// Why: a settings file can be "silent" on `outputStyle` in two equivalent
/// ways — the file does not exist, or it exists but has no such key — and
/// both must fall through to the next scope in the resolution chain the same
/// way. Splitting this from a malformed/unreadable file (which must stop
/// resolution and `Fail` immediately) needs its own type rather than
/// overloading `Option`.
/// What: `Present` carries the configured id; `Silent` means "keep looking".
/// Test: exercised indirectly by every `check_output_style` test below.
enum StyleKey {
    Present(String),
    Silent,
}

/// Read the `outputStyle` key from `path`, distinguishing "absent" (fall
/// through) from "malformed" (stop and report).
///
/// Why: shared by both the project and global read attempts in
/// [`check_output_style`] so the two scopes apply identical parsing and
/// error-reporting rules.
/// What: `Ok(StyleKey::Silent)` when the file is missing or has no
/// `outputStyle` key; `Ok(StyleKey::Present(id))` when it does;
/// `Err(DoctorCheck)` (a ready-to-return `Fail`) when the file exists but is
/// unreadable or not valid JSON.
/// Test: `output_style_fail_on_malformed_json`, `output_style_warn_when_key_absent`.
fn read_style_key(path: &Path) -> Result<StyleKey, DoctorCheck> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(StyleKey::Silent),
        Err(e) => {
            return Err(DoctorCheck::new(
                "output_style",
                CheckStatus::Fail,
                format!("{} unreadable: {e}", path.display()),
            ));
        }
    };

    let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
        DoctorCheck::new(
            "output_style",
            CheckStatus::Fail,
            format!("{} is not valid JSON: {e}", path.display()),
        )
    })?;

    match value.get("outputStyle").and_then(|v| v.as_str()) {
        Some(id) => Ok(StyleKey::Present(id.to_string())),
        None => Ok(StyleKey::Silent),
    }
}

/// Probe whether the EFFECTIVE `outputStyle` setting resolves to a real,
/// on-disk trusty-mpm style file.
///
/// Why: this is the deterministic, configuration-level check that catches the
/// exact incident condition (an `outputStyle` value that does not exist),
/// which no session-side "please confirm you loaded" instruction can ever
/// catch, because that instruction itself would be part of what failed to
/// load. It must inspect the value Claude Code will ACTUALLY resolve — the
/// project settings when they set the key, else the global settings — so a
/// bad global value is never masked by a project file that is silent on it
/// (issue #1863).
/// What: tries `<project_dir>/.claude/settings.json` first (when
/// `project_dir` is given); if that file is missing or has no `outputStyle`
/// key, falls back to `<home>/.claude/settings.json`. `Fail`s immediately on
/// an unreadable/malformed settings file at whichever scope it was found in.
/// `Warn` when neither scope configures a value (Claude Code will use its own
/// default, a valid if unconfigured state). Otherwise validates the resolved
/// id against [`OUTPUT_STYLES`] and the deployed file under
/// `<home>/.claude/output-styles/`, reporting `Fail`/`Ok` with the resolved
/// scope ("project"/"global") named in the message.
/// Test: `output_style_ok_when_style_resolves`, `output_style_fail_when_id_unknown`,
/// `output_style_warn_when_key_absent`, `output_style_fail_when_file_missing`,
/// `output_style_falls_back_to_global_when_project_silent`.
pub(crate) fn check_output_style(project_dir: Option<&Path>, home: &Path) -> DoctorCheck {
    let global_path = home.join(".claude").join("settings.json");

    if let Some(dir) = project_dir {
        let project_path = dir.join(".claude").join("settings.json");
        match read_style_key(&project_path) {
            Ok(StyleKey::Present(style_id)) => {
                return evaluate_style(&style_id, home, &project_path, "project");
            }
            Ok(StyleKey::Silent) => { /* fall through to global scope below */ }
            Err(check) => return check,
        }
    }

    match read_style_key(&global_path) {
        Ok(StyleKey::Present(style_id)) => evaluate_style(&style_id, home, &global_path, "global"),
        Ok(StyleKey::Silent) => DoctorCheck::new(
            "output_style",
            CheckStatus::Warn,
            format!(
                "no outputStyle configured (checked project and {}) — \
                 Claude Code will use its own default",
                global_path.display()
            ),
        ),
        Err(check) => check,
    }
}

/// Validate a resolved `outputStyle` id against the known styles and the
/// deployed style files, naming which scope it was resolved from.
///
/// Why: shared by both the project-scope and global-scope resolution
/// branches in [`check_output_style`] so an unknown id or a missing deployed
/// file is reported identically regardless of which settings file supplied
/// it — only the reported scope/path differs.
/// What: `Fail` when `style_id` matches no [`OUTPUT_STYLES`] entry or its
/// deployed file under `<home>/.claude/output-styles/` is missing/empty;
/// `Ok` otherwise. `source` and `source_path` are folded into the message so
/// the operator knows which file the effective value came from.
/// Test: `output_style_ok_when_style_resolves`, `output_style_fail_when_id_unknown`,
/// `output_style_fail_when_file_missing`.
fn evaluate_style(style_id: &str, home: &Path, source_path: &Path, source: &str) -> DoctorCheck {
    let known_ids: Vec<&str> = OUTPUT_STYLES.iter().map(|s| s.id).collect();
    let Some(style) = OUTPUT_STYLES.iter().find(|s| s.id == style_id) else {
        return DoctorCheck::new(
            "output_style",
            CheckStatus::Fail,
            format!(
                "{source} outputStyle {style_id:?} ({}) is not a known trusty-mpm style \
                 (valid: {}) — run `tm run`/`tm load` to rewrite it correctly",
                source_path.display(),
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
                "{source} outputStyle {style_id:?} ({}) resolves to {}",
                source_path.display(),
                style_path.display()
            ),
        ),
        _ => DoctorCheck::new(
            "output_style",
            CheckStatus::Fail,
            format!(
                "{source} outputStyle {style_id:?} ({}) is a known id but {} is missing or \
                 empty — run `tm install` to redeploy styles",
                source_path.display(),
                style_path.display()
            ),
        ),
    }
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

    #[test]
    fn output_style_falls_back_to_global_when_project_silent() {
        // Issue #1863: the exact incident condition. A project settings file
        // EXISTS (so the old `effective_settings_path` would stop looking
        // there) but does not set `outputStyle` — Claude Code itself falls
        // back to the global value in that case, and so must this check. The
        // global value here is the bad incident id, so this must Fail, not
        // silently report Warn/Ok from the project scope.
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write_settings(home.path(), r#"{"outputStyle": "claude_mpm"}"#);
        write_settings(project.path(), r#"{"theme": "dark"}"#);
        let check = check_output_style(Some(project.path()), home.path());
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.message.contains("claude_mpm"));
        assert!(
            check.message.contains("global"),
            "message must name the resolved scope: {}",
            check.message
        );
    }

    #[test]
    fn output_style_falls_back_to_global_when_project_missing() {
        // No project settings file at all (e.g. `tm doctor` run before the
        // project has ever launched a session) must still catch a bad global
        // value.
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write_settings(home.path(), r#"{"outputStyle": "claude_mpm"}"#);
        let check = check_output_style(Some(project.path()), home.path());
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.message.contains("claude_mpm"));
    }

    #[test]
    fn output_style_reports_project_scope_on_success() {
        // The `Ok` message must name which scope ("project") supplied the
        // resolved value, so operators can tell the two apart.
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        deploy_default_style(home.path());
        write_settings(project.path(), r#"{"outputStyle": "trusty-mpm"}"#);
        let check = check_output_style(Some(project.path()), home.path());
        assert_eq!(check.status, CheckStatus::Ok);
        assert!(
            check.message.contains("project"),
            "message must name the resolved scope: {}",
            check.message
        );
    }
}
