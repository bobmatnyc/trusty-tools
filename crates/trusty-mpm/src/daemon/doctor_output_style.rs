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
//!
//! [`check_output_style_staleness`] closes a second gap (issue #2333): even
//! when [`check_output_style`] reports `Ok` (the configured id resolves to a
//! non-empty file), that file's CONTENT can still have drifted from the
//! bundled asset — exactly what happened after PR #2328 corrected the PM
//! identity string but the deployed `~/.claude/output-styles/*.md` copies
//! were never refreshed. It adopts the same Warn-on-drift diagnostic pattern
//! `skill_staleness` (issue #2876) already uses for skills, but compares
//! deployed bytes directly rather than via a deploy-manifest checksum — output
//! styles have no per-deploy manifest, and a direct byte read also catches
//! post-deploy manual edits/corruption that a manifest-checksum comparison
//! would not see. Plus an orphan-file scan for foreign/dormant files sitting
//! in the same directory (e.g. a pre-rebrand `claude-mpm.md`).
//! Test: `staleness_ok_when_in_sync`, `staleness_warns_on_drift`,
//! `staleness_warns_on_orphan`, `staleness_ok_when_dir_missing`,
//! `staleness_ok_when_file_never_deployed`,
//! `staleness_orphan_exempts_configured_custom_id`.

use std::collections::HashSet;
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

/// Best-effort resolution of the effective `outputStyle` id, ignoring any
/// error/malformed states — those are already reported by
/// [`check_output_style`] itself. Used only to exempt the currently
/// configured style's file from orphan detection below.
///
/// Why: [`check_output_style_staleness`] must not flag the file backing a
/// deliberately configured custom style id (one that resolves to no
/// [`OUTPUT_STYLES`] entry — already `Fail`ed by [`evaluate_style`], but the
/// operator's own file, not a foreign leftover) as an orphan.
/// What: project scope first (when `project_dir` is given and sets the key),
/// else global scope; any unreadable/malformed/absent file at either scope
/// resolves to `None` rather than propagating an error, since this helper
/// only narrows an advisory scan.
/// Test: `staleness_orphan_exempts_configured_custom_id`.
fn resolve_effective_style_id(project_dir: Option<&Path>, home: &Path) -> Option<String> {
    if let Some(dir) = project_dir
        && let Ok(StyleKey::Present(id)) =
            read_style_key(&dir.join(".claude").join("settings.json"))
    {
        return Some(id);
    }
    match read_style_key(&home.join(".claude").join("settings.json")) {
        Ok(StyleKey::Present(id)) => Some(id),
        _ => None,
    }
}

