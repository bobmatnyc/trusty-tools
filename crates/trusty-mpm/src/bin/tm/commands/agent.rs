//! `tm agent` command handler — list/show the deployed agent roster's
//! declared skills (DOC-42, issue #2889).
//!
//! Why: `docs/specs/agent-bundled-skills.md` §SPEC-AGENTSKILLS-04 requires
//! the `skills:` declaration and its 3-tier resolution to be visible from the
//! CLI. This reads the operator's already-deployed roster
//! (`~/.claude/agents/`, `~/.claude/skills/`) directly — no daemon round-trip
//! is needed, mirroring `tm validate`.
//! What: `agent` dispatches `AgentAction::List` (every deployed agent, one
//! line/JSON-object each) and `AgentAction::Show` (one agent's full metadata
//! plus each declared skill's resolved tier, or `NOT FOUND`).
//! Test: `cli_parses_agent_list`, `cli_parses_agent_show` in `cli/tests.rs`
//! cover argument parsing; `agent_list_*`/`agent_show_*` in this file's
//! `tests` module cover the rendering/resolution logic against temp
//! directories.

use std::collections::BTreeSet;

use trusty_mpm::core::agent_metadata::{AgentMetadata, read_agent_metadata};
use trusty_mpm::core::paths::FrameworkPaths;
use trusty_mpm::core::skill_tiers::{
    SkillTier, list_project_custom_stems, list_source_stems, resolve_skill_tier,
};

use crate::cli::AgentAction;

/// `tm agent <list|show>` — dispatch to the requested view.
pub(crate) async fn agent(action: AgentAction) -> anyhow::Result<()> {
    let paths = FrameworkPaths::default();
    match action {
        AgentAction::List { json } => list_agents(&paths, json),
        AgentAction::Show { name, json } => show_agent(&paths, &name, json),
    }
}

/// The 3-tier skill stem sets, resolved once per command invocation.
///
/// Why: every declared skill on every agent needs the SAME resolution
/// (§SPEC-AGENTSKILLS-04's "resolution metadata" column); computing the three
/// stem sets once avoids a directory re-scan per skill.
/// What: wraps [`resolve_skill_tier`] with the resolved sets bound.
/// Test: `tier_label_reports_not_found_for_dangling_skill`,
/// `tier_label_reports_resolved_tier`.
struct SkillTiers {
    project: BTreeSet<String>,
    user: BTreeSet<String>,
    bundled: BTreeSet<String>,
}

impl SkillTiers {
    fn resolve(paths: &FrameworkPaths) -> Self {
        Self {
            bundled: list_source_stems(&paths.skill_source_dir()).unwrap_or_default(),
            user: list_source_stems(&paths.user_skill_source_dir()).unwrap_or_default(),
            project: list_project_custom_stems(&paths.claude_skills_dir()).unwrap_or_default(),
        }
    }

    /// The tier label for `skill`, or `"NOT FOUND"` when it resolves in no
    /// tier — the exact wording `docs/specs/agent-bundled-skills.md`
    /// §SPEC-AGENTSKILLS-04's `show` example uses.
    fn tier_label(&self, skill: &str) -> &'static str {
        resolve_skill_tier(skill, &self.project, &self.user, &self.bundled)
            .map(SkillTier::label)
            .unwrap_or("NOT FOUND")
    }
}

/// The sorted stems of every deployed `*.md` agent file (manifest excluded).
///
/// Why: `list` and `show` both need the deployed roster's names; sorting
/// gives stable, scriptable output.
/// What: reads `paths.claude_agents_dir()`; a missing directory yields an
/// empty list rather than an error (an unprovisioned roster is not this
/// command's concern — `tm doctor` covers that).
/// Test: `deployed_agent_names_sorted_and_filtered`.
fn deployed_agent_names(paths: &FrameworkPaths) -> Vec<String> {
    let dir = paths.claude_agents_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                return None;
            }
            path.file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string)
        })
        .collect();
    names.sort();
    names
}

/// Render one skill as a JSON object `{"name": ..., "tier": ...}`.
fn skill_json(skill: &str, tiers: &SkillTiers) -> serde_json::Value {
    serde_json::json!({ "name": skill, "tier": tiers.tier_label(skill) })
}

