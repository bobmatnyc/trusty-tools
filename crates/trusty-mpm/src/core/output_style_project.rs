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

/// Project-relative directory holding project output styles.
pub const PROJECT_STYLES_DIR: &str = ".claude/output-styles";

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
    pub fn describe(&self) -> String {
        match self {
            ActiveStyle::Bundled(style) => format!("{} (bundled)", style.id),
            ActiveStyle::Project { id, path, .. } => {
                format!("{id} (project file {})", path.display())
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
        .filter(|id| is_safe_id(id) && !OUTPUT_STYLES.iter().any(|s| s.id == id))
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
/// whole; [`StyleError::Unknown`] listing bundled and project ids otherwise.
/// Test: `a_project_style_file_resolves_by_id`,
/// `unknown_id_lists_bundled_and_project_styles`, `a_path_like_id_is_refused`.
pub fn resolve_style_in_project(project_dir: &Path, id: &str) -> Result<ActiveStyle, StyleError> {
    if let Ok(style) = resolve_style(id) {
        return Ok(ActiveStyle::Bundled(style));
    }
    if is_safe_id(id) {
        let path = project_dir.join(PROJECT_STYLES_DIR).join(format!("{id}.md"));
        if let Ok(content) = std::fs::read_to_string(&path) {
            return Ok(ActiveStyle::Project {
                id: id.to_string(),
                path,
                content,
            });
        }
    }
    let mut valid = vec![super::valid_style_ids()];
    valid.extend(project_style_ids(project_dir));
    Err(StyleError::Unknown {
        requested: id.to_string(),
        valid: valid.join(", "),
    })
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
    explicit
        .map(str::to_string)
        .or_else(|| project_selected_style(project_dir))
        .or_else(|| config.style.active.clone())
        .or_else(|| manifest.map(str::to_string))
}

/// Resolve `id` (or the default), turning an unknown id into a warning.
///
/// Why: a launch must not fail on a style typo, and it must not hide one
/// either (#8533). The warning is returned so each launch path prints it where
/// its operator looks.
/// What: `None` → the bundled default. A resolvable id → its style. An unknown
/// id → the bundled default plus `"<error>; using `trusty-mpm` instead"`.
/// Test: `an_unknown_style_warns_and_uses_the_default`.
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

/// One line naming the style a launch in `project_dir` uses, or the warning.
///
/// Why: `tm sessions instructions` is where an operator checks what a session
/// receives; the style is half of that (#8533).
/// What: `output style: <id> (bundled|project file <path>)`, plus a
/// `warning:` line when the selected id is unknown.
/// Test: `instructions_reports_section_status_and_project_style`.
pub fn describe_effective_style(project_dir: &Path) -> String {
    let config = MpmConfig::load_default();
    let id = effective_style_id(project_dir, None, &config, None);
    let (style, warning) = resolve_or_default(project_dir, id.as_deref());
    let mut out = format!("output style: {}\n", style.describe());
    if let Some(warning) = warning {
        out.push_str(&format!("warning: {warning}\n"));
    }
    out
}

#[cfg(test)]
#[path = "output_style_project_tests.rs"]
mod tests;
