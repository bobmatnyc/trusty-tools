//! Dynamic agent discovery with hierarchical search paths and capability
//! matching (#167).
//!
//! Why: Hard-coding agent definitions in `config/agents/` makes the harness
//! brittle for operators who want per-project or per-user agent overrides.
//! This registry scans a priority-ordered list of directories on startup,
//! keeping the highest-priority copy of any agent with the same name, and
//! exposes a capability-scored `best_match` lookup so PM delegation can pick
//! the right agent from task signals without hard-coded routing tables.
//! What: `AgentRegistry` owns an ordered (name → (config, source)) map and
//! offers `get`, `best_match`, and `list` methods. Discovery is
//! failure-tolerant: missing directories and malformed TOML files log at
//! `warn` level and are skipped so one bad file never breaks startup. The
//! `.md`-agent parser lives in `md_agent`, and the PM roster renderers live
//! in `roster`.
//! Test: See the unit tests in `tests`.

use std::path::{Path, PathBuf};

use indexmap::IndexMap;

use super::AgentConfig;

mod md_agent;
mod roster;

#[cfg(test)]
mod tests;

use md_agent::parse_md_agent;
pub use roster::{build_roster_section, inject_roster_into_prompt};

/// Discovers and holds the project's agent configs.
pub struct AgentRegistry {
    /// Ordered map: agent_name → (AgentConfig, source_path).
    ///
    /// Insertion order reflects discovery order (highest priority dir first),
    /// but because shadowing skips already-inserted names, the first insert
    /// wins and later occurrences are dropped.
    agents: IndexMap<String, (AgentConfig, PathBuf)>,
}

/// Summary view of a discovered agent for the `agents list` subcommand.
///
/// Why: The raw `AgentConfig` is heavy (full system prompt, LLM params);
/// this thin view is enough for listing + capability debugging.
#[derive(Debug, Clone)]
pub struct AgentSummary {
    pub name: String,
    pub source: PathBuf,
    pub description: String,
    pub roles: Vec<String>,
    pub languages: Vec<String>,
    pub frameworks: Vec<String>,
    pub tags: Vec<String>,
}

