//! Workflow JSON config parsing.
//!
//! Why: Workflows are declarative so they can be added without recompiling
//! the engine. JSON chosen for easy hand-authoring and tooling. The module is
//! split into focused files (#359): the top-level workflow/phase shapes and the
//! `safe_join` path guard live here, while the wave-decomposition shapes
//! (`Assignments` / `WaveDef` / `FileAssignment`) and their validation/repair
//! logic live in `assignments`.
//! What: `WorkflowDef` / `PhaseDef` structs with `serde(Deserialize)`, plus a
//! re-export of the assignment types so existing `workflow::config::Assignments`
//! paths keep resolving unchanged.
//! Test: Round-trip a minimal JSON doc through `serde_json::from_str` — see the
//! `tests` submodule.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

mod assignments;
#[cfg(test)]
mod tests;
mod wave_repair;

pub use assignments::{Assignments, FileAssignment, WaveDef};

/// Defense-in-depth path join that refuses to escape `base` (#114).
///
/// Why: `validate_file_path` is a lexical check on assignments.json input, but
/// any future caller constructing a path from external data (or a malicious
/// agent that bypasses validation) could still write outside `out_dir`. This
/// helper resolves the candidate path lexically (without touching the
/// filesystem for the relative part), then asserts the result is contained
/// within the canonicalized `base`. Returns `None` for any escape attempt
/// (parent-traversal beyond root, absolute paths, paths that resolve outside
/// base) so callers can log+skip rather than crash.
/// What: Strips a leading `/`, walks the candidate components manually
/// resolving `..` segments against an in-memory stack, then joins onto the
/// canonicalized base and verifies the final path starts with the base.
/// Filesystem canonicalization runs only against `base` (which must exist);
/// the candidate path itself may not yet exist on disk.
/// Test: `safe_join_*` unit tests in this module.
pub fn safe_join(base: &Path, rel: &str) -> Option<PathBuf> {
    // Strip any leading slash so "/etc/passwd" and "etc/passwd" both produce
    // a candidate rooted at `base` rather than escaping to the filesystem
    // root before we get a chance to inspect it.
    let rel = rel.trim_start_matches('/');

    // Canonicalize the base — this requires `base` to exist. Without it we
    // cannot safely compare prefixes against symlink-rewritten ancestors.
    let canonical_base = base.canonicalize().ok()?;

    // Seed the path stack with the canonical base components, then apply
    // each `rel` component lexically. `..` pops the stack but is FORBIDDEN
    // from popping below the base depth — that would escape `base`. We do
    // NOT canonicalize the rel portion (the file may not yet exist on disk).
    let mut parts: Vec<std::ffi::OsString> = canonical_base
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_os_string()),
            _ => None,
        })
        .collect();
    let base_depth = parts.len();
    for c in Path::new(rel).components() {
        match c {
            Component::ParentDir => {
                // Refuse to pop above the base prefix — any `..` that would
                // shrink `parts` below `base_depth` escapes `base`.
                if parts.len() <= base_depth {
                    return None;
                }
                parts.pop();
            }
            Component::Normal(s) => parts.push(s.to_os_string()),
            // Treat any RootDir embedded in `rel` (post-strip) as a request
            // to reset to root — that's an explicit escape attempt.
            Component::RootDir => return None,
            // Prefix (Windows drive letters) and CurDir are no-ops here.
            _ => {}
        }
    }

    // Reassemble as an absolute path. On Unix we need a leading `/`; on
    // Windows the prefix component would normally provide the drive, but
    // trusty-agents targets Unix so we use Path::new("/") as the root anchor.
    let mut final_path = PathBuf::from("/");
    for p in &parts {
        final_path.push(p);
    }

    if final_path.starts_with(&canonical_base) {
        Some(final_path)
    } else {
        None
    }
}

/// Top-level workflow definition loaded from `config/workflows/<name>.json`.
///
/// # Intent
/// Workflows are declarative so the engine can run new pipelines without code
/// changes. The JSON shape is versioned implicitly by the set of optional
/// fields — older configs keep working as new fields are added with
/// `#[serde(default)]`.
///
/// Test: `config_from_json_parses_minimal` round-trips a minimal config.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct WorkflowDef {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub phases: Vec<PhaseDef>,
    /// #76: Optional auto-push config. When present AND `enabled=true`, the
    /// engine stages `out_dir`, bumps Cargo.toml version, commits, and pushes
    /// after a successful workflow run.
    #[serde(default)]
    pub auto_push: Option<AutoPushConfig>,
    /// #84: Optional automatic GitHub issue lifecycle management. When present
    /// AND `enabled=true`, the engine creates a tracking issue at workflow
    /// start, comments after each phase, and closes the issue on success.
    #[serde(default)]
    pub ticket_management: Option<TicketManagementConfig>,
}

