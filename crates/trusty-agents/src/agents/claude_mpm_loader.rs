//! Loader for claude-mpm format agents (.md with YAML frontmatter).
//!
//! Why: claude-mpm-style agents are authored as Markdown files with YAML
//! frontmatter and deployed under `~/.claude/agents/` (user-level) and
//! `.claude/agents/` (project-level). Supporting this format lets trusty-agents
//! reuse the growing claude-mpm agent ecosystem without forcing authors to
//! hand-maintain parallel TOML copies.
//! What: Scans both directories, parses each `.md` file's frontmatter + body,
//! and converts it into an `AgentConfig` that plugs into the existing engine
//! transparently. Project-level entries override user-level entries by name.
//! Test: See the `tests` module below — parsing, fallback defaults, frontmatter
//! absence, and conversion to `AgentConfig` are all covered.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use serde::Deserialize;

use crate::agents::{
    AgentConfig, AgentInfo, LlmParams, RunnerKind, SystemPrompt, ToolChoice, ToolsConfig,
};
use crate::llm::adapter::adapter_for_model;

/// Default model used when a claude-mpm agent file does not specify one.
const DEFAULT_MODEL: &str = "anthropic/claude-sonnet-4-6";

/// Parsed YAML frontmatter from a claude-mpm agent `.md` file.
///
/// Why: claude-mpm's agent schema is richer than our minimal needs. We
/// extract only the fields we actively consume so unknown keys don't fail
/// parsing (serde_yaml silently drops fields absent from the struct).
/// What: Holds the handful of keys we care about, all optional so partial
/// frontmatter loads instead of erroring.
/// Test: `test_parse_valid_agent`.
#[derive(Debug, Deserialize, Default)]
struct ClaudeMpmFrontmatter {
    name: Option<String>,
    description: Option<String>,
    /// The agent's declared domain in trusty-mpm's SOURCE assets and in
    /// hand-authored agent files (#4502). Normalized, never used verbatim —
    /// see [`super::claude_mpm_role::normalize_role`].
    role: Option<String>,
    /// The agent's declared domain in the DEPLOYED artifacts (#4502): this is
    /// the key trusty-mpm's composer actually emits into `.claude/agents/*.md`
    /// — it does not emit `role` — so this is the one that carries the domain
    /// in practice. Parsed since the loader was written; consumed as of #4502.
    agent_type: Option<String>,
    #[allow(dead_code)]
    version: Option<String>,
    #[serde(default)]
    skills: Vec<String>,
    #[allow(dead_code)]
    #[serde(rename = "initialPrompt")]
    initial_prompt: Option<String>,
    /// Optional model override (non-standard in claude-mpm but supported).
    model: Option<String>,
}

/// A loaded claude-mpm agent ready to be surfaced as `AgentConfig`.
///
/// Why: Keeping the intermediate representation separate from `AgentConfig`
/// lets us defer adapter construction and model resolution until the caller
/// actually wants to use the agent (cheap + cache-friendly).
/// What: Owns the fields needed to materialize an `AgentConfig`, plus the
/// source path for diagnostics.
/// Test: Covered indirectly by `test_to_agent_config_*`.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ClaudeMpmAgent {
    pub name: String,
    pub description: String,
    pub model: String,
    pub system_prompt: String,
    pub skills: Vec<String>,
    pub source_path: PathBuf,
    /// The NORMALIZED coarse role (#4502) — already translated by
    /// [`super::claude_mpm_role::normalize_role`] at parse time, so nothing
    /// downstream can accidentally read a raw frontmatter value. Fail-closed:
    /// [`super::claude_mpm_role::UNMAPPED_ROLE`] when the file declares no
    /// recognizable domain, which is byte-identical to the value this loader
    /// hardcoded before #4502.
    pub role: String,
}

