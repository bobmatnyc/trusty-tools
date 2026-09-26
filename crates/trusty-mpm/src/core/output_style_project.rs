//! Project-local output styles (#8533).
//!
//! Why: the bundled styles speak as a delegating PM. A project with a different
//! role (the fleet supervisor is the first) needs its own voice, and before
//! #8533 an id outside the bundled registry fell back to the default with only a
//! `tracing` line nobody saw.
//! What: a style id resolves to a bundled style first, then to a project file
//! `<project>/.claude/output-styles/<id>.md` — the directory Claude Code itself
//! reads project styles from, so a native-capable Claude Code applies it from
//! the `outputStyle` key alone. [`effective_style_id`] adds the committed
//! `.trusty-mpm.toml` `[style] active` to the selection chain, and
//! [`resolve_or_default`] turns an unknown id into a warning the launch paths
//! print instead of a silent fallback.
//! Test: `output_style_project_tests.rs`.

use std::path::{Path, PathBuf};

use super::{StyleError, resolve_style};
use crate::core::bundle::{BundledStyle, DEFAULT_OUTPUT_STYLE_ID, OUTPUT_STYLES};
use crate::core::config::MpmConfig;
use crate::core::session_profile::SessionProfile;

/// Project-relative directory holding project output styles.
pub const PROJECT_STYLES_DIR: &str = ".claude/output-styles";

/// Why a style id did not resolve against the bundle and the project.
///
/// Why: "unknown" and "present but unreadable" need different fixes, and a
/// launch reports either one rather than falling back in silence (#8533).
/// Test: `unknown_id_lists_bundled_and_project_styles`,
/// `an_unreadable_style_file_warns_and_keeps_the_prompt`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProjectStyleError {
    /// Neither a bundled style nor a project style file has this id.
    #[error(transparent)]
    Unknown(#[from] StyleError),
    /// The project style file exists and could not be read.
    #[error("output style '{id}' at {} is unreadable: {source}", path.display())]
    Unreadable {
        /// The requested id.
        id: String,
        /// The style file.
        path: PathBuf,
        /// The read error.
        #[source]
        source: std::io::Error,
    },
    /// The style file is a symlink, or resolves outside the styles directory.
    #[error(
        "output style '{id}' at {} is a symlink or resolves outside {PROJECT_STYLES_DIR}; \
         refusing to inject it",
        path.display()
    )]
    Escapes {
        /// The requested id.
        id: String,
        /// The style file.
        path: PathBuf,
    },
}

/// A resolved output style: bundled, or authored in the project.
///
/// Why: the injection path needs the style's text and the settings writer
/// needs its id, whichever source it came from.
/// What: the bundled registry entry, or the project file's id, path and text.
/// Test: `a_project_style_file_resolves_by_id`.
#[derive(Debug, Clone)]
pub enum ActiveStyle {
    /// A style shipped in the binary.
    Bundled(&'static BundledStyle),
    /// A style file authored in the project.
    Project {
        /// The id — the file stem.
        id: String,
        /// Where the style was read from.
        path: PathBuf,
        /// The file's full text, frontmatter included.
        content: String,
    },
}

impl ActiveStyle {
    /// The style id written to `.claude/settings.json`.
    pub fn id(&self) -> &str {
        match self {
            ActiveStyle::Bundled(style) => style.id,
            ActiveStyle::Project { id, .. } => id,
        }
    }

    /// The style's full text, frontmatter included.
    pub fn content(&self) -> &str {
        match self {
            ActiveStyle::Bundled(style) => style.content,
            ActiveStyle::Project { content, .. } => content,
        }
    }