/// One phase in a workflow — a single agent invocation with its context
/// template and optional parallel/produces-files modifiers.
///
/// # Intent
/// Each phase is addressable by `name` so later phases can interpolate prior
/// outputs (`{{<phase_name>}}`). Keeping phase state inside the JSON (rather
/// than code) lets workflows be shared and reproduced verbatim.
///
/// Test: `config_from_json_parses_minimal`, `parallel_subtasks_parse_from_json`.
#[derive(Debug, Clone, Deserialize)]
pub struct PhaseDef {
    /// Phase id, e.g. `"research"`. Used as the key for context outputs.
    pub name: String,
    /// Agent name used for this phase (must resolve to a TOML config file).
    pub agent: String,
    /// Optional model override for this phase (takes precedence over the
    /// agent's own `agent.model`).
    #[serde(default, alias = "model_override")]
    pub model: Option<String>,
    /// Template string expanded against `WorkflowContext` before being sent
    /// to the agent. Supports `{{task}}`, `{{out_dir}}`, and
    /// `{{<phase_name>}}` substitutions.
    pub context_template: String,
    /// #64: When true, the engine scans this phase's output for
    /// `## File: <path>` sections and writes them under `out_dir` BEFORE
    /// advancing to the next phase. This is what lets the QA phase run
    /// pytest against the code the previous phase just produced.
    /// Defaults to `None` (treated as false) to preserve existing workflows.
    #[serde(default)]
    pub produces_files: Option<bool>,
    /// #73: Optional list of parallel subtasks. When present, one agent per
    /// subtask is spawned concurrently. When absent, single-agent behavior
    /// is unchanged.
    #[serde(default)]
    pub parallel_subtasks: Option<Vec<ParallelSubtask>>,
    /// #74: When true and parallel_subtasks is present, each sub-agent runs
    /// in a dedicated git worktree. Default false.
    #[serde(default)]
    pub worktree_protection: Option<bool>,
    /// #82: When true, the engine skips this phase entirely (no agent run,
    /// no output recorded, no file extraction). Lets workflows declare
    /// optional phases (e.g. `docs`) that are off by default and can be
    /// enabled by flipping `skip` to `false` without editing the phase list.
    /// Defaults to `None` (treated as false) for backwards compatibility.
    #[serde(default)]
    pub skip: Option<bool>,
    /// Skills-first: explicit skill names (or `["auto"]`) to inject for this
    /// phase via `SkillsLoader`. When absent, falls back to the agent TOML's
    /// `system_prompt.skills` field. `["auto"]` triggers language/framework
    /// auto-detection from the project directory and task text.
    /// Defaults to `None` so existing workflow JSON files are unaffected.
    #[serde(default)]
    pub skills: Option<Vec<String>>,
    /// Override AST-native tool surface for this phase.
    ///
    /// Why: Bake-off data shows AST-native is materially cheaper for research
    /// and plan phases (-55-60%) but slightly more expensive for code/qa
    /// phases (+20%) with fewer tests generated. Per-phase override lets
    /// workflows opt research+plan into AST-native while keeping code+qa on
    /// the traditional toolchain — a hybrid that yields ~14% total cost
    /// reduction with no quality loss.
    /// What: `None` = inherit from the global `--ast-native` flag.
    /// `Some(true)` = force-enable for this phase. `Some(false)` =
    /// force-disable for this phase.
    /// Test: `phase_ast_native_parses_from_json`,
    /// `phase_ast_native_defaults_to_none` in this module.
    #[serde(default)]
    pub ast_native: Option<bool>,
}

/// #73: A single parallel subtask description for a phase.
///
/// Why: Lets a phase dispatch multiple specialized sub-agent runs concurrently
/// (e.g. "backend" + "frontend" + "tests") so independent workstreams overlap.
/// What: `label` identifies the subtask (used for worktree dir naming and
/// merge reports); `task_suffix` is appended to the rendered phase template.
/// Test: `parallel_subtasks_parse_from_json` in workflow config tests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParallelSubtask {
    /// Short identifier used for worktree naming and merge reports.
    pub label: String,
    /// Appended to the phase's rendered task text for this sub-agent.
    pub task_suffix: String,
}

