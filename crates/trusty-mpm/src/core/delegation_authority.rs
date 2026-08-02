//! Delegation authority — scan deployed agents and render a routing section.
//!
//! Why: the orchestrating Claude Code instance needs to know which agents are
//! deployed and what each one handles so it can route work correctly; that
//! list is dynamic (it depends on what was deployed) and must be regenerated
//! at every session start.
//! What: [`scan_agents`] reads every `.md` file in an agents directory, parses
//! its frontmatter, and returns one [`AgentSummary`] per deployable (non-base)
//! agent; [`generate_authority`] folds those summaries into a Markdown section
//! injected into the session launch instructions;
//! [`deployed_roster_section`] resolves the tiers a launched session actually
//! loads agents from and renders that live roster for the PM prompt (#4069).
//! Test: `cargo test -p trusty-mpm-core delegation_authority` covers scanning,
//! base-agent exclusion, the empty directory, and both render branches.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::frontmatter::parse_kv_line;

/// A deployed agent as advertised to the orchestrating instance.
///
/// Why: the delegation authority section needs a small, render-ready view of
/// each agent — name, role, what it handles, its foundation chain, and a model
/// hint — without exposing the full composed agent body.
/// What: the display fields parsed from a composed agent's frontmatter plus the
/// resolved `extends` chain (base-first, ending in the agent itself).
/// Test: exercised by every `scan_*` and `generate_*` test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSummary {
    /// Agent name (frontmatter `name`, falling back to the file stem).
    pub name: String,
    /// Agent role (frontmatter `role`, falling back to `name`).
    pub role: String,
    /// One-line description of what the agent handles, if declared.
    pub description: Option<String>,
    /// Model hint (frontmatter `model`), if declared.
    pub model: Option<String>,
    /// Resolved inheritance chain, base-first, e.g.
    /// `["base-agent", "base-engineer", "engineer"]`.
    pub extends_chain: Vec<String>,
}

/// File name of the deploy manifest, excluded from agent scans.
const MANIFEST_FILE: &str = "manifest.json";

/// The minimal frontmatter fields a composed agent advertises.
///
/// Why: scanning only needs a handful of YAML keys; a tiny struct avoids a
/// full YAML dependency and keeps parsing explicit.
/// What: the display fields plus `extends` (composed agents normally carry no
/// `extends`, but it is parsed so a hand-written source dir still resolves).
/// Test: exercised indirectly by every `scan_*` test.
#[derive(Debug, Default)]
struct AgentFrontmatter {
    name: Option<String>,
    role: Option<String>,
    description: Option<String>,
    model: Option<String>,
    extends: Option<String>,
}

/// Parse the leading `---` frontmatter block of a Markdown document.
///
/// Why: composed agents store their metadata in a YAML-ish frontmatter block;
/// the scanner reads just the keys it needs without a YAML library.
/// What: if the document opens with a `---` line, collects `key: value` pairs
/// until the closing `---`; quotes are stripped and keys lower-cased. A
/// document with no frontmatter yields an all-`None` result.
/// Test: `scan_finds_agents` (frontmatter present), `scan_handles_no_frontmatter`.
fn parse_frontmatter(raw: &str) -> AgentFrontmatter {
    let trimmed = raw.trim_start_matches(['\u{feff}']);
    let mut lines = trimmed.lines();

    match lines.next() {
        Some(first) if first.trim() == "---" => {}
        _ => return AgentFrontmatter::default(),
    }

    let mut fm = AgentFrontmatter::default();
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        // Use the shared parser so colon-containing values (URLs, timestamps,
        // model ids) are preserved rather than silently truncated.
        let Some((key, value)) = parse_kv_line(line) else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        match key.as_str() {
            "name" => fm.name = Some(value),
            "role" => fm.role = Some(value),
            "description" => fm.description = Some(value),
            "model" => fm.model = Some(value),
            "extends" => fm.extends = Some(value),
            _ => {}
        }
    }
    fm
}

