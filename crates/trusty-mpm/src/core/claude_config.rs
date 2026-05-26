//! Claude Code configuration model (pure, no I/O).
//!
//! Why: trusty-mpm inspects a project's Claude Code setup — user-level and
//! project-level `settings.json`, hooks, permissions, agent directories — to
//! recommend improvements. The *path resolution* and the recommendation /
//! config data model are pure logic and belong in `core`; the daemon's
//! `claude_config` module does the actual file reads and edits.
//! What: [`ClaudeConfigPaths`] (every expected config path for a project),
//! [`ClaudeConfigReader::paths_for_project`] (resolves them), [`ClaudeConfig`]
//! (the merged, analyzed config), [`ConfigRecommendation`] and [`Severity`].
//! Test: `cargo test -p trusty-mpm-core claude_config` covers path resolution
//! and the recommendation JSON round-trip.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Every Claude Code configuration path for one project.
///
/// Why: the analyzer reads four `settings.json` files and two agent
/// directories; bundling their paths keeps the reader and the recommendation
/// applier from re-deriving joins.
/// What: user- and project-level settings (regular + `.local`) plus the two
/// `agents/` directories.
/// Test: `paths_for_project_resolves_all`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeConfigPaths {
    /// `~/.claude/settings.json`.
    pub user_settings: PathBuf,
    /// `~/.claude/settings.local.json`.
    pub user_local_settings: PathBuf,
    /// `<project>/.claude/settings.json`.
    pub project_settings: PathBuf,
    /// `<project>/.claude/settings.local.json`.
    pub project_local_settings: PathBuf,
    /// `~/.claude/agents/`.
    pub user_agents_dir: PathBuf,
    /// `<project>/.claude/agents/`.
    pub project_agents_dir: PathBuf,
}

/// Pure resolver for Claude Code configuration paths.
///
/// Why: path layout is a fixed convention; isolating it as a unit type keeps
/// it testable without touching the filesystem.
/// What: [`paths_for_project`](ClaudeConfigReader::paths_for_project) builds a
/// [`ClaudeConfigPaths`] from a project directory and the user's home.
/// Test: `paths_for_project_resolves_all`.
pub struct ClaudeConfigReader;

impl ClaudeConfigReader {
    /// Resolve every Claude Code config path for `project`.
    ///
    /// Why: the daemon's analyzer needs the full path set; computing it from
    /// the project directory and `dirs::home_dir()` in one place keeps the
    /// convention consistent.
    /// What: builds `<project>/.claude/...` and `<home>/.claude/...` paths.
    /// When the home directory cannot be determined the user paths fall back to
    /// `.claude/...` (relative), which simply never exist — degrading safely.
    /// Test: `paths_for_project_resolves_all`.
    pub fn paths_for_project(project: &Path) -> ClaudeConfigPaths {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let user_claude = home.join(".claude");
        let project_claude = project.join(".claude");
        ClaudeConfigPaths {
            user_settings: user_claude.join("settings.json"),
            user_local_settings: user_claude.join("settings.local.json"),
            project_settings: project_claude.join("settings.json"),
            project_local_settings: project_claude.join("settings.local.json"),
            user_agents_dir: user_claude.join("agents"),
            project_agents_dir: project_claude.join("agents"),
        }
    }
}

/// The merged, analyzed view of a project's Claude Code configuration.
///
/// Why: recommendations are computed from a few high-level facts (are hooks
/// configured? is the allow list broad? are agents deployed?); flattening the
/// raw JSON into these booleans keeps the analyzer simple and testable.
/// What: whether any hooks are configured, whether the permission allow list
/// contains a wildcard, the number of allow-list entries, whether any agents
/// are deployed, and whether `OPENROUTER_API_KEY` appears in the env block.
/// Test: `claude_config_json_roundtrip`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeConfig {
    /// True when the merged config defines at least one hook.
    pub has_hooks: bool,
    /// True when `permissions.allow` contains a `*` wildcard entry.
    pub allow_list_has_wildcard: bool,
    /// Number of entries in the merged `permissions.allow` list.
    pub allow_list_entries: usize,
    /// True when at least one agent file was found in either agents dir.
    pub has_agents: bool,
    /// True when `OPENROUTER_API_KEY` appears in the config's `env` block.
    pub has_openrouter_key: bool,
}

/// How serious a [`ConfigRecommendation`] is.
///
/// Why: the dashboard sorts and colours recommendations by severity.
/// What: `Info` (nice-to-have), `Warning` (should fix), `Critical` (security).
/// Test: `severity_json_roundtrip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Informational — a non-urgent improvement.
    Info,
    /// Warning — should be addressed.
    Warning,
    /// Critical — a security concern.
    Critical,
}

