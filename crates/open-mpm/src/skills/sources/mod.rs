//! Configurable skill sources for `.open-mpm/skill-sources.toml` (#172).
//!
//! Why: `skill_search_paths()` in `registry.rs` is hard-coded, so operators
//! can't add an extra local skills directory or pull in a remote git
//! repository of curated skills without recompiling. Treating skill sources
//! like agent registries (priority-ordered, declarative) brings parity with
//! the rest of the harness and lets teams share skill libraries via
//! version-controlled URLs.
//! What: Defines `SkillSource` (one entry) and `SkillSourceRegistry` (the
//! full ordered list parsed from `.open-mpm/skill-sources.toml`). Provides
//! resolved on-disk paths (handling `~` expansion + remote-git cache dirs)
//! and `ensure_remote_sources()` to clone or fast-forward each remote source
//! before scanning. Sources flagged `approved = false` log a WARN at startup.
//! Test: See unit tests at the bottom of this module.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

/// One configured skill source — either a local directory or a remote git
/// repository whose contents are mirrored under `~/.open-mpm/skills/cache/`.
///
/// Why: Treating local + remote sources with one shape simplifies the rest of
/// the pipeline; the consumer just calls `resolved_paths()` and walks.
/// What: All optional fields are populated from the TOML during deserialization;
/// `name`, `url`, `branch`, and `skills_subdir` are only meaningful for
/// `RemoteGit` sources.
/// Test: `sources_load_defaults_when_file_missing`,
/// `remote_git_cache_path_computed_correctly`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillSource {
    #[serde(rename = "type")]
    pub source_type: SkillSourceType,
    /// Local path (relative to project root or absolute, may include `~`).
    /// Required for `Local`, ignored for `RemoteGit`.
    #[serde(default)]
    pub path: Option<String>,
    /// Friendly identifier; required for `RemoteGit` (used as cache dir name).
    #[serde(default)]
    pub name: Option<String>,
    /// HTTPS or SSH git URL. Required for `RemoteGit`.
    #[serde(default)]
    pub url: Option<String>,
    /// Branch to track. Defaults to `main`.
    #[serde(default = "default_branch")]
    pub branch: String,
    /// Optional subdirectory inside the cloned repo that holds the skills.
    #[serde(default)]
    pub skills_subdir: Option<String>,
    /// Higher-priority sources are scanned first; first writer wins on name
    /// collision (matches the existing `SkillRegistry::load` rule).
    #[serde(default = "default_priority")]
    pub priority: i32,
    /// Disabled sources are loaded silently and excluded from the resolved
    /// path list — useful for keeping templates checked in while not active.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Approval flag for remote/external sources. `false` triggers a WARN at
    /// startup (MVP: warn only, do not block).
    #[serde(default = "default_approved")]
    pub approved: bool,
}

fn default_branch() -> String {
    "main".to_string()
}
fn default_priority() -> i32 {
    0
}
fn default_enabled() -> bool {
    true
}
fn default_approved() -> bool {
    true
}

/// Discriminator for the two supported source types.
///
/// Why: Local + remote-git is the minimum viable surface; richer types (S3,
/// HTTP zip, etc.) can join the enum later without breaking the file format.
/// What: Serialized as `local` / `remote_git` to keep the TOML readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillSourceType {
    Local,
    RemoteGit,
}

/// Top-level deserialization shape for `.open-mpm/skill-sources.toml`.
#[derive(Debug, Default, Deserialize)]
struct SkillSourcesFile {
    #[serde(default)]
    sources: Vec<SkillSource>,
}

/// Ordered registry of configured sources.
///
/// Why: Centralizes "where do skills come from" so `main.rs`, the CLI, and
/// future plugins all share one resolution policy. Built once at startup.
/// What: Parses the TOML file (or returns sensible defaults), sorts by
/// `priority` descending, and exposes `resolved_paths()` for the registry.
/// Test: `sources_load_defaults_when_file_missing`,
/// `sources_resolved_paths_expands_tilde`.
#[derive(Debug, Clone)]
pub struct SkillSourceRegistry {
    sources: Vec<SkillSource>,
    project_root: PathBuf,
}

