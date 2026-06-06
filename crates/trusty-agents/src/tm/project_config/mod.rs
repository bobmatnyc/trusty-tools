//! Per-project TOML config under `.trusty-agents/projects/<name>.toml`.
//!
//! Why: The `/connect` REPL command and `tell` routing need a stable per-project
//! definition (path, default harness, declared harnesses) that survives
//! restarts and is human-editable. Putting it under `.trusty-agents/projects/`
//! mirrors the agent/skill/workflow layout users already understand.
//! What: Defines `ProjectConfig` and `HarnessConfig` types plus load/save/
//! list helpers rooted at a base directory (typically the user's
//! `.trusty-agents/projects/` path). Configs are TOML with `[project]` and
//! repeated `[[harnesses]]` tables. A project's session-name serial counter
//! is computed from the live TM registry rather than stored here, so
//! configs stay declarative.
//! Test: See the `tests` module below — covers round-trip, list, default
//! harness lookup, and basename-defaulting.
//!
//! Example file:
//! ```toml
//! [project]
//! name = "trusty-agents"
//! path = "/Users/masa/Projects/trusty-agents"
//! default_harness = "repl"
//!
//! [[harnesses]]
//! name = "repl"
//! startup_command = "om"
//! adapter = "claude-mpm"
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// One declared harness inside a project config.
///
/// Why: A project can be driven by multiple harnesses (`repl`, `bash`,
/// `claude-code` review pane, etc.); each gets its own startup command and
/// adapter so `tell <project>:<harness>` can find the right session.
/// What: `name` is the harness label used in `<project>-<harness>-<serial>`
/// and `tell <project>:<harness>`; `adapter` maps to `AdapterType::from_id`.
/// Test: `test_roundtrip_with_harnesses`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HarnessConfig {
    pub name: String,
    /// Shell command spawned when `/connect` first creates a session for
    /// this harness (e.g., `"om"`, `"claude"`, `"bash"`).
    pub startup_command: String,
    /// Adapter id string (matches `AdapterType::from_id` — `"claude-mpm"`,
    /// `"claude-code"`, `"shell"`, etc.).
    pub adapter: String,
}

/// The `[project]` table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectMeta {
    pub name: String,
    pub path: PathBuf,
    /// Name of the harness used by `tell <project> "..."` (no `:harness`).
    /// Optional so brand-new projects can be created and harnesses added
    /// later without a chicken-and-egg.
    #[serde(default)]
    pub default_harness: Option<String>,
}

/// Full project config (one TOML file under `.trusty-agents/projects/`).
///
/// Why: Wraps the `[project]` meta and the `[[harnesses]]` array so the
/// whole file deserializes in one shot. New fields are added with
/// `#[serde(default)]` to keep older files loadable.
/// What: `project` is the meta; `harnesses` is a flat list (lookup is O(n)
/// because the number of harnesses per project is tiny).
/// Test: `test_roundtrip_with_harnesses`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectConfig {
    pub project: ProjectMeta,
    #[serde(default)]
    pub harnesses: Vec<HarnessConfig>,
}

impl ProjectConfig {
    /// Create a fresh config rooted at `path`. Defaults the project name to
    /// the directory's basename (matching the `/connect` spec where the
    /// optional `[name]` arg defaults to `basename(path)`).
    pub fn new(path: PathBuf) -> Self {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("project")
            .to_string();
        Self {
            project: ProjectMeta {
                name,
                path,
                default_harness: None,
            },
            harnesses: Vec::new(),
        }
    }

    /// Find a harness by name (case-sensitive — matches the `tell` syntax
    /// which uses raw identifiers).
    pub fn find_harness(&self, name: &str) -> Option<&HarnessConfig> {
        self.harnesses.iter().find(|h| h.name == name)
    }