/// Scan `agents_dir` and return summaries for all non-base agents.
///
/// Why: the orchestrating CC instance needs to know which agents exist
/// and what they handle so it can route work correctly.
///
/// What: reads all .md files in agents_dir, parses frontmatter (name,
/// role, description, model, extends), excludes BASE-* files and the
/// manifest file, returns one AgentSummary per deployable agent.
///
/// Foundation templates are identified by the `BASE-*` FILE NAME convention
/// alone (#4589). An earlier revision also dropped any agent whose frontmatter
/// `role:` began with `base`, which silently deleted three real, dispatchable
/// agents — `memory-manager`, `mpm-agent-manager`, `mpm-skills-manager` — from
/// every roster. `role:` is the wrong signal for the job: the roster is a union
/// over three tiers (see [`deployed_agent_dirs`]) and tm authors the frontmatter
/// in exactly one of them, so no asset edit can stop the same rule from eating
/// an operator's own agent in `<project>/.claude/agents` or `~/.claude/agents`.
/// The file-name convention is tm's own, applies to the files tm actually
/// ships, and is checked before the file is even read.
///
/// Test: `scan_finds_agents`, `scan_excludes_base_agents`,
/// `agent_with_a_base_role_but_no_base_filename_stays_in_the_roster`,
/// `scan_empty_dir`
pub fn scan_agents(agents_dir: &Path) -> Vec<AgentSummary> {
    // An ABSENT tier is normal (not every launch mode populates every tier) and
    // stays silent. Any OTHER enumeration failure — permissions, a bad mount, an
    // I/O fault — is NOT normal and must never be indistinguishable from "empty"
    // (#4069 review): a tier that fails to enumerate silently shrinks the roster,
    // and if every tier fails the prompt reverts to the stale bundled asset with
    // no alarm anywhere. Fail open (a launch must not be blocked by one bad
    // directory) but never fail quiet.
    let entries = match std::fs::read_dir(agents_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            tracing::warn!(
                dir = %agents_dir.display(),
                %err,
                "agent tier could not be enumerated; the delegation roster may be incomplete"
            );
            return Vec::new();
        }
    };

    let mut summaries: Vec<AgentSummary> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if file_name.eq_ignore_ascii_case(MANIFEST_FILE) {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        // Exclude BASE-* source/foundation files by file name, before any read.
        if stem.to_ascii_lowercase().starts_with("base") {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let fm = parse_frontmatter(&raw);
        let name = fm.name.clone().unwrap_or_else(|| stem.to_string());
        let role = fm.role.clone().unwrap_or_else(|| name.clone());
        let extends_chain = build_extends_chain(fm.extends.as_deref(), &name);

        summaries.push(AgentSummary {
            name,
            role,
            description: fm.description,
            model: fm.model,
            extends_chain,
        });
    }

    // Deterministic order so the rendered section is stable across runs.
    summaries.sort_by_key(|a| a.name.clone());
    summaries
}

/// Build a base-first inheritance chain from a single `extends` parent.
///
/// Why: composed agents flatten their inheritance into one file and normally
/// carry no `extends`; when one is present we still record the immediate
/// parent so the rendered "Foundation" line is informative.
/// What: returns `[parent, name]` when an `extends` parent exists, otherwise
/// just `[name]`.
/// Test: `scan_finds_agents` asserts the single-element chain.
fn build_extends_chain(extends: Option<&str>, name: &str) -> Vec<String> {
    match extends {
        Some(parent) if !parent.is_empty() => {
            vec![parent.to_string(), name.to_string()]
        }
        _ => vec![name.to_string()],
    }
}

