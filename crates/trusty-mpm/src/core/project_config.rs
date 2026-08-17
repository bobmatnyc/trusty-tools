//! The committed, project-level trusty-mpm config file (#5207).
//!
//! Why: trusty-mpm's settings were split across four surfaces, none of which
//! travel with the repository — `~/.trusty-mpm/config.toml`
//! ([`crate::core::config::MpmConfig`]), `~/.trusty-tools/trusty-mpm/config.yaml`
//! ([`crate::core::trusty_tools_config::TrustyToolsConfig`]), the machine-global
//! `~/.trusty-mpm/project-registry/projects.json`, and `$XDG_CONFIG_HOME`. Every
//! one is per-HOST, so a project's own conventions — "this repo launches on main,
//! never in a worktree" — had to be re-declared by every operator on every
//! machine, and could not be reviewed, versioned, or diffed. The owner ruling
//! (2026-08-08) is that configuration belongs at the PROJECT level and is
//! unitary: if a setting is configurable, every system reads the same value.
//!
//! What: [`PROJECT_CONFIG_FILE`] names one TOML file at the project root that is
//! TRACKED IN GIT — it travels with clones and shows up in PR diffs — and sits at
//! the TOP of every precedence chain it participates in.
//! [`ProjectLevelConfig::from_toml`] parses it with `deny_unknown_fields`, so a
//! misspelled key is an error rather than a silent no-op;
//! [`ProjectLevelConfig::load`] is the fallible loader and
//! [`load_or_report`] the lenient one used on the spawn path.
//!
//! `workspace_root` is deliberately NOT a member of this surface (owner
//! ruling): it decides where a project gets CLONED, so it cannot be read from
//! the project that does not exist yet. It stays host-level in
//! [`crate::core::trusty_tools_config::workspace_root`]. `auto_resume` likewise
//! stays host-level — it is a property of the operator's supervisor, not of the
//! repository.
//!
//! Test: `project_config_parses_worktree`, `project_config_parses_agent_worktree`,
//! `project_config_rejects_unknown_key`,
//! `project_config_absent_is_none`, `project_config_rejects_wrong_type`,
//! `project_config_empty_file_is_all_none` in `project_config_tests.rs`.
//!
//! [`PROJECT_CONFIG_FILE`]: crate::core::project_config::PROJECT_CONFIG_FILE
//! [`ProjectLevelConfig::from_toml`]: crate::core::project_config::ProjectLevelConfig::from_toml
//! [`ProjectLevelConfig::load`]: crate::core::project_config::ProjectLevelConfig::load
//! [`load_or_report`]: crate::core::project_config::load_or_report

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File name of the committed project-level config, at the project root.
///
/// Why: this is a ROOT-LEVEL dotfile rather than a member of the existing
/// `<project>/.trusty-mpm/` directory, and that choice is load-bearing. Owner
/// ruling 4 requires the file to be tracked in git. `.trusty-mpm/` is where
/// trusty-mpm writes machine-local session state (`sessions/`, `logs/`), so
/// projects gitignore it wholesale — this very repository ignores
/// `.trusty-mpm/*`, which already makes the #4832 `framework/manifest.toml`
/// layer untrackable here. Putting a file that MUST be committed inside a
/// directory that exists to hold uncommittable state would require every
/// consumer project to carve a `!` re-include out of its own ignore rules, and
/// trusty-mpm cannot reach into a consumer's `.gitignore`. A root dotfile is
/// trackable by default everywhere, and sits beside the other
/// committed-by-convention project files (`Cargo.toml`, `.gitignore`).
/// What: `.trusty-mpm.toml`.
/// Test: `project_config_path_is_a_root_dotfile`.
pub const PROJECT_CONFIG_FILE: &str = ".trusty-mpm.toml";

