//! Tests for the never-deny floors, including one drift guard per source each
//! floor must cover (#8988 MCP, #8992 tools and skills).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::*;

/// `content/<sub>` in this checkout.
fn content(sub: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../content")
        .join(sub)
}

/// Every `.md` file under `dir`, recursively.
fn md_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).expect("read content dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            out.extend(md_files(&path));
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            out.push(path);
        }
    }
    out
}

/// Every token on an agent frontmatter `<key>:` line, e.g. `tools:`/`skills:`.
fn agent_frontmatter_tokens(key: &str) -> BTreeSet<String> {
    let prefix = format!("{key}:");
    let mut tokens = BTreeSet::new();
    for path in md_files(&content("agents")) {
        let text = std::fs::read_to_string(&path).expect("read agent file");
        for line in text.lines().filter(|l| l.starts_with(&prefix)) {
            let list = &line[prefix.len()..];
            for token in list.split(['[', ']', ',']) {
                let token = token.trim();
                if !token.is_empty() {
                    tokens.insert(token.to_string());
                }
            }
        }
    }
    tokens
}

/// Every `mcp__<server>` named on an agent's `tools:` line.
fn agent_tools_mcp_servers() -> BTreeSet<String> {
    agent_frontmatter_tokens("tools")
        .iter()
        .filter_map(|t| t.strip_prefix("mcp__"))
        .map(|rest| rest.split("__").next().unwrap_or(rest).to_string())
        .collect()
}

/// Every name a `Skill(skill="…")` call in the agents or instructions loads,
/// plus the Architect's seeded skills.
fn skills_the_prompts_load() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let files = md_files(&content("agents"))
        .into_iter()
        .chain(md_files(&content("instructions")));
    for path in files {
        let text = std::fs::read_to_string(&path).expect("read prompt file");
        for rest in text.split("Skill(skill=").skip(1) {
            let rest = rest.trim_start_matches(['\\', '"']);
            let name: String = rest
                .chars()
                .take_while(|c| !matches!(c, '"' | '\\'))
                .collect();
            if !name.is_empty() && !name.starts_with('<') {
                names.insert(name);
            }
        }
    }
    let seeds = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/assets/architect/skills");
    for path in md_files(&seeds) {
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            names.insert(stem.to_string());
        }
    }
    names
}

/// Whether `word` occurs in `text` with no identifier character on either side.
fn has_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        let ident = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        !ident(before) && !ident(after)
    })
}

#[test]
fn floor_holds_the_builtins_and_chrome() {
    for name in [
        "trusty-memory",
        "trusty-mpm",
        "trusty-review",
        "trusty-search",
        "claude-in-chrome",
    ] {
        assert!(is_floor(name), "{name} must be in the floor");
    }
    assert!(!is_floor("heygen"));
    assert!(!is_floor("slack-mcp"));
}

/// #8988 Q2 drift guard: a new agent MCP grant must join the floor, or an
/// owner `deny` could break that agent.
#[test]
fn floor_covers_every_agent_tools_mcp_grant() {
    let granted = agent_tools_mcp_servers();
    assert!(
        granted.contains("trusty-search"),
        "the scan found no grants — the parser or the path is wrong: {granted:?}"
    );
    let missing: Vec<&String> = granted.iter().filter(|s| !is_floor(s)).collect();
    assert!(
        missing.is_empty(),
        "agent tools: lines grant MCP servers outside the never-deny floor: {missing:?}"
    );
}

#[test]
fn the_tool_floor_holds_builtins_and_floor_servers() {
    for rule in [
        "Agent",
        "bash",
        "CronList",
        "mcp__trusty-search",
        "mcp__Trusty-Search__search",
        "mcp__claude-in-chrome__navigate",
    ] {
        assert!(is_tool_floor(rule), "{rule} must be in the tool floor");
    }
    for rule in ["NotebookEdit", "Workflow", "mcp__heygen", "mcp__heygen__x"] {
        assert!(!is_tool_floor(rule), "{rule} must not be in the tool floor");
    }
}