/// Generate the delegation authority Markdown section from summaries.
///
/// Why: injected into session launch instructions so the orchestrating
/// instance knows its delegation options.
///
/// What: produces a Markdown section listing each agent with name,
/// description, extends chain, and model hint.
///
/// Zero-information lines are omitted (#4069 review): `Role` is skipped when it
/// merely repeats the `###` heading (it falls back to `name` when unset, true
/// for ~14 of 40 deployed agents) and `Foundation` is skipped for a
/// single-element chain, which renders as the agent citing itself as its own
/// foundation (true for every composed agent, since composition flattens
/// `extends`). This section is re-emitted into EVERY PM session, so ~2 KB of
/// per-session noise is worth deleting.
///
/// Test: `generate_authority_nonempty`, `generate_authority_empty`,
/// `generate_authority_omits_self_referential_lines`
pub fn generate_authority(agents: &[AgentSummary]) -> String {
    let mut out = String::from("## Delegation Authority\n\n");

    if agents.is_empty() {
        out.push_str(
            "No delegatable agents are currently available. Handle all work \
             directly until agents are deployed.\n",
        );
        return out;
    }

    out.push_str(
        "The following agents are available for delegation. Route work to the\n\
         appropriate agent based on task type.\n\n",
    );

    for agent in agents {
        out.push_str(&format!("### {}\n", agent.name));
        if agent.role != agent.name {
            out.push_str(&format!("- **Role:** {}\n", agent.role));
        }
        let handles = agent
            .description
            .as_deref()
            .unwrap_or("(no description provided)");
        out.push_str(&format!("- **Handles:** {handles}\n"));
        if agent.extends_chain.len() > 1 {
            out.push_str(&format!(
                "- **Foundation:** {}\n",
                agent.extends_chain.join(" → ")
            ));
        }
        if let Some(model) = &agent.model {
            out.push_str(&format!("- **Model:** {model}\n"));
        }
        out.push('\n');
    }

    out
}

/// Agent directories a launched session can load composed agents from,
/// highest precedence first.
///
/// Why (#4069): the roster the PM must route to is whatever Claude Code will
/// actually load, and that is a union of tiers rather than a single directory.
/// Since #4409 every BUNDLED agent deploys into exactly one of them — the
/// tm-owned `CLAUDE_CONFIG_DIR/agents` — while `<project>/.claude/agents` holds
/// only agents the operator hand-placed (and, in future, project-custom
/// trusty-built agents), and `~/.claude/agents` holds whatever the operator's
/// own generic Claude Code install carries, which tm never writes to. All three
/// are still scanned because a session can legitimately resolve an agent from
/// any of them, and the project tier still WINS on a name collision. Prompt
/// composition only ever receives a `project_dir`, so it resolves all three
/// tiers here rather than depending on a launch-mode value it cannot see.
/// What: returns `<project>/.claude/agents`, then the active managed
/// `CLAUDE_CONFIG_DIR/agents` (the `CLAUDE_CONFIG_DIR` env var when set,
/// otherwise [`managed_claude_config_dir`]), then
/// `FrameworkPaths::default().claude_agents_dir()`. Paths are returned whether
/// or not they exist; [`scan_agents`] treats an unreadable directory as empty.
/// Test: `deployed_agent_dirs_puts_project_tier_first`.
pub fn deployed_agent_dirs(project_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![project_dir.join(".claude").join("agents")];

    let managed = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(crate::core::trusty_tools_config::managed_claude_config_dir);
    if let Some(managed) = managed {
        dirs.push(managed.join("agents"));
    }

    dirs.push(crate::core::paths::FrameworkPaths::default().claude_agents_dir());
    dirs
}

/// Merge the agents found across `dirs` into one roster, earlier dirs winning.
///
/// Why: the tiers returned by [`deployed_agent_dirs`] overlap — the same agent
/// is normally deployed into more than one of them — so a naive concatenation
/// would advertise duplicates. Claude Code resolves a same-named agent by tier
/// precedence, and the rendered roster must describe the copy that actually
/// wins.
/// What: scans each directory in order and keeps the FIRST summary seen per
/// agent name, returning them name-sorted for a stable prompt. The dedup key is
/// LOWER-CASED — agent names are case-insensitive to Claude Code's dispatcher,
/// so `QA` in one tier and `qa` in another are one agent, not two.
/// Test: `roster_from_dirs_dedupes_with_first_dir_winning`,
/// `roster_from_dirs_dedup_is_case_insensitive`.
pub fn roster_from_dirs(dirs: &[PathBuf]) -> Vec<AgentSummary> {
    let mut by_name: BTreeMap<String, AgentSummary> = BTreeMap::new();
    for dir in dirs {
        for agent in scan_agents(dir) {
            by_name
                .entry(agent.name.to_ascii_lowercase())
                .or_insert(agent);
        }
    }
    by_name.into_values().collect()
}