impl ClaudeMpmAgent {
    /// Convert into an `AgentConfig` compatible with the existing engine.
    ///
    /// Why: The rest of trusty-agents works against `AgentConfig`; building one
    /// here means the runner, prompt-builder, and tool-dispatch paths don't
    /// need to know claude-mpm exists.
    /// What: Fills every required field with sensible defaults (temperature
    /// 0.3, 8k tokens, tool_choice Auto, Subprocess runner) and selects the
    /// provider adapter from the agent's resolved model string. `role` is the
    /// value [`super::claude_mpm_role::normalize_role`] already resolved at
    /// parse time (#4502) — it was a hardcoded `"agent"` before that.
    /// Test: `test_to_agent_config_name_preserved`,
    /// `test_to_agent_config_system_prompt_is_body`,
    /// `to_agent_config_carries_the_normalized_role`,
    /// `to_agent_config_role_fails_closed_for_an_unmappable_declaration`.
    pub fn to_agent_config(&self) -> AgentConfig {
        let adapter: Arc<dyn crate::llm::adapter::ModelAdapter> =
            Arc::from(adapter_for_model(&self.model));
        AgentConfig {
            agent: AgentInfo {
                name: self.name.clone(),
                // #4502: the normalized coarse role, resolved at parse time.
                // NEVER a verbatim frontmatter value — `role` selects the
                // tool-registry branch in `build_registry_for_agent` and is
                // checked against `ASSISTANT_ALLOWED_DELEGATE_ROLES` at every
                // delegation, so it must come from a reviewed table.
                role: self.role.clone(),
                model: self.model.clone(),
                description: self.description.clone(),
                persistent_session: false,
                runner: RunnerKind::Subprocess,
                capabilities: None,
                display_name: None,
                hidden: false,
                kind: "assistant".to_string(),
                prompt_label: None,
                extends: None,
                tier: None,
            },
            llm: LlmParams {
                temperature: 0.3,
                max_tokens: 8192,
                model_override: None,
                enable_prompt_caching: true,
                max_turns: 20,
                persona_max_turns: None,
                tool_choice: ToolChoice::Auto,
                use_finish_task: false,
                use_anthropic_direct: false,
                claude_allowed_tools: Vec::new(),
                aws_profile: None,
                aws_region: None,
                elevation_threshold: None,
                elevation_model: None,
                stop_sequences: Vec::new(),
                routing_model: None,
                thinking_enabled: None,
            },
            system_prompt: SystemPrompt {
                content: self.system_prompt.clone(),
                skills: if self.skills.is_empty() {
                    None
                } else {
                    Some(self.skills.clone())
                },
            },
            tools: ToolsConfig::default(),
            compress: crate::agents::AgentCompressConfig::default(),
            runner_config: crate::agents::RunnerConfig::default(),
            session: crate::agents::SessionCompressionConfig::default(),
            plugins: crate::agents::AgentPluginsConfig::default(),
            rbac: crate::agents::RbacConfig::default(),
            workstreams: crate::agents::WorkstreamContextConfig::default(),
            adapter,
            listeners: Vec::new(),
            // #3816: claude-mpm agents carry no `[[stores]]` table — an
            // unbound store is a valid state (see `AgentConfig::stores`).
            stores: crate::stores::StoresConfig::default(),
            skills: crate::agents::SkillsConfig::default(),
            // #3936: claude-mpm agents carry no `[permissions]` table.
            permissions: crate::agents::PermissionsConfig::default(),
            // #4026: no cross-product grants from a claude-mpm agent yet.
            subagents: crate::agents::SubagentsConfig::default(),
        }
    }
}