    /// Resolve the harness for a `tell` invocation.
    ///
    /// Why: `tell <project>` uses `default_harness`; `tell <project>:<harness>`
    /// is explicit. Centralizing the lookup so callers (REST, REPL) share
    /// the same rules.
    /// What: When `explicit` is Some, returns that harness or errors if
    /// unknown. When None, falls back to `default_harness`; errors if no
    /// default is set or the default points to an unknown harness.
    /// Test: `test_resolve_harness_default_and_explicit`.
    pub fn resolve_harness(&self, explicit: Option<&str>) -> Result<&HarnessConfig> {
        match explicit {
            Some(name) => self.find_harness(name).with_context(|| {
                format!(
                    "harness '{}' not declared for project '{}'",
                    name, self.project.name
                )
            }),
            None => {
                let default = self.project.default_harness.as_deref().with_context(|| {
                    format!(
                        "project '{}' has no default_harness; use `<project>:<harness>` to disambiguate",
                        self.project.name
                    )
                })?;
                self.find_harness(default).with_context(|| {
                    format!(
                        "default_harness '{}' is not declared in project '{}'",
                        default, self.project.name
                    )
                })
            }
        }
    }
}

/// Directory-rooted store for project TOML files.
///
/// Why: Centralizes path handling and atomic writes so callers don't poke
/// at the directory layout directly. Mirrors `TmSessionRegistry` in spirit
/// but operates on one file per project.
/// What: Holds the base directory (e.g., `<project>/.trusty-agents/projects/`)
/// and exposes `load`, `save`, `list`, and `delete` operations.
/// Test: see `tests` below.
pub struct ProjectConfigStore {
    base: PathBuf,
}

impl ProjectConfigStore {
    /// Open or initialize the store rooted at `base`.
    pub fn open(base: &Path) -> Result<Self> {
        std::fs::create_dir_all(base).with_context(|| format!("creating {}", base.display()))?;
        Ok(Self {
            base: base.to_path_buf(),
        })
    }

    fn config_path(&self, name: &str) -> PathBuf {
        self.base.join(format!("{}.toml", name))
    }

    /// Load a project config by name; returns Ok(None) if the file does not
    /// exist so callers can treat "no config" as a non-error.
    pub fn load(&self, name: &str) -> Result<Option<ProjectConfig>> {
        let path = self.config_path(name);
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let cfg: ProjectConfig =
            toml::from_str(&content).with_context(|| format!("parsing {}", path.display()))?;
        Ok(Some(cfg))
    }