/// Why a project-level config could not be used.
///
/// Why: the file is COMMITTED, so a bad edit is pushed to everyone. The error
/// must name the file and carry serde's own message (which identifies the
/// offending key), so the operator who reads the log can fix it without
/// guessing.
/// What: `Io` for a file that exists but cannot be read; `Malformed` for a file
/// that is not valid TOML or carries a key the schema does not define. An ABSENT
/// file is not an error — [`ProjectLevelConfig::load`] returns `Ok(None)`.
/// Test: `project_config_rejects_unknown_key`, `project_config_rejects_wrong_type`.
#[derive(Debug, thiserror::Error)]
pub enum ProjectConfigError {
    /// The file exists but could not be read.
    #[error("could not read {}: {source}", .path.display())]
    Io {
        /// The file that could not be read.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },

    /// The file is not valid TOML, or carries a key the schema does not define.
    #[error("{} is not a valid project config: {source}", .path.display())]
    Malformed {
        /// The offending file.
        path: PathBuf,
        /// serde's parse error, which names the unrecognised or ill-typed key.
        #[source]
        source: toml::de::Error,
    },
}

/// The committed, project-level trusty-mpm configuration.
///
/// Why: one struct per project-level setting keeps the precedence chains honest
/// — each field is the TOP layer of exactly one resolution, and a field that no
/// resolver reads cannot exist unnoticed (the `default_model` orphan this issue
/// also fixes was exactly that failure at the host level).
///
/// `deny_unknown_fields` is applied HERE and deliberately not to the pre-existing
/// host-level config structs. There is no legacy corpus of `.trusty-mpm.toml`
/// files to break, and this file is reviewed in a PR before it reaches anyone
/// else, so a hard rejection is caught by the author rather than suffered by the
/// team. See [`crate::core::config_keys`] for what the host-level structs get
/// instead, and why strictness there would be a regression rather than a fix.
///
/// What: every field is `Option`, so an absent key means "this layer declines to
/// decide" and resolution falls through to the next one down. An empty file is
/// valid and overrides nothing.
/// Test: `project_config_parses_worktree`, `project_config_empty_file_is_all_none`,
/// `project_config_rejects_unknown_key`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
// #5207: a misspelled key in a COMMITTED config must fail loudly, not no-op.
#[serde(deny_unknown_fields)]
pub struct ProjectLevelConfig {
    /// Whether this project's managed sessions get a per-session git worktree.
    ///
    /// Why: the worktree decision is a property of the PROJECT's workflow — a
    /// repo with a direct-to-main flow wants every session in the live checkout
    /// — so it belongs in the repo, not in each operator's machine-global
    /// `projects.json`. This field is the highest-precedence layer of that
    /// decision; see
    /// [`crate::project::worktree_enabled_for_project`] for the full chain.
    /// What: `None` → this project does not decide; the registry layer
    /// (`projects.json`) answers, and failing that the built-in `true`.
    /// `Some(false)` → launch directly in the checkout. `Some(true)` → force
    /// worktree isolation even if an operator's local registry disabled it.
    /// Test: `project_config_parses_worktree`,
    /// `worktree_project_config_overrides_registry`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<bool>,

    /// Whether a dispatched agent in this project gets a worktree of its own.
    ///
    /// Why (#5814): ADR-0048 decision 1 grants every dispatched writer its own
    /// worktree when the session stands in a main checkout, and that grant is
    /// mechanical — it never reads the dispatch prompt, so no instruction can
    /// wave it off. Worktree isolation pays for concurrent writers and separate
    /// build state; a writing or documentation repo has neither, and pays only
    /// the costs: agent edits land in `.claude/worktrees/agent-<id>/` and never
    /// reach the checkout, a second agent has to copy them across, and the trees
    /// and branches pile up needing reclamation. This field is that project
    /// class's opt-out.
    ///
    /// What: `None` (the default) → the grant behaves exactly as it did before
    /// this key existed. `Some(false)` → dispatched agents stay in the main
    /// checkout: nothing is created and nothing needs reclaiming. `Some(true)`
    /// → the default, stated explicitly.
    ///
    /// This is a SEPARATE key from [`Self::worktree`], not a widening of it.
    /// `worktree` decides where a managed SESSION is placed and ADR-0044
    /// decision 6 narrowed its live effect to the daemon-unreachable
    /// provisioning fallback; reusing it here would give one key two unrelated
    /// meanings again — the double duty ADR-0037 called out. Setting this key
    /// exempts the dispatch from the worktree GRANT only: the ADR-0044
    /// main-checkout write boundary is untouched, so a source-file edit there is
    /// still denied.
    /// Test: `project_config_parses_agent_worktree`,
    /// `agent_worktree_opt_out_is_honoured`,
    /// `grants_nothing_when_the_project_opts_out`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_worktree: Option<bool>,