/// THE agent roster for `project_dir`. Every consumer calls this one function.
///
/// Why (#4588): a roster resolved twice is a roster that drifts. The PM prompt
/// took the three-tier union while `tm session start` printed a count taken
/// from a single directory, so the operator was told 34 when the PM had been
/// given 39 — the two numbers were never wrong in the same way at the same
/// time, because they were never the same computation. There is now exactly one
/// implementation, and `tm session start`, the delegation section injected into
/// the PM prompt, and `tm doctor` all read it.
/// What: unions [`deployed_agent_dirs`] via [`roster_from_dirs`], name-sorted
/// and deduplicated with the highest-precedence tier winning.
/// Test: `session_start_count_matches_the_delivered_delegation_roster`
/// (instruction_pipeline_tests) is the cross-consumer agreement gate; the
/// tier-union behaviour itself is covered by the `roster_from_dirs_*` tests.
/// This function consults machine-global tiers, so it has no hermetic direct
/// test of its own.
pub fn resolve_roster(project_dir: &Path) -> Vec<AgentSummary> {
    roster_from_dirs(&deployed_agent_dirs(project_dir))
}

/// Render the LIVE deployed roster for a project, or `None` when none is found.
///
/// Why (#4069): `build_instructions` already computed this section via
/// [`generate_authority`], but its output is merged into a `PipelineOutput`
/// string that the prompt composer never reads — `resolve_pm_prompt` fell back
/// to the static `AGENT_DELEGATION.md` asset, so a 42-agent deployment was
/// advertised to the PM as the asset's hand-maintained 8-row table and agents
/// such as `ticketing` and `memory-manager` were invisible. Regenerating here
/// (rather than threading the pipeline's string down) is the only shape that
/// works for every composer: `build_system_prompt_for*` and its callers —
/// `tm session instructions`, the tmux connect path, the daemon spawn — receive
/// a `project_dir` and nothing else, and two of them never run the pipeline at
/// all, so there is no `PipelineOutput` available to thread.
/// What: resolves the roster via [`resolve_roster`] — the one resolver every
/// consumer shares — and renders it with [`generate_authority`]. Returns `None` when no agent is deployed
/// anywhere, so an unprovisioned environment keeps exactly the previous
/// behaviour (the bundled asset alone) instead of being told it has no agents.
/// Test: `roster_from_dirs_ignores_missing_dirs` (the `None` branch's input) and
/// `instruction_overrides::tests::bundled_delegation_appends_deployed_roster`
/// (the rendered output reaching the delivered prompt). This function itself
/// consults machine-global tiers, so it has no hermetic direct test.
pub fn deployed_roster_section(project_dir: &Path) -> Option<String> {
    let agents = resolve_roster(project_dir);
    if agents.is_empty() {
        // Reverting to the bundled asset must be an OBSERVABLE event, not an
        // invisible one — this is the state #4069 describes, and if it recurs
        // (every tier empty, or every tier unreadable per `scan_agents`'s warn)
        // the operator needs a thread to pull rather than a silently stale
        // 8-name prompt that looks healthy.
        tracing::info!(
            project = %project_dir.display(),
            tiers = deployed_agent_dirs(project_dir).len(),
            "no deployed agents found in any tier; delegation section falls back to the \
             bundled asset alone"
        );
        return None;
    }
    Some(generate_authority(&agents))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Write `<name>.md` into `dir` with the given raw content.
    fn write_agent(dir: &Path, name: &str, content: &str) {
        fs::write(dir.join(format!("{name}.md")), content).expect("write agent");
    }

    #[test]
    fn scan_finds_agents() {
        // A directory with one deployable agent and one base agent must yield
        // exactly the deployable one, with its frontmatter parsed.
        let tmp = TempDir::new().unwrap();
        write_agent(
            tmp.path(),
            "engineer",
            "---\nname: engineer\nrole: engineer\nextends: base-engineer\n\
             description: Implements features and fixes bugs.\nmodel: sonnet\n---\n\n# Engineer\n",
        );
        write_agent(
            tmp.path(),
            "BASE-AGENT",
            "---\nname: base-agent\nrole: base\n---\n\n# Base\n",
        );

        let agents = scan_agents(tmp.path());
        assert_eq!(agents.len(), 1, "only the engineer is deployable");
        let engineer = &agents[0];
        assert_eq!(engineer.name, "engineer");
        assert_eq!(engineer.role, "engineer");
        assert_eq!(
            engineer.description.as_deref(),
            Some("Implements features and fixes bugs.")
        );
        assert_eq!(engineer.model.as_deref(), Some("sonnet"));
        assert_eq!(
            engineer.extends_chain,
            vec!["base-engineer".to_string(), "engineer".to_string()]
        );
    }

    #[test]
    fn scan_excludes_base_agents() {
        // Foundation templates are excluded by the `BASE-*` FILE NAME
        // convention, case-insensitively, and never by frontmatter (#4589 —
        // see `agent_with_a_base_role_but_no_base_filename_stays_in_the_roster`
        // for why the `role:` half was removed).
        let tmp = TempDir::new().unwrap();
        write_agent(
            tmp.path(),
            "BASE-AGENT",
            "---\nname: base-agent\nrole: base\n---\n\nfoundation\n",
        );
        write_agent(
            tmp.path(),
            "base-engineer",
            "---\nname: base-engineer\nrole: base-engineer\n---\n\nfoundation\n",
        );
        write_agent(
            tmp.path(),
            "qa",
            "---\nname: qa\nrole: qa\ndescription: Tests things.\n---\n\n# QA\n",
        );

        let agents = scan_agents(tmp.path());
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "qa");
    }

    #[test]
    fn agent_with_a_base_role_but_no_base_filename_stays_in_the_roster() {
        // #4589 REGRESSION. `memory-manager`, `mpm-agent-manager` and
        // `mpm-skills-manager` shipped with `role: base` and were therefore
        // dropped from every tier of the roster — deployed, dispatchable, and
        // invisible to the PM. The exclusion keys off the `BASE-*` FILE NAME
        // convention (tm's own, applied to the files tm ships) rather than a
        // frontmatter value tm does not control in the project and generic
        // `~/.claude/agents` tiers it also unions.
        let tmp = TempDir::new().unwrap();
        write_agent(
            tmp.path(),
            "memory-manager",
            "---\nname: memory-manager\nrole: base\ndescription: Manages memory.\n---\n\n# MM\n",
        );

        let agents = scan_agents(tmp.path());
        assert_eq!(
            agents.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            vec!["memory-manager"],
            "a non-`BASE-*` file must never be classified as a foundation \
             template by its `role:` value alone (#4589)"
        );
    }

    #[test]
    fn scan_empty_dir() {
        // An empty directory must yield an empty vec with no error.
        let tmp = TempDir::new().unwrap();
        let agents = scan_agents(tmp.path());
        assert!(agents.is_empty());
    }

    #[test]
    fn scan_missing_dir_is_empty() {
        // A non-existent directory must also yield an empty vec, not panic.
        let agents = scan_agents(Path::new("/no/such/agents/dir/xyz"));
        assert!(agents.is_empty());
    }

    #[test]
    fn scan_ignores_manifest_and_non_md() {
        // The deploy manifest and non-Markdown files must be skipped.
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("manifest.json"), "{}").unwrap();
        fs::write(tmp.path().join("notes.txt"), "hello").unwrap();
        write_agent(
            tmp.path(),
            "writer",
            "---\nname: writer\nrole: writer\n---\n\n# Writer\n",
        );
        let agents = scan_agents(tmp.path());
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "writer");
    }

    #[test]
    fn scan_handles_no_frontmatter() {
        // An agent file with no frontmatter falls back to the file stem.
        let tmp = TempDir::new().unwrap();
        write_agent(
            tmp.path(),
            "plain",
            "# Plain agent\n\nNo frontmatter here.\n",
        );
        let agents = scan_agents(tmp.path());
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "plain");
        assert_eq!(agents[0].role, "plain");
        assert_eq!(agents[0].extends_chain, vec!["plain".to_string()]);
    }

    #[test]
    fn generate_authority_nonempty() {
        // A single agent renders under the heading with its name and details.
        let agents = vec![AgentSummary {
            name: "engineer".to_string(),
            role: "engineer".to_string(),
            description: Some("Implements features.".to_string()),
            model: Some("sonnet".to_string()),
            extends_chain: vec![
                "base-agent".to_string(),
                "base-engineer".to_string(),
                "engineer".to_string(),
            ],
        }];
        let md = generate_authority(&agents);
        assert!(md.contains("## Delegation Authority"));
        assert!(md.contains("### engineer"));
        assert!(md.contains("Implements features."));
        assert!(md.contains("base-agent → base-engineer → engineer"));
        assert!(md.contains("**Model:** sonnet"));
    }

    #[test]
    fn generate_authority_empty() {
        // With no agents the heading still renders, plus a "no agents" note.
        let md = generate_authority(&[]);
        assert!(md.contains("## Delegation Authority"));
        assert!(md.to_lowercase().contains("no delegatable agents"));
    }

    // ── colon-in-value regression tests (issue #389) ─────────────────────────

    #[test]
    fn scan_preserves_url_in_description() {
        // A `description:` value that is or contains a URL must not be
        // silently truncated at the first colon.
        let tmp = TempDir::new().unwrap();
        write_agent(
            tmp.path(),
            "docs-agent",
            "---\nname: docs-agent\nrole: docs\ndescription: See https://docs.example.com/guide\n---\n\n# Docs\n",
        );
        let agents = scan_agents(tmp.path());
        assert_eq!(agents.len(), 1);
        assert_eq!(
            agents[0].description.as_deref(),
            Some("See https://docs.example.com/guide"),
            "URL in description must not be truncated"
        );
    }

    #[test]
    fn scan_preserves_bedrock_model_id() {
        // A `model:` value containing a bedrock model id (with `/` and `.`)
        // must survive the scan without truncation.
        let tmp = TempDir::new().unwrap();
        write_agent(
            tmp.path(),
            "ml-agent",
            "---\nname: ml-agent\nrole: ml\nmodel: bedrock/us.anthropic.claude-sonnet-4-6\n---\n\n# ML\n",
        );
        let agents = scan_agents(tmp.path());
        assert_eq!(agents.len(), 1);
        assert_eq!(
            agents[0].model.as_deref(),
            Some("bedrock/us.anthropic.claude-sonnet-4-6"),
            "bedrock model id must be preserved verbatim"
        );
    }

    #[test]
    fn scan_preserves_timestamp_in_description() {
        // A description that embeds an ISO-8601 timestamp must keep the full
        // timestamp including the time component (colons after the first).
        let tmp = TempDir::new().unwrap();
        write_agent(
            tmp.path(),
            "timed-agent",
            "---\nname: timed-agent\nrole: timer\ndescription: Deployed at 2026-06-05T14:31:34\n---\n\n# Timed\n",
        );
        let agents = scan_agents(tmp.path());
        assert_eq!(agents.len(), 1);
        assert_eq!(
            agents[0].description.as_deref(),
            Some("Deployed at 2026-06-05T14:31:34"),
            "timestamp in description must not be truncated"
        );
    }

    #[test]
    fn every_bundled_non_base_agent_reaches_the_roster() {
        // Issue #4069 / #4589 REGRESSION GATE. The scanner's only exclusion is
        // the `BASE-*` file-name convention, so this asserts the property that
        // matters end to end: every bundled `.md` that is not a `BASE-*`
        // foundation file survives `scan_agents` and can therefore be routed
        // to. It is the asset-level half of the same invariant
        // `agent_with_a_base_role_but_no_base_filename_stays_in_the_roster`
        // asserts at the scanner level — one guards the shipped files, the
        // other guards the rule, and #4589 needed both.
        let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/assets/agents");

        let mut expected: Vec<String> = fs::read_dir(&assets)
            .expect("bundled agent assets dir")
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
            .filter_map(|p| {
                p.file_stem()
                    .and_then(|s| s.to_str())
                    .filter(|stem| !stem.to_ascii_lowercase().starts_with("base"))
                    .map(str::to_string)
            })
            .collect();
        expected.sort();

        let mut scanned: Vec<String> = scan_agents(&assets).into_iter().map(|a| a.name).collect();
        scanned.sort();

        let dropped: Vec<&String> = expected.iter().filter(|n| !scanned.contains(n)).collect();
        assert!(
            dropped.is_empty(),
            "bundled agents silently dropped from every roster (check for a stray \
             `role: base` in their frontmatter): {dropped:?}"
        );
        assert_eq!(
            scanned.len(),
            expected.len(),
            "every non-BASE bundled agent must be delegatable"
        );
    }

    #[test]
    fn generate_authority_omits_self_referential_lines() {
        // Review MEDIUM-3: `Role` that merely repeats the heading and a
        // single-element `Foundation` chain are pure noise, re-emitted into
        // every PM session. A real (multi-element) chain still renders.
        let agents = vec![AgentSummary {
            name: "ticketing".to_string(),
            role: "ticketing".to_string(),
            description: Some("Files tickets.".to_string()),
            model: None,
            extends_chain: vec!["ticketing".to_string()],
        }];
        let md = generate_authority(&agents);

        assert!(md.contains("### ticketing"));
        assert!(md.contains("Files tickets."));
        assert!(
            !md.contains("**Role:**"),
            "role duplicating the heading must be omitted"
        );
        assert!(
            !md.contains("**Foundation:**"),
            "a self-referential single-element chain must be omitted"
        );
    }

    #[test]
    fn roster_from_dirs_dedup_is_case_insensitive() {
        // Review LOW-1: agent names are case-insensitive to the dispatcher, so
        // `QA` and `qa` across two tiers are one agent, not two entries.
        let high = TempDir::new().unwrap();
        let low = TempDir::new().unwrap();
        write_agent(
            high.path(),
            "QA",
            "---\nname: QA\nrole: qa\ndescription: PROJECT TIER\n---\n\n# QA\n",
        );
        write_agent(
            low.path(),
            "qa",
            "---\nname: qa\nrole: qa\ndescription: USER TIER\n---\n\n# QA\n",
        );

        let roster = roster_from_dirs(&[high.path().to_path_buf(), low.path().to_path_buf()]);

        assert_eq!(roster.len(), 1, "QA and qa are the same agent");
        assert_eq!(roster[0].description.as_deref(), Some("PROJECT TIER"));
    }

    #[test]
    fn deployed_agent_dirs_puts_project_tier_first() {
        // Issue #4069: the project tier is the destination the daemon
        // managed-spawn path deploys into AND the only tier readable under
        // `--setting-sources project,local`, so it must rank first.
        let dirs = deployed_agent_dirs(Path::new("/proj"));
        assert_eq!(dirs[0], PathBuf::from("/proj/.claude/agents"));
        assert!(
            dirs.len() >= 2,
            "the user-level tier(s) must also be consulted"
        );
    }

    #[test]
    fn roster_from_dirs_dedupes_with_first_dir_winning() {
        // The same agent is normally deployed into more than one tier; the
        // higher-precedence copy must be the one described, exactly once.
        let high = TempDir::new().unwrap();
        let low = TempDir::new().unwrap();
        write_agent(
            high.path(),
            "qa",
            "---\nname: qa\nrole: qa\ndescription: PROJECT TIER\n---\n\n# QA\n",
        );
        write_agent(
            low.path(),
            "qa",
            "---\nname: qa\nrole: qa\ndescription: USER TIER\n---\n\n# QA\n",
        );
        write_agent(
            low.path(),
            "ops",
            "---\nname: ops\nrole: ops\ndescription: USER TIER\n---\n\n# Ops\n",
        );

        let roster = roster_from_dirs(&[high.path().to_path_buf(), low.path().to_path_buf()]);

        assert_eq!(roster.len(), 2, "qa must appear once, plus ops");
        assert_eq!(roster[0].name, "ops", "roster is name-sorted");
        assert_eq!(roster[1].name, "qa");
        assert_eq!(roster[1].description.as_deref(), Some("PROJECT TIER"));
    }

    #[test]
    fn roster_from_dirs_ignores_missing_dirs() {
        // A tier that does not exist is an empty tier, never a failure — this is
        // the input that makes `deployed_roster_section` return `None`, leaving
        // an unprovisioned environment with the bundled asset alone.
        let roster = roster_from_dirs(&[PathBuf::from("/nonexistent/agents")]);
        assert!(roster.is_empty());
        assert!(roster_from_dirs(&[]).is_empty());
    }
}