impl SkillSourceRegistry {
    /// Load `<project_root>/.open-mpm/skill-sources.toml` or fall back to
    /// built-in defaults (project + user `.open-mpm/skills`).
    ///
    /// Why: First-run installs and bare projects must "just work"; requiring
    /// every project to ship a config file would defeat the bundled-skills
    /// experience.
    /// What: Reads the TOML file when present; on parse error, logs WARN and
    /// returns defaults so a malformed file never blocks startup. Sources are
    /// stable-sorted by `priority` descending.
    /// Test: `sources_load_defaults_when_file_missing`,
    /// `sources_load_handles_malformed_file`.
    pub fn load(project_root: &Path) -> Self {
        let config_path = project_root.join(".open-mpm").join("skill-sources.toml");
        let mut sources = match std::fs::read_to_string(&config_path) {
            Ok(text) => match toml::from_str::<SkillSourcesFile>(&text) {
                Ok(parsed) => {
                    tracing::debug!(
                        path = %config_path.display(),
                        count = parsed.sources.len(),
                        "skill-sources.toml loaded"
                    );
                    parsed.sources
                }
                Err(e) => {
                    tracing::warn!(
                        path = %config_path.display(),
                        error = %e,
                        "skill-sources.toml malformed; using defaults"
                    );
                    default_sources()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(
                    path = %config_path.display(),
                    "skill-sources.toml missing; using defaults"
                );
                default_sources()
            }
            Err(e) => {
                tracing::warn!(
                    path = %config_path.display(),
                    error = %e,
                    "skill-sources.toml unreadable; using defaults"
                );
                default_sources()
            }
        };

        // Stable sort: higher priority wins, ties keep config order.
        sources.sort_by_key(|b| std::cmp::Reverse(b.priority));

        // Warn (do not block) on unapproved enabled sources.
        for s in &sources {
            if s.enabled && !s.approved {
                let label = s
                    .url
                    .as_deref()
                    .or(s.name.as_deref())
                    .or(s.path.as_deref())
                    .unwrap_or("<unnamed>");
                tracing::warn!(
                    source = %label,
                    "Unapproved skill source: {label} — use --allow-unapproved to enable"
                );
            }
        }

        Self {
            sources,
            project_root: project_root.to_path_buf(),
        }
    }

    /// Build directly from an explicit list (test + programmatic use).
    #[cfg(test)]
    pub fn from_sources(project_root: &Path, sources: Vec<SkillSource>) -> Self {
        let mut sources = sources;
        sources.sort_by_key(|b| std::cmp::Reverse(b.priority));
        Self {
            sources,
            project_root: project_root.to_path_buf(),
        }
    }

    /// Borrow the underlying ordered source list.
    pub fn sources(&self) -> &[SkillSource] {
        &self.sources
    }

    /// Compute the on-disk paths to feed `SkillRegistry::load`.
    ///
    /// Why: Source definitions may use `~`, relative paths, or refer to a
    /// remote-git cache directory; the registry just wants concrete dirs.
    /// What: Skips disabled sources. For `Local`, expands `~` and resolves
    /// relative paths against `project_root`. For `RemoteGit`, points at the
    /// computed cache dir + `skills_subdir` (when set).
    /// Test: `sources_resolved_paths_expands_tilde`,
    /// `sources_disabled_source_excluded`.
    pub fn resolved_paths(&self) -> Vec<PathBuf> {
        let mut out = Vec::with_capacity(self.sources.len());
        for source in &self.sources {
            if !source.enabled {
                continue;
            }
            match source.source_type {
                SkillSourceType::Local => {
                    if let Some(raw) = source.path.as_deref() {
                        out.push(resolve_local_path(raw, &self.project_root));
                    }
                }
                SkillSourceType::RemoteGit => {
                    if let Some(cache_dir) = remote_cache_dir(source) {
                        let final_dir = match source.skills_subdir.as_deref() {
                            Some(sub) if !sub.is_empty() => cache_dir.join(sub),
                            _ => cache_dir,
                        };
                        out.push(final_dir);
                    }
                }
            }
        }
        out
    }

    /// Clone or fast-forward every enabled `RemoteGit` source.
    ///
    /// Why: Operators expect remote skill libraries to stay current without
    /// manual `git pull` between runs; baking a refresh step into startup
    /// keeps the loop tight.
    /// What: For each enabled `RemoteGit` source: if the cache dir is absent,
    /// `git clone --depth 1 --branch <branch> <url> <cache>`. Otherwise
    /// `git -C <cache> fetch && git -C <cache> reset --hard origin/<branch>`
    /// (forced FF; we own the cache dir). Errors are logged and the source
    /// is skipped — a bad clone must not abort the whole run. Skips
    /// unapproved sources entirely (warn-only MVP also means no fetching).
    /// Test: Indirect — manual sanity check; the unit tests exercise path
    /// computation and config loading.
    pub fn ensure_remote_sources(&self) -> anyhow::Result<()> {
        for source in &self.sources {
            if !source.enabled || source.source_type != SkillSourceType::RemoteGit {
                continue;
            }
            if !source.approved {
                tracing::warn!(
                    name = source.name.as_deref().unwrap_or("<unnamed>"),
                    "skipping unapproved remote source"
                );
                continue;
            }
            let Some(url) = source.url.as_deref() else {
                tracing::warn!(
                    name = source.name.as_deref().unwrap_or("<unnamed>"),
                    "remote_git source missing url; skipping"
                );
                continue;
            };
            let Some(cache_dir) = remote_cache_dir(source) else {
                tracing::warn!(
                    url = %url,
                    "remote_git source missing name; cannot compute cache path"
                );
                continue;
            };

            if cache_dir.exists() {
                if let Err(e) = git_fast_forward(&cache_dir, &source.branch) {
                    tracing::warn!(
                        url = %url,
                        cache = %cache_dir.display(),
                        error = %e,
                        "remote_git: fast-forward failed; using stale cache"
                    );
                }
            } else if let Some(parent) = cache_dir.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    tracing::warn!(
                        path = %parent.display(),
                        error = %e,
                        "remote_git: cache parent dir creation failed; skipping"
                    );
                    continue;
                }
                if let Err(e) = git_clone(url, &source.branch, &cache_dir) {
                    tracing::warn!(
                        url = %url,
                        cache = %cache_dir.display(),
                        error = %e,
                        "remote_git: clone failed; source unavailable"
                    );
                }
            }
        }
        Ok(())
    }
}