/// Probe deployed output-style files for content drift against the bundled
/// catalog, and flag orphaned files under `output-styles/` (issue #2333).
///
/// Why: [`check_output_style`] only validates that the configured id
/// RESOLVES to a non-empty file — it never compares content, so a stale
/// on-disk copy reports a clean `Ok` even though the deployed text is out of
/// date (the exact gap that let the PR #2328 identity-string fix sit
/// undeployed through an install + daemon restart). A dormant foreign file
/// (e.g. a pre-rebrand `claude-mpm.md`) beside the current styles is also
/// invisible today and is a landmine if `outputStyle` is ever mis-set to it.
/// What: for every [`OUTPUT_STYLES`] entry, byte-compares the deployed
/// `<home>/.claude/output-styles/<file_name>` against the bundled `content`;
/// a mismatch is reported as drifted (a missing file is skipped here — that
/// state is already `Fail`ed by [`check_output_style`], so it is not
/// double-reported as drift). Separately scans `output-styles/` for `.md`
/// files whose name matches no bundled `file_name` and is not
/// `<configured_id>.md` (via [`resolve_effective_style_id`]) — those are
/// orphans, named but NEVER deleted (issue #2333: a foreign file may belong
/// to a separately-installed tool). `Warn` when either set is non-empty,
/// naming what was found and `tm install` as the drift remediation; `Ok`
/// when the directory is fully in sync (including when it does not exist
/// yet — nothing has been deployed there, a state [`check_output_style`]
/// already reports on).
/// Test: `staleness_ok_when_in_sync`, `staleness_warns_on_drift`,
/// `staleness_warns_on_orphan`, `staleness_ok_when_dir_missing`,
/// `staleness_ok_when_file_never_deployed`,
/// `staleness_orphan_exempts_configured_custom_id`.
pub(crate) fn check_output_style_staleness(project_dir: Option<&Path>, home: &Path) -> DoctorCheck {
    let styles_dir = home.join(".claude").join("output-styles");
    let known_names: HashSet<&str> = OUTPUT_STYLES.iter().map(|s| s.file_name).collect();

    let mut drifted: Vec<&str> = Vec::new();
    for style in OUTPUT_STYLES {
        match std::fs::read(styles_dir.join(style.file_name)) {
            Ok(bytes) if bytes != style.content.as_bytes() => drifted.push(style.file_name),
            _ => {} // in sync, missing (not this probe's concern), or unreadable (skip)
        }
    }

    let mut orphans: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&styles_dir) {
        let exempt_custom =
            resolve_effective_style_id(project_dir, home).map(|id| format!("{id}.md"));
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            if !name.ends_with(".md") || known_names.contains(name) {
                continue;
            }
            if exempt_custom.as_deref() == Some(name) {
                continue;
            }
            orphans.push(name.to_string());
        }
    }
    orphans.sort_unstable();

    if drifted.is_empty() && orphans.is_empty() {
        return DoctorCheck::new(
            "output_style_staleness",
            CheckStatus::Ok,
            "deployed output styles match the installed binary's bundled assets",
        );
    }

    let mut parts: Vec<String> = Vec::new();
    if !drifted.is_empty() {
        parts.push(format!(
            "{} drifted from bundled content ({}) — run `tm install` to redeploy",
            drifted.len(),
            drifted.join(", ")
        ));
    }
    if !orphans.is_empty() {
        parts.push(format!(
            "{} unrecognized file(s) present ({}) — not removed automatically; confirm they \
             are not referenced by outputStyle before deleting",
            orphans.len(),
            orphans.join(", ")
        ));
    }

    DoctorCheck::new(
        "output_style_staleness",
        CheckStatus::Warn,
        parts.join("; "),
    )
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

    /// Deploy every bundled style, byte-exact, under `<home>/.claude/output-styles/`.
    fn deploy_all_styles(home: &Path) {
        let styles_dir = home.join(".claude").join("output-styles");
        std::fs::create_dir_all(&styles_dir).unwrap();
        for style in OUTPUT_STYLES {
            std::fs::write(styles_dir.join(style.file_name), style.content).unwrap();
        }
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

    #[test]
    fn staleness_ok_when_in_sync() {
        // All three bundled styles deployed byte-exact must be Ok.
        let home = tempfile::tempdir().unwrap();
        deploy_all_styles(home.path());
        let check = check_output_style_staleness(None, home.path());
        assert_eq!(check.status, CheckStatus::Ok, "message: {}", check.message);
    }

    #[test]
    fn staleness_warns_on_drift() {
        // This is the exact incident condition (issue #2333): PR #2328
        // corrected the bundled content, but the deployed copy never got
        // refreshed. A byte-level mismatch must Warn and point at `tm install`.
        let home = tempfile::tempdir().unwrap();
        deploy_all_styles(home.path());
        let styles_dir = home.path().join(".claude").join("output-styles");
        let first = &OUTPUT_STYLES[0];
        std::fs::write(
            styles_dir.join(first.file_name),
            "stale pre-#2328 identity text",
        )
        .unwrap();

        let check = check_output_style_staleness(None, home.path());
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.message.contains(first.file_name));
        assert!(check.message.contains("tm install"));
    }

    #[test]
    fn staleness_warns_on_orphan() {
        // A dormant foreign file (issue #2333's `claude-mpm.md` example) must
        // be flagged, but never deleted.
        let home = tempfile::tempdir().unwrap();
        deploy_all_styles(home.path());
        let styles_dir = home.path().join(".claude").join("output-styles");
        std::fs::write(styles_dir.join("claude-mpm.md"), "pre-rebrand lineage").unwrap();

        let check = check_output_style_staleness(None, home.path());
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.message.contains("claude-mpm.md"));
        assert!(
            styles_dir.join("claude-mpm.md").exists(),
            "orphan detection must never delete the foreign file"
        );
    }

    #[test]
    fn staleness_ok_when_dir_missing() {
        // Nothing deployed yet is not staleness — `check_output_style` already
        // reports on that state (Warn/Fail depending on configuration).
        let home = tempfile::tempdir().unwrap();
        let check = check_output_style_staleness(None, home.path());
        assert_eq!(check.status, CheckStatus::Ok);
    }

    #[test]
    fn staleness_ok_when_file_never_deployed() {
        // A style present in the bundle but never written to disk (a subset
        // deploy, or a deletion) must be skipped, not reported as drift —
        // that state is `check_output_style`'s Fail, not this probe's Warn.
        let home = tempfile::tempdir().unwrap();
        deploy_default_style(home.path()); // only OUTPUT_STYLES[0]
        let check = check_output_style_staleness(None, home.path());
        assert_eq!(check.status, CheckStatus::Ok, "message: {}", check.message);
    }

    #[test]
    fn staleness_orphan_exempts_configured_custom_id() {
        // A deliberately configured custom style id (unknown to OUTPUT_STYLES,
        // already `Fail`ed by `check_output_style` itself) is the operator's
        // own file, not a foreign leftover — orphan detection must not also
        // flag it.
        let home = tempfile::tempdir().unwrap();
        deploy_all_styles(home.path());
        write_settings(home.path(), r#"{"outputStyle": "my-custom-style"}"#);
        let styles_dir = home.path().join(".claude").join("output-styles");
        std::fs::write(
            styles_dir.join("my-custom-style.md"),
            "operator's own style",
        )
        .unwrap();

        let check = check_output_style_staleness(None, home.path());
        assert_eq!(check.status, CheckStatus::Ok, "message: {}", check.message);
    }
}