/// Parse a claude-mpm `.md` file into a `ClaudeMpmAgent`.
///
/// Why: Agent files without valid YAML frontmatter aren't claude-mpm agents
/// (they may be README.md files or unrelated notes in the same directory),
/// so returning `None` lets the scanner skip them without noise.
/// What: Detects a leading `---\n` fence, extracts the YAML block, parses it
/// with serde_yaml, then treats everything after the closing `---` fence as
/// the system prompt body. Falls back to the filename stem when `name` is
/// missing, and to `DEFAULT_MODEL` when `model` is absent.
/// Test: `test_parse_valid_agent`, `test_parse_no_frontmatter_returns_none`,
/// `test_default_model_applied`, `parse_normalizes_the_declared_domain`.
pub fn parse_agent_file(path: &Path, content: &str) -> Option<ClaudeMpmAgent> {
    let trimmed = content.trim_start_matches('\u{feff}'); // strip BOM if present
    let trimmed = trimmed.trim_start_matches(['\n', '\r']);

    // Must start with a frontmatter fence.
    let rest = trimmed.strip_prefix("---")?;
    let rest = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))?;

    // Find closing fence on its own line.
    let end_rel = rest.find("\n---")?;
    let fm_str = &rest[..end_rel];
    let after = &rest[end_rel + 4..]; // skip "\n---"
    let body = after
        .strip_prefix('\n')
        .or_else(|| after.strip_prefix("\r\n"))
        .unwrap_or(after);

    let fm: ClaudeMpmFrontmatter = serde_yaml::from_str(fm_str).ok()?;

    let name = fm.name.unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string()
    });
    let description = fm.description.unwrap_or_default();
    let model = fm.model.unwrap_or_else(|| DEFAULT_MODEL.to_string());

    // #4502: normalize the declared domain HERE, at the single parse point,
    // so no downstream consumer can reach a raw frontmatter value.
    let role = super::claude_mpm_role::normalize_role(fm.role.as_deref(), fm.agent_type.as_deref());

    Some(ClaudeMpmAgent {
        name,
        description,
        model,
        system_prompt: body.to_string(),
        skills: fm.skills,
        source_path: path.to_path_buf(),
        role,
    })
}

/// Discover claude-mpm agents from standard directories.
///
/// Why: A single entry point for callers (startup diagnostics, agent
/// fallback) means the priority rules live in one place.
/// What: Loads `~/.claude/agents/*.md` first (lower priority), then
/// `<project_dir>/.claude/agents/*.md` (higher priority — overrides by name).
/// Returns a HashMap keyed by agent name.
/// Test: Exercised by runtime discovery; unit tests cover the parsing
/// primitives used here.
pub async fn discover_agents(project_dir: &Path) -> Result<HashMap<String, ClaudeMpmAgent>> {
    let mut agents: HashMap<String, ClaudeMpmAgent> = HashMap::new();

    // User-level first (lower priority).
    let home = dirs::home_dir().unwrap_or_default();
    load_from_dir(&mut agents, &home.join(".claude").join("agents")).await;

    // Project-level second (higher priority — overrides user-level by name).
    load_from_dir(&mut agents, &project_dir.join(".claude").join("agents")).await;

    tracing::debug!(count = agents.len(), "discovered claude-mpm agents");
    Ok(agents)
}

/// Read every `.md` file in `dir` and insert parsed agents into `out`.
///
/// Why: Shared between user-level and project-level passes so override
/// semantics (later writes win) work identically for both.
/// What: Silently skips non-existent directories and unreadable files;
/// logs parse failures at debug. Later calls overwrite earlier entries.
/// Test: Indirect — exercised via `discover_agents` in integration usage.
async fn load_from_dir(out: &mut HashMap<String, ClaudeMpmAgent>, dir: &Path) {
    if !dir.exists() {
        return;
    }
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!(dir = %dir.display(), error = %e, "claude-mpm: read_dir failed");
            return;
        }
    };
    loop {
        let next = match entries.next_entry().await {
            Ok(Some(e)) => e,
            Ok(None) => break,
            Err(e) => {
                tracing::debug!(error = %e, "claude-mpm: dir iter error");
                break;
            }
        };
        let path = next.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "claude-mpm: read failed");
                continue;
            }
        };
        if let Some(agent) = parse_agent_file(&path, &content) {
            out.insert(agent.name.clone(), agent);
        } else {
            tracing::debug!(path = %path.display(), "claude-mpm: not a valid agent file (no frontmatter)");
        }
    }
}