#[test]
fn the_skill_floor_holds_the_prefix_and_the_exact_names() {
    for name in [
        "tm-workflow",
        "TM-Anything",
        "systematic-debugging",
        "Git-Workflow",
        "workflow-authoring",
    ] {
        assert!(is_skill_floor(name), "{name} must be in the skill floor");
    }
    for name in [
        "xlsx",
        "breeze:voice",
        "tmux-helper",
        "rust-runtime-performance",
    ] {
        assert!(
            !is_skill_floor(name),
            "{name} must not be in the skill floor"
        );
    }
}

/// #8992 drift guard: every built-in an agent `tools:` line grants is floored,
/// so an owner `[tools] deny` can never break that agent.
#[test]
fn tool_floor_covers_every_agent_tools_builtin_grant() {
    let granted: BTreeSet<String> = agent_frontmatter_tokens("tools")
        .into_iter()
        .filter(|t| !t.starts_with("mcp__"))
        .collect();
    assert!(
        granted.contains("Bash") && granted.contains("Read"),
        "the scan found no built-in grants — the parser or the path is wrong: {granted:?}"
    );
    let missing: Vec<&String> = granted.iter().filter(|t| !is_tool_floor(t)).collect();
    assert!(
        missing.is_empty(),
        "agent tools: lines grant built-ins outside TOOL_FLOOR: {missing:?}"
    );
}

/// #8992 drift guard, reverse direction: every hand-listed floor tool is used
/// by an agent, a prompt, a skill or the PM tool trim, so none is decorative.
#[test]
fn every_tool_floor_name_is_used_by_a_source() {
    let mut corpus = String::from(include_str!("../pm_tool_trim.rs"));
    for sub in ["agents", "instructions", "skills"] {
        for path in md_files(&content(sub)) {
            corpus.push_str(&std::fs::read_to_string(&path).expect("read content file"));
        }
    }
    let unused: Vec<&&str> = TOOL_FLOOR
        .iter()
        .filter(|t| !has_word(&corpus, t))
        .collect();
    assert!(
        unused.is_empty(),
        "TOOL_FLOOR names no source uses — a rename left them stale: {unused:?}"
    );
}

/// #8992 drift guard: every skill an agent preloads is floored.
#[test]
fn skill_floor_covers_every_agent_skills_grant() {
    let granted = agent_frontmatter_tokens("skills");
    assert!(
        granted.contains("systematic-debugging"),
        "the scan found no skills grants — the parser or the path is wrong: {granted:?}"
    );
    let missing: Vec<&String> = granted.iter().filter(|s| !is_skill_floor(s)).collect();
    assert!(
        missing.is_empty(),
        "agent skills: lines preload skills outside the skill floor: {missing:?}"
    );
}

/// #8992 drift guard: every skill a prompt loads by name, and every skill the
/// Architect is seeded with, is floored.
#[test]
fn skill_floor_covers_every_skill_the_prompts_load() {
    let loaded = skills_the_prompts_load();
    assert!(
        loaded.contains("tm-workflow") && loaded.contains("tm-fleet-check"),
        "the scan found no Skill() loads — the parser or the path is wrong: {loaded:?}"
    );
    let missing: Vec<&String> = loaded.iter().filter(|s| !is_skill_floor(s)).collect();
    assert!(
        missing.is_empty(),
        "prompts load skills outside the skill floor: {missing:?}"
    );
}

/// #8992 drift guard, reverse direction: every exact floor skill is still
/// preloaded by an agent or loaded by a prompt, so a rename cannot leave a
/// stale entry behind.
#[test]
fn every_exact_skill_floor_name_has_a_source() {
    let sources: BTreeSet<String> = agent_frontmatter_tokens("skills")
        .into_iter()
        .chain(skills_the_prompts_load())
        .collect();
    let stale: Vec<&&str> = SKILL_FLOOR_EXACT
        .iter()
        .filter(|s| !sources.contains(**s))
        .collect();
    assert!(
        stale.is_empty(),
        "SKILL_FLOOR_EXACT names no agent or prompt uses: {stale:?}"
    );
}