/// Built-in defaults applied when no `skill-sources.toml` is present.
///
/// Why: Mirrors the previous hard-coded behavior so existing installs keep
/// working unchanged.
/// What: Project-local `.open-mpm/skills` (priority 10) + user-level
/// `~/.open-mpm/skills` (priority 5).
fn default_sources() -> Vec<SkillSource> {
    vec![
        SkillSource {
            source_type: SkillSourceType::Local,
            path: Some(".open-mpm/skills".to_string()),
            name: None,
            url: None,
            branch: default_branch(),
            skills_subdir: None,
            priority: 10,
            enabled: true,
            approved: true,
        },
        SkillSource {
            source_type: SkillSourceType::Local,
            path: Some("~/.open-mpm/skills".to_string()),
            name: None,
            url: None,
            branch: default_branch(),
            skills_subdir: None,
            priority: 5,
            enabled: true,
            approved: true,
        },
    ]
}

/// Expand `~` and resolve relative paths against `project_root`.
fn resolve_local_path(raw: &str, project_root: &Path) -> PathBuf {
    let expanded = if let Some(stripped) = raw.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            home.join(stripped)
        } else {
            PathBuf::from(raw)
        }
    } else if raw == "~" {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from(raw))
    } else {
        PathBuf::from(raw)
    };

    if expanded.is_absolute() {
        expanded
    } else {
        project_root.join(expanded)
    }
}

/// Cache dir for a remote-git source: `~/.open-mpm/skills/cache/<name>/`.
///
/// Returns `None` when the source has no `name` (cannot disambiguate).
fn remote_cache_dir(source: &SkillSource) -> Option<PathBuf> {
    let name = source.name.as_deref()?;
    let base = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    Some(
        base.join(".open-mpm")
            .join("skills")
            .join("cache")
            .join(name),
    )
}

/// Run `git clone --depth 1 --branch <branch> <url> <dest>`.
fn git_clone(url: &str, branch: &str, dest: &Path) -> anyhow::Result<()> {
    let status = Command::new("git")
        .arg("clone")
        .arg("--depth")
        .arg("1")
        .arg("--branch")
        .arg(branch)
        .arg(url)
        .arg(dest)
        .status()?;
    if !status.success() {
        anyhow::bail!("git clone exited with {status}");
    }
    Ok(())
}

/// Update an existing cached clone to the tip of `<branch>`.
fn git_fast_forward(repo: &Path, branch: &str) -> anyhow::Result<()> {
    let fetch = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("fetch")
        .arg("--depth")
        .arg("1")
        .arg("origin")
        .arg(branch)
        .status()?;
    if !fetch.success() {
        anyhow::bail!("git fetch exited with {fetch}");
    }
    let reset = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("reset")
        .arg("--hard")
        .arg(format!("origin/{branch}"))
        .status()?;
    if !reset.success() {
        anyhow::bail!("git reset exited with {reset}");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