/// One actionable recommendation for a project's Claude Code config.
///
/// Why: the analyzer produces a list of these; the dashboard renders them and
/// `POST /claude-config/apply` acts on one by `id`.
/// What: a stable `id`, a [`Severity`], a human title and description, and
/// whether the daemon can apply it without further input.
/// Test: `recommendation_json_roundtrip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigRecommendation {
    /// Stable identifier, e.g. `add-trusty-hooks`.
    pub id: String,
    /// How serious the recommendation is.
    pub severity: Severity,
    /// One-line summary.
    pub title: String,
    /// Longer explanation of the issue and the fix.
    pub description: String,
    /// True when the daemon can apply the fix without operator input.
    pub auto_applicable: bool,
}

/// A named checkpoint of all Claude Code config files at a moment in time.
///
/// Why: every mutating operation (`apply`, `deploy`) must be reversible; a
/// checkpoint captures the full pre-change config so the operator can undo a
/// change with a single restore call.
/// What: a stable `id`, an RFC3339 `created_at`, the `project` root, an optional
/// human `label`, and a map of relative-path → file content for every config
/// file that existed (absent files are simply not present in the map).
/// Test: `config_checkpoint_json_roundtrip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigCheckpoint {
    /// Stable identifier, e.g. `checkpoint-20260517-153000-a1b2`.
    pub id: String,
    /// RFC3339 timestamp the checkpoint was created at.
    pub created_at: String,
    /// Project root at snapshot time.
    pub project: PathBuf,
    /// Optional human label, e.g. `before-apply-add-trusty-hooks`.
    pub label: Option<String>,
    /// Relative path → file content. An absent file is not present in the map.
    pub files: HashMap<String, String>,
}

/// Where checkpoints are stored for a project.
///
/// Why: checkpoints live in a fixed, project-local location so listing and
/// restoring need no extra configuration; isolating the path layout as a unit
/// type keeps it testable without filesystem access.
/// What: [`dir`](CheckpointPaths::dir) yields `<project>/.trusty-mpm/checkpoints`
/// and [`for_id`](CheckpointPaths::for_id) the JSON file for one checkpoint id.
/// Test: `checkpoint_paths_resolve`.
pub struct CheckpointPaths;

impl CheckpointPaths {
    /// The directory holding every checkpoint JSON file for `project`.
    ///
    /// Why: the checkpointer creates, lists, and deletes files here.
    /// What: returns `<project>/.trusty-mpm/checkpoints`.
    /// Test: `checkpoint_paths_resolve`.
    pub fn dir(project: &Path) -> PathBuf {
        project.join(".trusty-mpm").join("checkpoints")
    }

    /// The JSON file path for one checkpoint `id` under `project`.
    ///
    /// Why: each checkpoint is a single self-contained JSON document.
    /// What: returns `<project>/.trusty-mpm/checkpoints/<id>.json`.
    /// Test: `checkpoint_paths_resolve`.
    pub fn for_id(project: &Path, id: &str) -> PathBuf {
        Self::dir(project).join(format!("{id}.json"))
    }
}

/// Which Claude Code settings scope a [`DeploymentProfile`] writes to.
///
/// Why: a profile may belong at the user level (every project), the project
/// level (this repo only), or both; the deployer needs to know which files to
/// touch.
/// What: `User`, `Project`, or `Both`.
/// Test: `deploy_target_json_roundtrip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeployTarget {
    /// Write to `~/.claude/settings.json`.
    User,
    /// Write to `<project>/.claude/settings.json`.
    Project,
    /// Write to both user- and project-level settings.
    Both,
}

/// The hook commands a [`DeploymentProfile`] installs.
///
/// Why: oversight profiles wire Claude Code hook events to commands (typically a
/// `curl` POST to the daemon); grouping them keeps the profile model flat.
/// What: lists of command strings for `PreToolUse`, `PostToolUse`, and `Stop`.
/// Test: `hook_config_json_roundtrip`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookConfig {
    /// Commands run on the `PreToolUse` hook event.
    pub pre_tool_use: Vec<String>,
    /// Commands run on the `PostToolUse` hook event.
    pub post_tool_use: Vec<String>,
    /// Commands run on the `Stop` hook event.
    pub stop: Vec<String>,
}

/// The permission allow/deny lists a [`DeploymentProfile`] installs.
///
/// Why: profiles set the tools Claude Code may (or must not) use; a flat pair of
/// lists mirrors the Claude Code `permissions` block.
/// What: an `allow` list and a `deny` list of tool matcher strings.
/// Test: `permission_config_json_roundtrip`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionConfig {
    /// Tool matchers Claude Code is permitted to use.
    pub allow: Vec<String>,
    /// Tool matchers Claude Code must not use.
    pub deny: Vec<String>,
}