    /// Default model id (or tier alias) for sessions launched in this project.
    ///
    /// Why: model choice is a project economics decision (a docs repo does not
    /// need Opus), and it was previously only expressible per-host. This is the
    /// top layer of the same chain that
    /// [`crate::core::config::resolve_agent_model`] already terminates in, so
    /// setting it here reaches every launch path without a second resolver.
    /// What: `None` → fall through to the host layers. `Some(m)` → `m` becomes
    /// the effective `models.default`, still subject to an explicit `--model`
    /// flag, a per-agent override, and agent frontmatter, all of which are
    /// MORE specific than a default and therefore still win.
    /// Test: `project_default_model_tops_the_chain`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
}

impl ProjectLevelConfig {
    /// Parse a project config from TOML text.
    ///
    /// Why: separated from the file read so the schema contract — above all the
    /// `deny_unknown_fields` rejection — is assertable without touching a
    /// filesystem.
    /// What: `Ok(cfg)` for valid TOML whose every key is defined by this struct;
    /// `Err(Malformed)` for a syntax error, a wrongly-typed value, or an
    /// unrecognised key. `path` is carried into the error for the message only.
    /// Test: `project_config_parses_worktree`, `project_config_rejects_unknown_key`,
    /// `project_config_rejects_wrong_type`.
    pub fn from_toml(raw: &str, path: &Path) -> Result<Self, ProjectConfigError> {
        toml::from_str(raw).map_err(|source| ProjectConfigError::Malformed {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Load `<project_dir>/.trusty-mpm.toml`, if it exists.
    ///
    /// Why: the fallible entry point, for callers that want to surface a bad
    /// config to a human (`tm doctor`, a future `tm config check`) rather than
    /// degrade past it.
    /// What: `Ok(None)` when the file is absent — the overwhelmingly common
    /// case, and not an error. `Ok(Some(cfg))` when it parses. `Err` when it
    /// exists but cannot be read or does not parse.
    ///
    /// The file is read from `project_dir` AS GIVEN, not from the harness root
    /// [`crate::core::harness_root::harness_root_for`] would resolve. That is
    /// deliberate: unlike the machine-local `.trusty-mpm/` state #4832 hoisted
    /// to the owning checkout, this file is TRACKED, so it is part of the
    /// branch's working set exactly like `Cargo.toml`. A worktree testing a
    /// branch that changes this file must see the changed file, or the PR that
    /// changes it could never be reviewed by running it.
    /// Test: `project_config_absent_is_none`, `project_config_reads_from_disk`.
    pub fn load(project_dir: &Path) -> Result<Option<Self>, ProjectConfigError> {
        let path = project_dir.join(PROJECT_CONFIG_FILE);
        match std::fs::read_to_string(&path) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(ProjectConfigError::Io { path, source }),
            Ok(raw) => Self::from_toml(&raw, &path).map(Some),
        }
    }
}

/// Load a project config for a runtime path that must not be blocked by a bad file.
///
/// Why: the spawn path cannot abort on a malformed config. This file is
/// COMMITTED, so one bad push would otherwise break every session for every
/// operator on the project at once — a far worse outcome than the setting not
/// applying. The error contract therefore matches the harness manifest's
/// (HR-2, [`crate::core::manifest::resolve`](crate::core::catchup::resolve)): a broken layer is skipped, never
/// fatal.
///
/// Skipped is not silent. The file's values are REJECTED WHOLESALE — nothing in
/// a file that failed `deny_unknown_fields` is trusted, because a typo means the
/// author's intent is unknown, not partially known — and the parse error is
/// logged at `error` level (not `warn`), naming the file and the offending key.
/// What: `Some(cfg)` when the file parses; `None` when it is absent (silent) or
/// unusable (logged).
/// Test: `load_or_report_returns_none_for_unknown_key`,
/// `load_or_report_returns_none_when_absent`.
pub fn load_or_report(project_dir: &Path) -> Option<ProjectLevelConfig> {
    match ProjectLevelConfig::load(project_dir) {
        Ok(cfg) => cfg,
        Err(err) => {
            tracing::error!(
                "project config ignored: {err}; \
                 no setting from this file is applied until it parses"
            );
            None
        }
    }
}

#[cfg(test)]
#[path = "project_config_tests.rs"]
mod tests;