/// Find a single claude-mpm agent by name.
///
/// Why: The TOML loader uses this as a fallback when no `<name>.toml` exists
/// in the configured config dir, giving users a low-friction way to drop a
/// claude-mpm agent into a project.
/// What: Runs full discovery and returns the matching entry, or `None`.
/// Test: Indirect — covered by integration flow.
#[allow(dead_code)]
pub async fn find_agent(name: &str, project_dir: &Path) -> Option<ClaudeMpmAgent> {
    let mut agents = discover_agents(project_dir).await.ok()?;
    agents.remove(name)
}

/// Synchronous single-agent lookup mirroring [`find_agent`] (#3055).
///
/// Why: the sync per-name loader (`AgentConfig::by_name_unresolved_src`) and the
/// `extends` ancestor lookup must reach the SAME claude-mpm agents the async
/// `by_name_async` loader can, or a child extending a claude-mpm-only base gets
/// an asymmetric `ExtendsNotFound` (code-critic drift finding, PR #3106). The
/// reads are a handful of small `.md` files, so doing them synchronously here
/// costs nothing meaningful next to the LLM dispatch that follows.
/// What: scans `~/.claude/agents` then `<cwd>/.claude/agents` (project overrides
/// user, matching [`discover_agents`]) with `std::fs`, returns the matching
/// agent already projected into an [`AgentConfig`], or `None`.
/// Test: exercised via the loader's `by_name` symmetry tests.
pub fn find_agent_sync(name: &str) -> Option<AgentConfig> {
    let project_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut agents: HashMap<String, ClaudeMpmAgent> = HashMap::new();
    let home = dirs::home_dir().unwrap_or_default();
    load_from_dir_sync(&mut agents, &home.join(".claude").join("agents"));
    load_from_dir_sync(&mut agents, &project_dir.join(".claude").join("agents"));
    let agent = agents.remove(name)?;
    tracing::info!(
        agent = %name,
        source = %agent.source_path.display(),
        "loaded claude-mpm agent (sync fallback)"
    );
    Some(agent.to_agent_config())
}