/// #76: Auto-push config — commits and pushes workflow output on success.
///
/// Why: For workflows that produce release-worthy artifacts, pushing to a
/// shared remote is the final step. Centralizing version bump + commit + push
/// keeps it opt-in and scriptable.
/// What: `enabled=false` by default (must be explicitly turned on).
/// `version_bump` is "patch" | "minor" | "none". `commit_message_template`
/// accepts `{{workflow}}`, `{{build}}`, `{{task_preview}}`, `{{version}}`
/// placeholders.
/// Test: `auto_push_config_defaults` round-trips a minimal JSON.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AutoPushConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_version_bump")]
    pub version_bump: String,
    #[serde(default = "default_commit_template")]
    pub commit_message_template: String,
    #[serde(default = "default_remote")]
    pub push_remote: String,
    #[serde(default = "default_branch")]
    pub push_branch: String,
}

fn default_version_bump() -> String {
    "patch".to_string()
}

fn default_commit_template() -> String {
    "feat(workflow): {{workflow}} build {{build}} — {{task_preview}}".to_string()
}

fn default_remote() -> String {
    "origin".to_string()
}

fn default_branch() -> String {
    "main".to_string()
}

fn default_true() -> bool {
    true
}

/// #84: Configuration for automatic GitHub issue lifecycle management.
///
/// Why: Every workflow run creates and manages a GitHub issue automatically,
/// providing full traceability without manual ticket creation. `enabled=false`
/// by default so the feature is strictly opt-in and existing workflows that
/// omit the block keep running unchanged.
/// What: Repo/assignee/milestone/labels drive issue creation; `auto_relate`,
/// `phase_comments`, and `close_on_success` control lifecycle hooks. Every
/// lifecycle call is a no-op when `enabled=false`, so shipping the struct as
/// `Default` is safe.
/// Test: `ticket_management_config_defaults` in this module;
/// `ticket_management_config_deserializes` in `tickets.rs`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TicketManagementConfig {
    /// Enable/disable ticket management. Default: false (opt-in).
    #[serde(default)]
    pub enabled: bool,

    /// GitHub repo in owner/name format (e.g. "bobmatnyc/trusty-agents").
    #[serde(default)]
    pub repo: String,

    /// GitHub username to assign issues to.
    #[serde(default)]
    pub assignee: String,

    /// Milestone title to assign issues to.
    #[serde(default)]
    pub milestone: String,

    /// Labels to apply to each run issue.
    #[serde(default)]
    pub labels: Vec<String>,

    /// Whether to search for related issues and cross-link them.
    #[serde(default = "default_true")]
    pub auto_relate: bool,

    /// Whether to add a comment after each phase completes.
    #[serde(default = "default_true")]
    pub phase_comments: bool,

    /// Whether to close the issue on successful workflow completion.
    #[serde(default = "default_true")]
    pub close_on_success: bool,
}

impl WorkflowDef {
    /// Load a workflow from a JSON file path.
    ///
    /// Why: Centralizes read + parse error messages for the CLI caller.
    /// What: Reads the file, deserializes via serde_json, and validates the
    /// resulting config so semantic errors (e.g. ticket_management enabled
    /// without a repo) fail loudly at load time instead of silently at
    /// runtime (#102).
    /// Test: See `config_from_json_parses_minimal` and
    /// `validate_rejects_enabled_ticket_management_without_repo` below.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read workflow config {}", path.display()))?;
        let def: WorkflowDef = serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse workflow JSON {}", path.display()))?;
        def.validate()
            .with_context(|| format!("invalid workflow config {}", path.display()))?;
        Ok(def)
    }

    /// Validate semantic invariants of a loaded workflow config (#102).
    ///
    /// Why: Some fields only matter when another is enabled; catching the
    /// mismatch here converts a confusing runtime 404 (e.g. empty repo
    /// yielding a `POST /repos//issues` request) into a clear load-time
    /// failure that names the offending field.
    /// What: Returns `Err` when `ticket_management.enabled` is true but
    /// `repo` is empty. Additional validations can be appended here.
    /// Test: `validate_rejects_enabled_ticket_management_without_repo`,
    /// `validate_accepts_disabled_ticket_management_without_repo`.
    pub fn validate(&self) -> Result<()> {
        if let Some(tm) = self.ticket_management.as_ref()
            && tm.enabled
            && tm.repo.trim().is_empty()
        {
            anyhow::bail!(
                "ticket_management.enabled = true but ticket_management.repo is empty; \
                 set repo to \"owner/name\" or set enabled = false"
            );
        }
        Ok(())
    }
}