    /// Save the config atomically (tmp + rename) so a crash mid-write can't
    /// leave a truncated TOML on disk.
    pub fn save(&self, cfg: &ProjectConfig) -> Result<()> {
        if cfg.project.name.is_empty() {
            bail!("project name must not be empty");
        }
        let path = self.config_path(&cfg.project.name);
        let content = toml::to_string_pretty(cfg)
            .with_context(|| format!("serializing project '{}'", cfg.project.name))?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, content).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()))?;
        Ok(())
    }

    /// List all project configs. Skips files that don't parse so a single
    /// bad TOML doesn't break listing — callers can re-load by name for
    /// detailed error reporting.
    pub fn list(&self) -> Result<Vec<ProjectConfig>> {
        if !self.base.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.base)
            .with_context(|| format!("listing {}", self.base.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("toml") {
                continue;
            }
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            if let Ok(cfg) = toml::from_str::<ProjectConfig>(&content) {
                out.push(cfg);
            }
        }
        Ok(out)
    }

    /// Remove a config file by project name. No-op if absent.
    pub fn delete(&self, name: &str) -> Result<()> {
        let path = self.config_path(name);
        if path.exists() {
            std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        }
        Ok(())
    }

    /// Find an existing project config whose `project.path` matches `path`.
    ///
    /// Why: `/connect <path> <adapter>` and the WebUI "Add Project" form both
    /// need to ask "is this directory already registered?" before deciding to
    /// create vs. reuse. Centralizing the lookup keeps the policy in one place
    /// so CLI and HTTP behave identically.
    /// What: Lists every config under the base dir and returns the first whose
    /// `project.path` equals `path` (path comparison is exact — callers should
    /// canonicalize first to avoid `/tmp/foo` vs `/private/tmp/foo` misses).
    /// Test: `test_find_by_path_returns_existing` and
    /// `test_find_by_path_returns_none_when_missing`.
    pub fn find_by_path(&self, path: &Path) -> Result<Option<ProjectConfig>> {
        for cfg in self.list()? {
            if cfg.project.path == path {
                return Ok(Some(cfg));
            }
        }
        Ok(None)
    }

    /// Look up a project config by path, or auto-create one rooted at `path`.
    ///
    /// Why: Implements step 1 of the refined `/connect` spec — "if no project
    /// exists for that path, auto-create project (name = dirname,
    /// default_harness = adapter)". Both the REPL slash command and the WebUI
    /// "Add Project" form converge on this single helper so the on-disk shape
    /// is identical regardless of entry point.
    /// What:
    ///   1. `find_by_path(path)` — short-circuit if a config already exists.
    ///   2. Build a fresh `ProjectConfig` rooted at `path`. `name_override`
    ///      wins when supplied; otherwise the basename is used (same default
    ///      as `ProjectConfig::new`).
    ///   3. Register a default harness named after the adapter id, with
    ///      `startup_command = startup_cmd_for(adapter)` and
    ///      `default_harness = adapter_id` so `tell <project>` works out of
    ///      the box.
    ///   4. Save and return the new config.
    /// Test: `test_find_or_create_creates_when_missing`,
    /// `test_find_or_create_returns_existing_unchanged`,
    /// `test_find_or_create_honors_name_override`.
    pub fn find_or_create(
        &self,
        path: &Path,
        adapter_id: &str,
        name_override: Option<&str>,
    ) -> Result<ProjectConfig> {
        if let Some(existing) = self.find_by_path(path)? {
            return Ok(existing);
        }
        let name = name_override.map(str::to_string).unwrap_or_else(|| {
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("project")
                .to_string()
        });
        let harness = HarnessConfig {
            name: adapter_id.to_string(),
            startup_command: default_startup_command_for(adapter_id).to_string(),
            adapter: adapter_id.to_string(),
        };
        let cfg = ProjectConfig {
            project: ProjectMeta {
                name,
                path: path.to_path_buf(),
                default_harness: Some(adapter_id.to_string()),
            },
            harnesses: vec![harness],
        };
        self.save(&cfg)?;
        Ok(cfg)
    }
}

/// Return the default startup command for an adapter id.
///
/// Why: `/connect <path> <adapter>` and the WebUI form both need to seed a
/// project's harness with a reasonable shell command without asking the user
/// to type it. Keeping the table here means new adapters opt in by adding a
/// match arm rather than duplicating the mapping across CLI and HTTP.
/// What: Maps adapter ids (`AdapterType::as_str` values) to their canonical
/// invocation. Unknown ids fall back to `bash` — safer than panicking or
/// inventing a command.
/// Test: `test_default_startup_command_for_known_adapters` and
/// `test_default_startup_command_falls_back_to_bash`.
pub fn default_startup_command_for(adapter_id: &str) -> &'static str {
    match adapter_id {
        "claude-mpm" => "claude-mpm",
        "claude-code" => "claude",
        "codex" => "codex",
        "augment" => "augment",
        "gemini" => "gemini",
        "trusty-agents" => "om",
        "shell" => "bash",
        _ => "bash",
    }
}

/// Compute the next session name for `<project>-<harness>` using the live
/// session list as the source of truth for the serial counter.
///
/// Why: Per the refined `/connect` spec, sessions are named
/// `<project>-<harness>-<serial>` and serials auto-increment per
/// `(project, harness)` pair. Computing from `existing_names` (rather than
/// storing a counter) means renames/deletes can't desync the serial.
/// What: Scans `existing_names` for entries matching `<project>-<harness>-<N>`,
/// finds the highest `N`, and returns `<project>-<harness>-(max+1)` (or
/// `<project>-<harness>-1` if none exist).
/// Test: `test_next_session_name`.
pub fn next_session_name(project: &str, harness: &str, existing_names: &[String]) -> String {
    let prefix = format!("{}-{}-", project, harness);
    let mut max_serial: u32 = 0;
    for name in existing_names {
        if let Some(rest) = name.strip_prefix(&prefix)
            && let Ok(n) = rest.parse::<u32>()
            && n > max_serial
        {
            max_serial = n;
        }
    }
    format!("{}{}", prefix, max_serial + 1)
}

#[cfg(test)]
mod tests;