fn list_agents(paths: &FrameworkPaths, json: bool) -> anyhow::Result<()> {
    let tiers = SkillTiers::resolve(paths);
    let agents_dir = paths.claude_agents_dir();
    let names = deployed_agent_names(paths);

    if json {
        let entries: Vec<serde_json::Value> = names
            .iter()
            .map(|name| {
                let meta = read_agent_metadata(&agents_dir.join(format!("{name}.md")));
                let skills: Vec<serde_json::Value> =
                    meta.skills.iter().map(|s| skill_json(s, &tiers)).collect();
                serde_json::json!({ "name": name, "role": meta.role, "skills": skills })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }

    println!("agents ({}):", names.len());
    for name in &names {
        let meta = read_agent_metadata(&agents_dir.join(format!("{name}.md")));
        if meta.skills.is_empty() {
            println!("  {name}");
        } else {
            let rendered: Vec<String> = meta
                .skills
                .iter()
                .map(|s| format!("{s} ({})", tiers.tier_label(s)))
                .collect();
            println!("  {name}  skills: {}", rendered.join(", "));
        }
    }
    Ok(())
}

fn show_agent(paths: &FrameworkPaths, name: &str, json: bool) -> anyhow::Result<()> {
    let agents_dir = paths.claude_agents_dir();
    let path = agents_dir.join(format!("{name}.md"));
    if !path.is_file() {
        anyhow::bail!("agent `{name}` not found in {}", agents_dir.display());
    }
    let meta = read_agent_metadata(&path);
    let tiers = SkillTiers::resolve(paths);
    render_agent_show(name, &meta, &tiers, json)
}

/// Render a single agent's metadata and skill resolution (pure, testable).
///
/// Why: separating the presentation from the filesystem lookups in
/// [`show_agent`] lets the rendering logic be exercised against a
/// hand-built [`AgentMetadata`] without touching disk.
/// What: prints either a formatted text block or pretty-printed JSON,
/// depending on `json`.
/// Test: `render_agent_show_text_includes_skill_tier`,
/// `render_agent_show_json_shape`.
fn render_agent_show(
    name: &str,
    meta: &AgentMetadata,
    tiers: &SkillTiers,
    json: bool,
) -> anyhow::Result<()> {
    if json {
        let skills: Vec<serde_json::Value> =
            meta.skills.iter().map(|s| skill_json(s, tiers)).collect();
        let value = serde_json::json!({
            "name": meta.name.clone().unwrap_or_else(|| name.to_string()),
            "role": meta.role,
            "model": meta.model,
            "description": meta.description,
            "skills": skills,
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    println!("Name: {}", meta.name.as_deref().unwrap_or(name));
    if let Some(role) = &meta.role {
        println!("Role: {role}");
    }
    if let Some(model) = &meta.model {
        println!("Model: {model}");
    }
    if let Some(description) = &meta.description {
        println!("Description: {description}");
    }
    if meta.skills.is_empty() {
        println!("Declared Skills: (none)");
    } else {
        println!("Declared Skills:");
        for skill in &meta.skills {
            println!("  - {skill} ({})", tiers.tier_label(skill));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn tier_label_reports_resolved_tier() {
        let tiers = SkillTiers {
            project: BTreeSet::new(),
            user: set(&["u"]),
            bundled: set(&["b"]),
        };
        assert_eq!(tiers.tier_label("u"), "user-custom");
        assert_eq!(tiers.tier_label("b"), "bundled");
    }

    #[test]
    fn tier_label_reports_not_found_for_dangling_skill() {
        let tiers = SkillTiers {
            project: BTreeSet::new(),
            user: BTreeSet::new(),
            bundled: BTreeSet::new(),
        };
        assert_eq!(tiers.tier_label("missing"), "NOT FOUND");
    }

    #[test]
    fn deployed_agent_names_sorted_and_filtered() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = FrameworkPaths::under(tmp.path());
        let dir = paths.claude_agents_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("zebra.md"), "z").unwrap();
        std::fs::write(dir.join("alpha.md"), "a").unwrap();
        std::fs::write(dir.join(".trusty-mpm-manifest.json"), "{}").unwrap();
        std::fs::write(dir.join("notes.txt"), "ignored").unwrap();

        let names = deployed_agent_names(&paths);
        assert_eq!(names, vec!["alpha".to_string(), "zebra".to_string()]);
    }

    #[test]
    fn deployed_agent_names_missing_dir_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = FrameworkPaths::under(tmp.path());
        assert!(deployed_agent_names(&paths).is_empty());
    }

    #[test]
    fn render_agent_show_text_includes_skill_tier() {
        let meta = AgentMetadata {
            name: Some("code-critic".to_string()),
            role: Some("qa".to_string()),
            skills: vec![
                "code-review-standards".to_string(),
                "missing-skill".to_string(),
            ],
            ..Default::default()
        };
        let tiers = SkillTiers {
            project: BTreeSet::new(),
            user: BTreeSet::new(),
            bundled: set(&["code-review-standards"]),
        };
        // The text path prints via println! — assert the underlying data
        // that drives it resolves as expected (the exact tier strings
        // `render_agent_show` interpolates).
        assert_eq!(tiers.tier_label("code-review-standards"), "bundled");
        assert_eq!(tiers.tier_label("missing-skill"), "NOT FOUND");
        assert!(render_agent_show("code-critic", &meta, &tiers, false).is_ok());
    }

    #[test]
    fn render_agent_show_json_shape() {
        let meta = AgentMetadata {
            name: Some("code-critic".to_string()),
            role: Some("qa".to_string()),
            skills: vec!["code-review-standards".to_string()],
            ..Default::default()
        };
        let tiers = SkillTiers {
            project: BTreeSet::new(),
            user: BTreeSet::new(),
            bundled: set(&["code-review-standards"]),
        };
        assert!(render_agent_show("code-critic", &meta, &tiers, true).is_ok());
    }

    #[test]
    fn show_agent_missing_file_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = FrameworkPaths::under(tmp.path());
        let result = show_agent(&paths, "nonexistent", false);
        assert!(result.is_err());
    }

    #[test]
    fn list_agents_reads_declared_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = FrameworkPaths::under(tmp.path());
        let agents = paths.claude_agents_dir();
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(
            agents.join("code-critic.md"),
            "---\nname: code-critic\nrole: qa\nskills: [code-review-standards]\n---\n\nBody.\n",
        )
        .unwrap();
        assert!(list_agents(&paths, false).is_ok());
        assert!(list_agents(&paths, true).is_ok());
    }
}