    /// One-line description of where the style came from, for reports.
    ///
    /// A project style is named `<id> (project) + floor`: the launch appends
    /// the trusty-mpm floor to it (#8533, [`super::style_floor`]).
    /// Test: `a_manifest_only_style_is_named_alike_by_the_report_and_the_launch`.
    pub fn describe(&self) -> String {
        match self {
            ActiveStyle::Bundled(style) => format!("{} (bundled)", style.id),
            ActiveStyle::Project { id, path, .. } => {
                format!("{id} (project) + floor, file {}", path.display())
            }
        }
    }
}

/// Whether `id` is usable as a file stem under [`PROJECT_STYLES_DIR`].
///
/// Why: the id comes from a flag or a committed config file and becomes a
/// path; a separator or a leading dot would let it name a file outside the
/// styles directory.
fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Whether `path` is a regular (non-symlink) file under the project's
/// [`PROJECT_STYLES_DIR`], canonically.
///
/// Why (#8533): `read_to_string` follows symlinks, so a style file linked to a
/// secret elsewhere on the host would be injected into the PM prompt. The
/// styles directory is resolved from the canonical project root, so a symlinked
/// `.claude` or `.claude/output-styles` directory escapes too. A path that does
/// not exist passes: the read then reports it as unknown, which is the right
/// message.
/// Test: `a_symlinked_style_file_is_refused_and_the_default_used`,
/// `a_style_in_a_symlinked_styles_dir_is_refused`.
fn stays_in_styles_dir(project_dir: &Path, path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return true;
    };
    if meta.file_type().is_symlink() {
        return false;
    }
    match (
        std::fs::canonicalize(project_dir),
        std::fs::canonicalize(path),
    ) {
        (Ok(root), Ok(path)) => path.starts_with(root.join(PROJECT_STYLES_DIR)),
        _ => false,
    }
}

/// Ids of the project style files that are not bundled ids, sorted.
///
/// Why: the unknown-id message lists what the operator could have meant, and
/// the bundled copies tm deploys into the same directory are already listed.
/// Test: `unknown_id_lists_bundled_and_project_styles`.
pub fn project_style_ids(project_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(project_dir.join(PROJECT_STYLES_DIR)) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_suffix(".md").map(str::to_string)
        })
        .filter(|id| {
            is_safe_id(id)
                && !super::is_composite_style_id(id)
                && !OUTPUT_STYLES.iter().any(|s| s.id == id)
        })
        .collect();
    ids.sort();
    ids
}