impl AgentRegistry {
    /// Scan the given directories in priority order (first = highest).
    ///
    /// Why: Earlier directories shadow later ones for the same agent name, so
    /// `.open-mpm/agents/engineer.toml` in the project overrides a bundled
    /// `config/agents/engineer.toml`. Missing dirs are silently skipped so
    /// operators don't need to pre-create every path.
    /// What: Walks each directory for `*.toml` files, parses each via
    /// `AgentConfig::load`, and inserts the first occurrence of each agent
    /// name. Malformed files log a warn and are skipped.
    /// Test: `registry_loads_from_multiple_dirs_with_priority`,
    /// `registry_skips_missing_dirs_silently`.
    pub fn load(search_paths: &[PathBuf]) -> Self {
        let mut agents: IndexMap<String, (AgentConfig, PathBuf)> = IndexMap::new();

        for dir in search_paths {
            if !dir.is_dir() {
                tracing::debug!(path = %dir.display(), "agent search path missing, skipping");
                continue;
            }
            let entries = match std::fs::read_dir(dir) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(path = %dir.display(), error = %e, "failed to read agent dir");
                    continue;
                }
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let ext = path.extension().and_then(|s| s.to_str());
                let parsed = match ext {
                    Some("toml") => AgentConfig::load(&path),
                    Some("md") => parse_md_agent(&path),
                    _ => continue,
                };
                match parsed {
                    Ok(cfg) => {
                        let name = cfg.agent.name.clone();
                        if agents.contains_key(&name) {
                            tracing::debug!(
                                agent = %name,
                                shadowed = %path.display(),
                                "lower-priority agent copy shadowed by earlier dir"
                            );
                            continue;
                        }
                        tracing::debug!(
                            agent = %name,
                            source = %path.display(),
                            "discovered agent"
                        );
                        agents.insert(name, (cfg, path));
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = %path.display(),
                            error = %format!("{e:#}"),
                            "failed to parse agent file, skipping"
                        );
                    }
                }
            }
        }

        Self { agents }
    }

    /// Look up an agent by exact name.
    ///
    /// Why: Exact-name lookup is the fast path when the PM already decided
    /// which agent to delegate to (e.g. `--agent python-engineer`).
    /// What: Returns `None` if the agent was not discovered.
    /// Test: Covered implicitly by the priority + best-match tests.
    #[allow(dead_code)] // Wired into PM dispatch in a follow-up PR.
    pub fn get(&self, name: &str) -> Option<&AgentConfig> {
        self.agents.get(name).map(|(cfg, _)| cfg)
    }

    /// Number of discovered agents.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.agents.len()
    }

    /// Whether the registry discovered any agents.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }

    /// Pick the best-matching agent name for a task given capability hints.
    ///
    /// Why: The PM receives free-form task text; upstream heuristics extract
    /// role/language/framework/tag signals and this method turns those into
    /// a concrete agent choice without a hard-coded switch statement.
    ///
    /// Design: open-mpm intentionally ships ONE language specialist
    /// (`python-engineer`) and a generic `engineer` agent that handles all
    /// other languages via runtime skill injection (see
    /// `.open-mpm/skills/languages/`). For non-Python engineering tasks, the
    /// generic `engineer` should win — the right language idiom skill
    /// (rust-idiomatic.md, go-idiomatic.md, etc.) is injected at delegation
    /// time, not encoded as a separate agent.
    ///
    /// What: Scoring — role match = 10pts, language match = 5pts, framework
    /// match = 5pts, tag match = 2pts. Plus a +50 bonus when the detected
    /// language is `python` AND the candidate is `python-engineer` (the one
    /// language specialist we ship). Critically, when the task has language
    /// signals AND an agent declares languages but NONE match, that agent
    /// is disqualified — this prevents `python-engineer` (which declares
    /// `["python"]` + four Python frameworks) from out-scoring `engineer`
    /// on Rust/Go/TS tasks via tag/role specificity tiebreaks. Ties broken
    /// by specificity (more non-empty capability fields). Returns `None`
    /// when no agents are registered or no candidate scores above 0.
    ///
    /// Test: `registry_best_match_prefers_specific_over_general`,
    /// `registry_best_match_uses_engineer_for_non_python`.
    #[allow(dead_code)] // Wired into PM delegation in a follow-up PR.
    pub fn best_match(
        &self,
        role: Option<&str>,
        languages: &[&str],
        frameworks: &[&str],
        tags: &[&str],
    ) -> Option<&str> {
        if self.agents.is_empty() {
            return None;
        }

        let mut best: Option<(&str, i32, usize)> = None;
        for (name, (cfg, _)) in &self.agents {
            let caps = cfg.agent.capabilities.as_ref();
            let mut score = 0i32;

            if let (Some(role), Some(c)) = (role, caps)
                && c.roles.iter().any(|r| r.eq_ignore_ascii_case(role))
            {
                score += 10;
            }
            if let Some(c) = caps {
                for want in languages {
                    if c.languages.iter().any(|l| l.eq_ignore_ascii_case(want)) {
                        score += 5;
                    }
                }
                for want in frameworks {
                    if c.frameworks.iter().any(|f| f.eq_ignore_ascii_case(want)) {
                        score += 5;
                    }
                }
                for want in tags {
                    if c.tags.iter().any(|t| t.eq_ignore_ascii_case(want)) {
                        score += 2;
                    }
                }
            }

            // Disqualify language-mismatched specialists.
            //
            // Why: When the task names a language (e.g. "rust") and a
            // candidate declares a non-empty `languages` list with no
            // overlap, that candidate is the wrong specialist. Without this
            // gate, `python-engineer` (which declares `["python"]` + four
            // Python frameworks) wins the specificity tiebreak over the
            // generic `engineer` (which declares no languages) on a Rust
            // task — both score 10 from role match, but python-engineer has
            // more non-empty capability fields. Skipping these mismatched
            // specialists routes non-Python language tasks to `engineer`
            // where runtime skill injection supplies the right language
            // idioms.
            // What: Only applies when the task has language signals AND the
            // agent declares a non-empty language list. Agents with no
            // declared languages (the generic `engineer`) remain eligible.
            if !languages.is_empty()
                && let Some(c) = caps
                && !c.languages.is_empty()
                && !languages
                    .iter()
                    .any(|want| c.languages.iter().any(|l| l.eq_ignore_ascii_case(want)))
            {
                continue;
            }

            // Priority bonus: route Python tasks to python-engineer, the one
            // language specialist we ship. All other languages are handled
            // by the generic `engineer` agent with runtime skill injection.
            //
            // Gate: only apply when the requested role is "engineer" (or
            // unspecified). Otherwise a task like "Plan a multi-file Python
            // project" — which legitimately wants plan-agent — would be
            // hijacked into python-engineer just because the task text
            // mentions Python.
            let role_allows_lang_bonus = role
                .map(|r| r.eq_ignore_ascii_case("engineer"))
                .unwrap_or(true);
            if role_allows_lang_bonus
                && name.eq_ignore_ascii_case("python-engineer")
                && languages.iter().any(|l| l.eq_ignore_ascii_case("python"))
            {
                score += 50;
            }

            if score == 0 {
                continue;
            }

            let specificity = caps
                .map(|c| {
                    usize::from(!c.roles.is_empty())
                        + usize::from(!c.languages.is_empty())
                        + usize::from(!c.frameworks.is_empty())
                        + usize::from(!c.tags.is_empty())
                })
                .unwrap_or(0);

            match best {
                None => best = Some((name.as_str(), score, specificity)),
                Some((_, prev_score, prev_spec)) => {
                    if score > prev_score || (score == prev_score && specificity > prev_spec) {
                        best = Some((name.as_str(), score, specificity));
                    }
                }
            }
        }

        best.map(|(name, _, _)| name)
    }

    /// List all discovered agents with their source and capability summary.
    ///
    /// Why: Powers the `open-mpm agents list` subcommand and startup log
    /// summary so operators can verify their overrides are picked up.
    /// What: Returns one `AgentSummary` per discovered agent, preserving
    /// discovery (priority) order.
    /// Test: `registry_list_returns_all_agents`.
    pub fn list(&self) -> Vec<AgentSummary> {
        self.agents
            .iter()
            .map(|(name, (cfg, path))| {
                let (roles, languages, frameworks, tags) = match &cfg.agent.capabilities {
                    Some(c) => (
                        c.roles.clone(),
                        c.languages.clone(),
                        c.frameworks.clone(),
                        c.tags.clone(),
                    ),
                    None => (vec![], vec![], vec![], vec![]),
                };
                AgentSummary {
                    name: name.clone(),
                    source: path.clone(),
                    description: cfg.agent.description.clone(),
                    roles,
                    languages,
                    frameworks,
                    tags,
                }
            })
            .collect()
    }
}

/// Compute the agent search paths in priority order (highest first).
///
/// Why: Centralizes the discovery policy — project-level overrides beat
/// user-level overrides beat bundled defaults — so every call site
/// (`main.rs`, `ompm agents list`, tests) sees the same order.
/// What: Returns, in order: `.open-mpm/agents`, `.claude/agents`,
/// `~/.open-mpm/agents`, `~/.claude/agents`, `<config_dir>/agents`.
/// Test: `agent_search_paths_order`.
pub fn agent_search_paths(config_dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    paths.push(PathBuf::from(".open-mpm/agents"));
    paths.push(PathBuf::from(".claude/agents"));
    if let Some(home) = std::env::var_os("HOME") {
        paths.push(PathBuf::from(home.clone()).join(".open-mpm/agents"));
        paths.push(PathBuf::from(home).join(".claude/agents"));
    }
    paths.push(config_dir.join("agents"));
    paths
}
