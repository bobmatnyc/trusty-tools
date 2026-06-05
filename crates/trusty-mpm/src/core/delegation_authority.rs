//! Delegation authority — scan deployed agents and render a routing section.
//!
//! Why: the orchestrating Claude Code instance needs to know which agents are
//! deployed and what each one handles so it can route work correctly; that
//! list is dynamic (it depends on what was deployed) and must be regenerated
//! at every session start.
//! What: [`scan_agents`] reads every `.md` file in an agents directory, parses
//! its frontmatter, and returns one [`AgentSummary`] per deployable (non-base)
//! agent; [`generate_authority`] folds those summaries into a Markdown section
//! injected into the session launch instructions.
//! Test: `cargo test -p trusty-mpm-core delegation_authority` covers scanning,
//! base-agent exclusion, the empty directory, and both render branches.

use std::path::Path;

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

/// Whether a role marks a foundation (non-delegatable) agent.
///
/// Why: `base-agent` / `base-engineer` and friends are inheritance foundations,
/// not work destinations; advertising them would mislead the router.
/// What: returns true when the role (lower-cased) starts with `base`.
/// Test: `scan_excludes_base_agents`.
fn is_base_role(role: &str) -> bool {
    role.trim().to_ascii_lowercase().starts_with("base")
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
/// Test: `scan_finds_agents`, `scan_excludes_base_agents`, `scan_empty_dir`
pub fn scan_agents(agents_dir: &Path) -> Vec<AgentSummary> {
    let Ok(entries) = std::fs::read_dir(agents_dir) else {
        return Vec::new();
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

        // Also exclude by declared role, catching base agents whose file name
        // does not follow the `base-` convention.
        if is_base_role(&role) {
            continue;
        }

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
/// Test: `generate_authority_nonempty`, `generate_authority_empty`
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
        out.push_str(&format!("- **Role:** {}\n", agent.role));
        let handles = agent
            .description
            .as_deref()
            .unwrap_or("(no description provided)");
        out.push_str(&format!("- **Handles:** {handles}\n"));
        out.push_str(&format!(
            "- **Foundation:** {}\n",
            agent.extends_chain.join(" → ")
        ));
        if let Some(model) = &agent.model {
            out.push_str(&format!("- **Model:** {model}\n"));
        }
        out.push('\n');
    }

    out
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
        // BASE-* files (by name) and base-role agents (by frontmatter) must
        // never appear in the scan results.
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
        // A file not following the `base-` name convention but with a base role.
        write_agent(
            tmp.path(),
            "foundation",
            "---\nname: foundation\nrole: base-thing\n---\n\nfoundation\n",
        );
        write_agent(
            tmp.path(),
            "qa",
            "---\nname: qa\nrole: qa\ndescription: Tests things.\n---\n\n# QA\n",
        );

        let agents = scan_agents(tmp.path());
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "qa");
        assert!(
            agents.iter().all(|a| !a.role.starts_with("base")),
            "no base-role agent should survive the scan"
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
}