/// Synchronous counterpart to [`load_from_dir`] used by [`find_agent_sync`].
///
/// Why: keeps the sync fallback's discovery semantics identical to the async
/// path (same directories, same override order, same silent-skip tolerance).
/// What: reads every `.md` in `dir`, parses it, and inserts (later writes win).
/// Missing/unreadable dirs and files are skipped.
/// Test: exercised via `find_agent_sync`.
fn load_from_dir_sync(out: &mut HashMap<String, ClaudeMpmAgent>, dir: &Path) {
    if !dir.exists() {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!(dir = %dir.display(), error = %e, "claude-mpm: read_dir failed (sync)");
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "claude-mpm: read failed (sync)");
                continue;
            }
        };
        if let Some(agent) = parse_agent_file(&path, &content) {
            out.insert(agent.name.clone(), agent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const SAMPLE_AGENT_MD: &str = "---\nname: test-agent\ndescription: \"A test agent for unit testing\"\nagent_type: test\nversion: \"1.0.0\"\nskills:\n- some-skill\n---\n# Test Agent\n\nYou are a test agent. Do test things.\n\n## Rules\n- Always test\n";

    const AGENT_NO_FRONTMATTER: &str = "# Some Document\n\nThis has no frontmatter.\n";

    #[test]
    fn test_parse_valid_agent() {
        let agent = parse_agent_file(&PathBuf::from("test.md"), SAMPLE_AGENT_MD).unwrap();
        assert_eq!(agent.name, "test-agent");
        assert_eq!(agent.description, "A test agent for unit testing");
        assert_eq!(agent.skills, vec!["some-skill".to_string()]);
        assert!(agent.system_prompt.contains("You are a test agent"));
    }

    #[test]
    fn test_parse_no_frontmatter_returns_none() {
        let result = parse_agent_file(&PathBuf::from("test.md"), AGENT_NO_FRONTMATTER);
        assert!(result.is_none());
    }

    #[test]
    fn test_default_model_applied() {
        let agent = parse_agent_file(&PathBuf::from("test.md"), SAMPLE_AGENT_MD).unwrap();
        assert_eq!(agent.model, DEFAULT_MODEL);
    }

    #[test]
    fn test_to_agent_config_name_preserved() {
        let agent = parse_agent_file(&PathBuf::from("test.md"), SAMPLE_AGENT_MD).unwrap();
        let config = agent.to_agent_config();
        assert_eq!(config.agent.name, "test-agent");
    }

    #[test]
    fn test_to_agent_config_system_prompt_is_body() {
        let agent = parse_agent_file(&PathBuf::from("test.md"), SAMPLE_AGENT_MD).unwrap();
        let config = agent.to_agent_config();
        assert!(
            config
                .system_prompt
                .content
                .contains("You are a test agent")
        );
        // Frontmatter must not leak into system prompt body.
        assert!(!config.system_prompt.content.contains("agent_type:"));
    }

    /// A DEPLOYED artifact, shaped exactly as trusty-mpm's composer emits one:
    /// `agent_type` carries the domain and there is no `role` key at all.
    /// This is the frontmatter that actually reaches this loader in
    /// production, so it is the one the normalization must handle.
    const DEPLOYED_ENGINEER_MD: &str = "---\nname: rust-engineer\ndescription: \"Rust specialist\"\nagent_type: engineer\nversion: \"1.2.2\"\n---\n# Rust Engineer\n\nYou are a Rust engineer.\n";

    /// A real trusty-mpm specialist with NO counterpart in the coarse
    /// vocabulary. It must stay on the sentinel — ineligible by design.
    const DEPLOYED_SECURITY_MD: &str = "---\nname: security\ndescription: \"Security specialist\"\nagent_type: security\n---\n# Security\n\nYou audit things.\n";

    #[test]
    fn parse_normalizes_the_declared_domain() {
        let agent = parse_agent_file(&PathBuf::from("rust-engineer.md"), DEPLOYED_ENGINEER_MD)
            .expect("parses");
        assert_eq!(
            agent.role, "engineer",
            "a deployed artifact declares its domain via agent_type, not role"
        );
        // The pre-existing fixture declares `agent_type: test`, which is not a
        // domain — it must land on the fail-closed sentinel, i.e. exactly the
        // value this loader hardcoded before #4502.
        let unmapped =
            parse_agent_file(&PathBuf::from("test.md"), SAMPLE_AGENT_MD).expect("parses");
        assert_eq!(
            unmapped.role,
            crate::agents::claude_mpm_role::UNMAPPED_ROLE,
            "an unrecognized agent_type must not become an eligible role"
        );
    }

    #[test]
    fn to_agent_config_carries_the_normalized_role() {
        let agent = parse_agent_file(&PathBuf::from("rust-engineer.md"), DEPLOYED_ENGINEER_MD)
            .expect("parses");
        assert_eq!(agent.to_agent_config().agent.role, "engineer");
    }

    /// The fail-closed property, asserted end-to-end through the real parse +
    /// projection path rather than only against the pure mapping function: a
    /// declared domain the table does not admit must reach `AgentInfo.role` as
    /// the sentinel, and the sentinel must not be role-eligible.
    #[test]
    fn to_agent_config_role_fails_closed_for_an_unmappable_declaration() {
        let agent =
            parse_agent_file(&PathBuf::from("security.md"), DEPLOYED_SECURITY_MD).expect("parses");
        let role = agent.to_agent_config().agent.role;
        assert_eq!(role, crate::agents::claude_mpm_role::UNMAPPED_ROLE);
        assert!(
            !crate::runtime::tool_registry::ASSISTANT_ALLOWED_DELEGATE_ROLES
                .contains(&role.as_str()),
            "an unmappable claude-mpm agent must stay role-ineligible, exactly \
             as it was before #4502"
        );
    }
}