/// Resolve `id` against the bundled registry, then the project's style files.
///
/// Why: a bundled id keeps meaning the shipped text — tm deploys those files
/// into the project itself, so the project copy is a deployment artifact, not an
/// override. Any other id must name a project file.
/// What: bundled entry, else `<project>/.claude/output-styles/<id>.md` read
/// whole; [`ProjectStyleError::Escapes`] when that file is a symlink or its
/// canonical path leaves the canonical styles directory (#8533: the file's
/// text is injected into the prompt, so it must be the project's own);
/// [`ProjectStyleError::Unreadable`] when it exists but cannot be read;
/// [`ProjectStyleError::Unknown`] listing bundled and project ids otherwise.
/// Test: `a_symlinked_style_file_is_refused_and_the_default_used`,
/// `a_project_style_file_resolves_by_id`,
/// `unknown_id_lists_bundled_and_project_styles`, `a_path_like_id_is_refused`,
/// `an_unreadable_style_file_warns_and_keeps_the_prompt`.
pub fn resolve_style_in_project(
    project_dir: &Path,
    id: &str,
) -> Result<ActiveStyle, ProjectStyleError> {
    if let Ok(style) = resolve_style(id) {
        return Ok(ActiveStyle::Bundled(style));
    }
    // #8533: a generated composite already carries the floor; selecting it as a
    // project style would append a second one.
    if is_safe_id(id) && !super::is_composite_style_id(id) {
        let dir = project_dir.join(PROJECT_STYLES_DIR);
        let path = dir.join(format!("{id}.md"));
        if !stays_in_styles_dir(project_dir, &path) {
            return Err(ProjectStyleError::Escapes {
                id: id.to_string(),
                path,
            });
        }
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                return Ok(ActiveStyle::Project {
                    id: id.to_string(),
                    path,
                    content,
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            // #8533: present but unreadable is its own report, not "unknown".
            Err(source) => {
                return Err(ProjectStyleError::Unreadable {
                    id: id.to_string(),
                    path,
                    source,
                });
            }
        }
    }
    let mut valid = vec![super::valid_style_ids()];
    valid.extend(project_style_ids(project_dir));
    Err(ProjectStyleError::Unknown(StyleError::Unknown {
        requested: id.to_string(),
        valid: valid.join(", "),
    }))
}

/// The committed `.trusty-mpm.toml` `[style] active`, if the project sets one.
pub fn project_selected_style(project_dir: &Path) -> Option<String> {
    crate::core::project_config::load_or_report(project_dir)?
        .style?
        .active
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
}

/// The effective output-style id for a project, or `None` for the default.
///
/// Why: one precedence chain for the settings writer and the prompt-injection
/// seam, so the two can never select different styles.
/// What: `--style` flag > `.trusty-mpm.toml` `[style] active` > host config
/// `[style] active` > harness manifest `[style] active`.
/// Test: `project_config_style_outranks_host_config`.
pub fn effective_style_id(
    project_dir: &Path,
    explicit: Option<&str>,
    config: &MpmConfig,
    manifest: Option<&str>,
) -> Option<String> {
    let profile = crate::core::session_profile::resolve(project_dir, config);
    style_chain(
        project_dir,
        explicit,
        config,
        || manifest.map(str::to_string),
        profile,
    )
}

/// The precedence chain, reading the manifest tier only when it can win.
fn style_chain(
    project_dir: &Path,
    explicit: Option<&str>,
    config: &MpmConfig,
    manifest: impl FnOnce() -> Option<String>,
    profile: SessionProfile,
) -> Option<String> {
    // #8453: a supervisor session's style is its profile's, above every tier.
    if profile.is_supervisor() {
        return Some(crate::core::session_profile::SUPERVISOR_OUTPUT_STYLE_ID.to_string());
    }
    explicit
        .map(str::to_string)
        .or_else(|| project_selected_style(project_dir))
        .or_else(|| config.style.active.clone())
        .or_else(manifest)
}

/// Resolve `id` (or the default), turning an unknown id into a warning.
///
/// Why: a launch must not fail on a style typo, and it must not hide one
/// either (#8533). The warning is returned so each launch path prints it where
/// its operator looks.
/// What: `None` → the bundled default. A resolvable id → its style. An unknown
/// or unreadable id → the bundled default plus
/// `"<error>; using `trusty-mpm` instead"`.
/// Test: `an_unknown_style_warns_and_uses_the_default`,
/// `an_unreadable_style_file_warns_and_keeps_the_prompt`.
pub fn resolve_or_default(project_dir: &Path, id: Option<&str>) -> (ActiveStyle, Option<String>) {
    let fallback = || {
        OUTPUT_STYLES
            .iter()
            .find(|s| s.id == DEFAULT_OUTPUT_STYLE_ID)
            .map(ActiveStyle::Bundled)
            .unwrap_or(ActiveStyle::Bundled(&OUTPUT_STYLES[0]))
    };
    let Some(id) = id else {
        return (fallback(), None);
    };
    match resolve_style_in_project(project_dir, id) {
        Ok(style) => (style, None),
        Err(err) => {
            let warning = format!("{err}; using `{DEFAULT_OUTPUT_STYLE_ID}` instead");
            tracing::warn!("{warning}");
            (fallback(), Some(warning))
        }
    }
}

/// The style a launch selected: the chosen id, the resolved style, any warning.
#[derive(Debug, Clone)]
pub struct SelectedStyle {
    /// The id the precedence chain chose; `None` means the default.
    pub id: Option<String>,
    /// The style that id resolved to, or the default after a warning.
    pub style: ActiveStyle,
    /// Why the chosen id fell back to the default, when it did.
    pub warning: Option<String>,
}

/// Select and resolve the output style — the one function every path uses.
///
/// Why: `tm sessions instructions` named a different style from the launch
/// whenever only the manifest tier set one, because each path assembled the
/// chain itself (#8533). The launch and the report now both call this.
/// What: the [`effective_style_id`] chain (flag > `.trusty-mpm.toml` > host
/// config > manifest; `manifest` is called only when no higher tier sets a
/// style), then [`resolve_or_default`] (unknown or unreadable → the default
/// plus a warning). #8453: a supervisor `profile` always selects the
/// supervisor style; any other session that names it gets the default plus a
/// warning, because the supervisor style carries no delegation floor.
/// Test: `a_manifest_only_style_is_named_alike_by_the_report_and_the_launch`,
/// `a_pm_project_cannot_select_the_supervisor_style`.
pub fn select_style(
    project_dir: &Path,
    explicit: Option<&str>,
    config: &MpmConfig,
    manifest: impl FnOnce() -> Option<String>,
    profile: SessionProfile,
) -> SelectedStyle {
    let id = style_chain(project_dir, explicit, config, manifest, profile);
    let supervisor_id = crate::core::session_profile::SUPERVISOR_OUTPUT_STYLE_ID;
    if id.as_deref() == Some(supervisor_id) && !profile.is_supervisor() {
        let warning = format!(
            "output style '{supervisor_id}' is for the supervisor profile only (set \
             `profile = \"supervisor\"` in .trusty-mpm.toml and allow-list the project under \
             `[supervisor] projects` in ~/.trusty-mpm/config.toml); using \
             `{DEFAULT_OUTPUT_STYLE_ID}` instead"
        );
        tracing::warn!("{warning}");
        let (style, _) = resolve_or_default(project_dir, None);
        return SelectedStyle {
            id,
            style,
            warning: Some(warning),
        };
    }
    let (style, warning) = resolve_or_default(project_dir, id.as_deref());
    SelectedStyle { id, style, warning }
}

/// The harness manifest's `[style] active` for a launch in `project_dir`.
///
/// Why: the launch reads this tier from its `HarnessPlan`; a caller with no
/// plan resolves the same manifest sources the launch does.
/// What: `ManifestSources::resolve` against the framework's catalog root, then
/// the merged manifest's style id — the value `HarnessPlan::style` carries.
/// Test: `a_manifest_only_style_is_named_alike_by_the_report_and_the_launch`.
pub fn manifest_style_id(framework_root: &Path, project_dir: &Path) -> Option<String> {
    let catalog_root = crate::content::catalog_root_for(framework_root);
    let sources = crate::core::manifest::ManifestSources::resolve(project_dir, &catalog_root);
    crate::core::manifest::resolve_manifest(&sources)
        .style
        .and_then(|s| s.active)
}

/// [`select_style`] with the host config and manifest read from `framework_root`.
///
/// Why: the report and the prompt-injection seam have no preloaded config or
/// plan; this reads both from the same root the launch reads them from.
/// What: `MpmConfig::load(framework_root)` and [`manifest_style_id`], passed to
/// [`select_style`] with the profile [`crate::core::session_profile::resolve`]
/// derives from that config.
/// Test: `a_manifest_only_style_is_named_alike_by_the_report_and_the_launch`.
pub fn select_style_under(
    framework_root: &Path,
    project_dir: &Path,
    explicit: Option<&str>,
) -> SelectedStyle {
    select_style_under_as(framework_root, project_dir, explicit, None)
}

/// [`select_style_under`] for a profile the caller already resolved (#8453).
pub fn select_style_under_for(
    framework_root: &Path,
    project_dir: &Path,
    explicit: Option<&str>,
    profile: SessionProfile,
) -> SelectedStyle {
    select_style_under_as(framework_root, project_dir, explicit, Some(profile))
}

/// The body of [`select_style_under`]; `None` resolves the profile here.
fn select_style_under_as(
    framework_root: &Path,
    project_dir: &Path,
    explicit: Option<&str>,
    profile: Option<SessionProfile>,
) -> SelectedStyle {
    let config = MpmConfig::load(framework_root);
    let profile =
        profile.unwrap_or_else(|| crate::core::session_profile::resolve(project_dir, &config));
    select_style(
        project_dir,
        explicit,
        &config,
        || manifest_style_id(framework_root, project_dir),
        profile,
    )
}

/// One line naming the style a launch in `project_dir` uses, or the warning.
///
/// Why: `tm sessions instructions` is where an operator checks what a session
/// receives; the style is half of that (#8533).
/// What: [`select_style_under`], rendered as
/// `output style: <id> (bundled)` or `<id> (project) + floor, file <path>`, plus a `warning:` line
/// when the selected id is unknown or unreadable.
/// Test: `instructions_reports_section_status_and_project_style`,
/// `a_manifest_only_style_is_named_alike_by_the_report_and_the_launch`.
pub fn describe_effective_style(framework_root: &Path, project_dir: &Path) -> String {
    let SelectedStyle { style, warning, .. } =
        select_style_under(framework_root, project_dir, None);
    let mut out = format!("output style: {}\n", style.describe());
    if let Some(warning) = warning {
        out.push_str(&format!("warning: {warning}\n"));
    }
    out
}

#[cfg(test)]
#[path = "output_style_project_tests.rs"]
mod tests;
