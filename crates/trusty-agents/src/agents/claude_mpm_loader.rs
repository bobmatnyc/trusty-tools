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
/// parsing (serde_yml silently drops fields absent from the struct).
/// What: Holds the handful of keys we care about, all optional so partial
/// frontmatter loads instead of erroring.
/// Test: `test_parse_valid_agent`.
#[derive(Debug, Deserialize, Default)]
struct ClaudeMpmFrontmatter {
    name: Option<String>,
    description: Option<String>,
    #[allow(dead_code)]
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
}

impl ClaudeMpmAgent {
    /// Convert into an `AgentConfig` compatible with the existing engine.
    ///
    /// Why: The rest of trusty-agents works against `AgentConfig`; building one
    /// here means the runner, prompt-builder, and tool-dispatch paths don't
    /// need to know claude-mpm exists.
    /// What: Fills every required field with sensible defaults (temperature
    /// 0.3, 8k tokens, tool_choice Auto, Subprocess runner) and selects the
    /// provider adapter from the agent's resolved model string.
    /// Test: `test_to_agent_config_name_preserved`,
    /// `test_to_agent_config_system_prompt_is_body`.
    pub fn to_agent_config(&self) -> AgentConfig {
        let adapter: Arc<dyn crate::llm::adapter::ModelAdapter> =
            Arc::from(adapter_for_model(&self.model));
        AgentConfig {
            agent: AgentInfo {
                name: self.name.clone(),
                role: "agent".to_string(),
                model: self.model.clone(),
                description: self.description.clone(),
                persistent_session: false,
                runner: RunnerKind::Subprocess,
                capabilities: None,
                display_name: None,
                prompt_label: None,
            },
            llm: LlmParams {
                temperature: 0.3,
                max_tokens: 8192,
                model_override: None,
                enable_prompt_caching: true,
                max_turns: 20,
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
            adapter,
        }
    }
}

/// Parse a claude-mpm `.md` file into a `ClaudeMpmAgent`.
///
/// Why: Agent files without valid YAML frontmatter aren't claude-mpm agents
/// (they may be README.md files or unrelated notes in the same directory),
/// so returning `None` lets the scanner skip them without noise.
/// What: Detects a leading `---\n` fence, extracts the YAML block, parses it
/// with serde_yml, then treats everything after the closing `---` fence as
/// the system prompt body. Falls back to the filename stem when `name` is
/// missing, and to `DEFAULT_MODEL` when `model` is absent.
/// Test: `test_parse_valid_agent`, `test_parse_no_frontmatter_returns_none`,
/// `test_default_model_applied`.
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

    let fm: ClaudeMpmFrontmatter = serde_yml::from_str(fm_str).ok()?;

    let name = fm.name.unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string()
    });
    let description = fm.description.unwrap_or_default();
    let model = fm.model.unwrap_or_else(|| DEFAULT_MODEL.to_string());

    Some(ClaudeMpmAgent {
        name,
        description,
        model,
        system_prompt: body.to_string(),
        skills: fm.skills,
        source_path: path.to_path_buf(),
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
}