/// A named, reusable set of Claude Code configuration values.
///
/// Why: operators want one-click configuration presets (full oversight, a
/// read-only review mode, a clean slate) rather than hand-editing JSON; a
/// profile bundles hooks, permissions, and env vars under a name.
/// What: a `name`, a `description`, a [`DeployTarget`], optional [`HookConfig`]
/// and [`PermissionConfig`], and a map of environment variables.
/// Test: `deployment_profile_json_roundtrip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentProfile {
    /// Profile name, e.g. `trusty-mpm-oversight`.
    pub name: String,
    /// Human-readable description of what the profile does.
    pub description: String,
    /// Which settings scope the profile writes to.
    pub target: DeployTarget,
    /// Hook commands to install, or `None` to leave hooks untouched.
    pub hooks: Option<HookConfig>,
    /// Permission lists to install, or `None` to leave permissions untouched.
    pub permissions: Option<PermissionConfig>,
    /// Environment variables to merge into the `env` block.
    pub env_vars: HashMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_for_project_resolves_all() {
        let paths = ClaudeConfigReader::paths_for_project(Path::new("/work/demo"));
        assert!(paths.project_settings.ends_with(".claude/settings.json"));
        assert!(
            paths
                .project_local_settings
                .ends_with(".claude/settings.local.json")
        );
        assert!(paths.project_agents_dir.ends_with(".claude/agents"));
        assert!(
            paths.project_settings.starts_with("/work/demo"),
            "project paths must be under the project dir"
        );
        // User paths are under `.claude` regardless of where home resolves.
        assert!(paths.user_settings.ends_with(".claude/settings.json"));
    }

    #[test]
    fn claude_config_json_roundtrip() {
        let cfg = ClaudeConfig {
            has_hooks: true,
            allow_list_has_wildcard: false,
            allow_list_entries: 3,
            has_agents: true,
            has_openrouter_key: false,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: ClaudeConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn severity_json_roundtrip() {
        for sev in [Severity::Info, Severity::Warning, Severity::Critical] {
            let json = serde_json::to_string(&sev).unwrap();
            let back: Severity = serde_json::from_str(&json).unwrap();
            assert_eq!(back, sev);
        }
        // Wire form is lowercase.
        assert_eq!(
            serde_json::to_string(&Severity::Critical).unwrap(),
            "\"critical\""
        );
    }

    #[test]
    fn recommendation_json_roundtrip() {
        let rec = ConfigRecommendation {
            id: "add-trusty-hooks".into(),
            severity: Severity::Warning,
            title: "No hooks configured".into(),
            description: "Add pre/post tool-use hooks for oversight.".into(),
            auto_applicable: false,
        };
        let json = serde_json::to_string(&rec).unwrap();
        let back: ConfigRecommendation = serde_json::from_str(&json).unwrap();
        assert_eq!(back, rec);
    }

    #[test]
    fn checkpoint_paths_resolve() {
        let project = Path::new("/work/demo");
        assert!(
            CheckpointPaths::dir(project).ends_with(".trusty-mpm/checkpoints"),
            "checkpoint dir is project-local"
        );
        assert!(
            CheckpointPaths::for_id(project, "checkpoint-x")
                .ends_with(".trusty-mpm/checkpoints/checkpoint-x.json"),
            "checkpoint file is <id>.json"
        );
    }

    #[test]
    fn config_checkpoint_json_roundtrip() {
        let mut files = HashMap::new();
        files.insert(".claude/settings.json".to_string(), "{}".to_string());
        let cp = ConfigCheckpoint {
            id: "checkpoint-20260517-153000-a1b2".into(),
            created_at: "2026-05-17T15:30:00+00:00".into(),
            project: PathBuf::from("/work/demo"),
            label: Some("before-apply".into()),
            files,
        };
        let json = serde_json::to_string(&cp).unwrap();
        let back: ConfigCheckpoint = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cp);
    }

    #[test]
    fn deploy_target_json_roundtrip() {
        for target in [
            DeployTarget::User,
            DeployTarget::Project,
            DeployTarget::Both,
        ] {
            let json = serde_json::to_string(&target).unwrap();
            let back: DeployTarget = serde_json::from_str(&json).unwrap();
            assert_eq!(back, target);
        }
        // Wire form is lowercase.
        assert_eq!(
            serde_json::to_string(&DeployTarget::Project).unwrap(),
            "\"project\""
        );
    }

    #[test]
    fn hook_config_json_roundtrip() {
        let hooks = HookConfig {
            pre_tool_use: vec!["curl pre".into()],
            post_tool_use: vec!["curl post".into()],
            stop: vec![],
        };
        let json = serde_json::to_string(&hooks).unwrap();
        let back: HookConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, hooks);
    }

    #[test]
    fn permission_config_json_roundtrip() {
        let perms = PermissionConfig {
            allow: vec!["Read".into(), "Glob".into()],
            deny: vec!["Bash".into()],
        };
        let json = serde_json::to_string(&perms).unwrap();
        let back: PermissionConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, perms);
    }

    #[test]
    fn deployment_profile_json_roundtrip() {
        let mut env_vars = HashMap::new();
        env_vars.insert("OPENROUTER_API_KEY".to_string(), "sk-x".to_string());
        let profile = DeploymentProfile {
            name: "trusty-mpm-oversight".into(),
            description: "Full oversight".into(),
            target: DeployTarget::Both,
            hooks: Some(HookConfig::default()),
            permissions: Some(PermissionConfig::default()),
            env_vars,
        };
        let json = serde_json::to_string(&profile).unwrap();
        let back: DeploymentProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(back, profile);
    }
}
