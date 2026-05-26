//! Agent inheritance resolution — the `compose` half of the build pipeline.
//!
//! Why: Claude Code has no native concept of agent inheritance. trusty-mpm
//! source agents declare `extends:` in their YAML frontmatter; to make this
//! work, the inheritance chain must be flattened at build time into a single
//! self-contained file before Claude Code ever sees it.
//! What: [`compose_agent`] loads a source `.md` file, walks its `extends`
//! chain base-first, strips intermediate frontmatter, and returns one composed
//! Markdown document with a single merged frontmatter block on top.
//! Test: `cargo test -p trusty-mpm-core agent_builder` covers a base-only
//! agent, a three-deep chain, cycle detection, and the depth limit.

use std::collections::HashMap;
use std::fmt;
use std::path::Path;

/// Maximum inheritance-chain depth before [`compose_agent`] gives up.
///
/// Why: a malformed framework could declare an unbounded `extends` chain;
/// the limit converts that into a clear error instead of a stack overflow.
const MAX_DEPTH: usize = 8;

/// A failure raised while composing an agent inheritance chain.
///
/// Why: the build pipeline needs a typed failure surface so callers can
/// distinguish "source file missing" from "frontmatter malformed" from
/// "the framework declares a cycle".
/// What: covers missing files, frontmatter parse faults, inheritance cycles,
/// exceeded depth limits, and underlying IO errors.
/// Test: `cycle_detection`, `depth_exceeded`, `compose_missing_agent`.
#[derive(Debug)]
pub enum AgentBuildError {
    /// A required `<name>.md` source file was not found.
    NotFound(String),
    /// A frontmatter block could not be parsed.
    FrontmatterParse(String),
    /// The `extends` chain forms a cycle; the payload is the offending chain.
    Cycle(Vec<String>),
    /// The `extends` chain exceeded [`MAX_DEPTH`].
    DepthExceeded(usize),
    /// An underlying filesystem operation failed.
    Io(std::io::Error),
}

impl fmt::Display for AgentBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(name) => write!(f, "agent source not found: {name}"),
            Self::FrontmatterParse(msg) => write!(f, "frontmatter parse error: {msg}"),
            Self::Cycle(chain) => {
                write!(f, "inheritance cycle detected: {}", chain.join(" -> "))
            }
            Self::DepthExceeded(depth) => {
                write!(f, "inheritance chain exceeded depth limit of {depth}")
            }
            Self::Io(err) => write!(f, "io error: {err}"),
        }
    }
}

impl std::error::Error for AgentBuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for AgentBuildError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

/// The parsed frontmatter fields trusty-mpm cares about for an agent.
///
/// Why: agent composition only needs a handful of YAML keys; a small struct
/// avoids pulling in a full YAML library and keeps merging explicit.
/// What: the `extends` parent (if any) plus the passthrough display fields.
/// Test: exercised indirectly by every `compose_*` test.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Frontmatter {
    name: Option<String>,
    role: Option<String>,
    description: Option<String>,
    model: Option<String>,
    extends: Option<String>,
}

/// Split a source document into its frontmatter map and body.
///
/// Why: composition strips intermediate frontmatter and re-emits one merged
/// block, so each file must be cleanly separated into metadata and content.
/// What: when the document opens with a `---` line, reads `key: value` pairs
/// until the closing `---`; everything after is the body. With no frontmatter
/// the whole document is the body.
/// Test: `compose_base_only` (frontmatter present), bodies verified in
/// `compose_engineer_chain`.
fn split_frontmatter(raw: &str) -> Result<(Frontmatter, String), AgentBuildError> {
    let trimmed_start = raw.trim_start_matches(['\u{feff}']);
    let mut lines = trimmed_start.lines();

    // A frontmatter block must be the very first non-empty content.
    match lines.next() {
        Some(first) if first.trim() == "---" => {}
        _ => return Ok((Frontmatter::default(), raw.to_string())),
    }

    let mut fields: HashMap<String, String> = HashMap::new();
    let mut closed = false;
    let mut consumed = first_line_len(trimmed_start);

    for line in lines {
        consumed += line.len() + 1; // +1 for the newline `lines()` strips.
        if line.trim() == "---" {
            closed = true;
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            return Err(AgentBuildError::FrontmatterParse(format!(
                "expected `key: value`, got `{line}`"
            )));
        };
        fields.insert(
            key.trim().to_ascii_lowercase(),
            value.trim().trim_matches(['"', '\'']).to_string(),
        );
    }

    if !closed {
        return Err(AgentBuildError::FrontmatterParse(
            "unterminated frontmatter block (missing closing `---`)".to_string(),
        ));
    }

    let body = trimmed_start
        .get(consumed.min(trimmed_start.len())..)
        .unwrap_or("")
        .trim_start_matches('\n')
        .to_string();

    let fm = Frontmatter {
        name: fields.remove("name"),
        role: fields.remove("role"),
        description: fields.remove("description"),
        model: fields.remove("model"),
        extends: fields.remove("extends"),
    };
    Ok((fm, body))
}

/// Byte length of the first line plus its trailing newline.
///
/// Why: `split_frontmatter` tracks how many bytes the frontmatter consumed so
/// it can slice the body; the opening `---` line must be counted exactly.
/// What: returns the first line's length + 1, or the whole string length if
/// there is no newline.
/// Test: covered indirectly by `compose_base_only`.
fn first_line_len(s: &str) -> usize {
    match s.find('\n') {
        Some(idx) => idx + 1,
        None => s.len(),
    }
}

/// Render a single merged frontmatter block from a resolved chain.
///
/// Why: the composed file needs exactly one frontmatter block; child fields
/// must win over base fields so a concrete agent's `model`/`description`
/// survive.
/// What: folds the chain base-first, overlaying each file's set fields, and
/// emits the populated keys in a stable order.
/// Test: `compose_engineer_chain` asserts the merged `name`/`model`.
fn merge_frontmatter(chain: &[Frontmatter]) -> String {
    let mut merged = Frontmatter::default();
    for fm in chain {
        if fm.name.is_some() {
            merged.name = fm.name.clone();
        }
        if fm.role.is_some() {
            merged.role = fm.role.clone();
        }
        if fm.description.is_some() {
            merged.description = fm.description.clone();
        }
        if fm.model.is_some() {
            merged.model = fm.model.clone();
        }
    }

    let mut out = String::from("---\n");
    if let Some(v) = &merged.name {
        out.push_str(&format!("name: {v}\n"));
    }
    if let Some(v) = &merged.role {
        out.push_str(&format!("role: {v}\n"));
    }
    if let Some(v) = &merged.description {
        out.push_str(&format!("description: {v}\n"));
    }
    if let Some(v) = &merged.model {
        out.push_str(&format!("model: {v}\n"));
    }
    out.push_str("---\n");
    out
}

/// Recursively resolve an agent and its ancestors, base-first.
///
/// Why: composition concatenates parent content before child content, so the
/// chain must be walked to its root before bodies are joined.
/// What: loads `<name>.md`, parses its frontmatter, recurses into `extends`
/// while tracking the visited path for cycle detection and enforcing the depth
/// limit, then returns `(frontmatter chain, body chain)` ordered base-first.
/// Test: `cycle_detection`, `depth_exceeded`.
fn resolve(
    name: &str,
    source_dir: &Path,
    visiting: &mut Vec<String>,
) -> Result<(Vec<Frontmatter>, Vec<String>), AgentBuildError> {
    if visiting.len() >= MAX_DEPTH {
        return Err(AgentBuildError::DepthExceeded(MAX_DEPTH));
    }
    if visiting.iter().any(|n| n == name) {
        let mut cycle = visiting.clone();
        cycle.push(name.to_string());
        return Err(AgentBuildError::Cycle(cycle));
    }

    let path = source_dir.join(format!("{name}.md"));
    let raw = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(AgentBuildError::NotFound(name.to_string()));
        }
        Err(err) => return Err(AgentBuildError::Io(err)),
    };

    let (fm, body) = split_frontmatter(&raw)?;

    visiting.push(name.to_string());
    let (mut frontmatters, mut bodies) = match &fm.extends {
        Some(parent) => resolve(parent, source_dir, visiting)?,
        None => (Vec::new(), Vec::new()),
    };
    visiting.pop();

    frontmatters.push(fm);
    bodies.push(body);
    Ok((frontmatters, bodies))
}

/// Resolves an inheritance chain and returns the composed content.
///
/// Why: Claude Code has no native agent inheritance; we resolve chains at
/// build time so CC sees only self-contained flat files.
///
/// What: reads source files from `source_dir`, parses `extends:` frontmatter,
/// concatenates base-first, returns composed Markdown with a single merged
/// frontmatter block at the top.
///
/// Test: `compose_engineer_chain`, `compose_base_only`, `cycle_detection`
pub fn compose_agent(name: &str, source_dir: &Path) -> Result<String, AgentBuildError> {
    let mut visiting = Vec::new();
    let (frontmatters, bodies) = resolve(name, source_dir, &mut visiting)?;

    let mut out = merge_frontmatter(&frontmatters);
    out.push('\n');
    let joined: Vec<String> = bodies
        .iter()
        .map(|b| b.trim_matches('\n').to_string())
        .filter(|b| !b.is_empty())
        .collect();
    out.push_str(&joined.join("\n\n"));
    out.push('\n');
    Ok(out)
}

/// The ordered inheritance chain (base-first) for an agent, by name.
///
/// Why: the deploy step records the resolved chain in the manifest and prints
/// it to the operator (`base-agent -> base-engineer -> engineer`).
/// What: walks `extends` exactly like [`compose_agent`] but returns only the
/// agent names, base-first.
/// Test: `source_chain_engineer`, `source_chain_base_only`.
pub fn source_chain(name: &str, source_dir: &Path) -> Result<Vec<String>, AgentBuildError> {
    fn walk(
        name: &str,
        source_dir: &Path,
        visiting: &mut Vec<String>,
        out: &mut Vec<String>,
    ) -> Result<(), AgentBuildError> {
        if visiting.len() >= MAX_DEPTH {
            return Err(AgentBuildError::DepthExceeded(MAX_DEPTH));
        }
        if visiting.iter().any(|n| n == name) {
            let mut cycle = visiting.clone();
            cycle.push(name.to_string());
            return Err(AgentBuildError::Cycle(cycle));
        }
        let path = source_dir.join(format!("{name}.md"));
        let raw = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(AgentBuildError::NotFound(name.to_string()));
            }
            Err(err) => return Err(AgentBuildError::Io(err)),
        };
        let (fm, _) = split_frontmatter(&raw)?;
        visiting.push(name.to_string());
        if let Some(parent) = &fm.extends {
            walk(parent, source_dir, visiting, out)?;
        }
        visiting.pop();
        out.push(name.to_string());
        Ok(())
    }

    let mut chain = Vec::new();
    let mut visiting = Vec::new();
    walk(name, source_dir, &mut visiting, &mut chain)?;
    Ok(chain)
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
    fn compose_base_only() {
        // An agent with no `extends` returns its own body under a merged
        // frontmatter block — no inheritance to resolve.
        let tmp = TempDir::new().unwrap();
        write_agent(
            tmp.path(),
            "base-agent",
            "---\nname: base-agent\nrole: base\n---\n\n# Base\n\nFoundation content.\n",
        );
        let composed = compose_agent("base-agent", tmp.path()).unwrap();
        assert!(composed.starts_with("---\n"));
        assert!(composed.contains("name: base-agent"));
        assert!(composed.contains("role: base"));
        assert!(composed.contains("Foundation content."));
        // `extends` must never leak into the composed frontmatter.
        assert!(!composed.contains("extends:"));
    }

    #[test]
    fn compose_engineer_chain() {
        // engineer -> base-engineer -> base-agent must concatenate bodies
        // base-first and merge frontmatter child-wins.
        let tmp = TempDir::new().unwrap();
        write_agent(
            tmp.path(),
            "base-agent",
            "---\nname: base-agent\nrole: base\n---\n\n# Base\n\nBASE BODY\n",
        );
        write_agent(
            tmp.path(),
            "base-engineer",
            "---\nname: base-engineer\nrole: base-engineer\nextends: base-agent\n---\n\n# Base Engineer\n\nENGINEER BASE BODY\n",
        );
        write_agent(
            tmp.path(),
            "engineer",
            "---\nname: engineer\nrole: engineer\nextends: base-engineer\nmodel: sonnet\n---\n\n# Engineer\n\nLEAF BODY\n",
        );
        let composed = compose_agent("engineer", tmp.path()).unwrap();

        // Child fields win in the merged frontmatter.
        assert!(composed.contains("name: engineer"));
        assert!(composed.contains("role: engineer"));
        assert!(composed.contains("model: sonnet"));

        // Bodies appear base-first.
        let base = composed.find("BASE BODY").expect("base body present");
        let mid = composed
            .find("ENGINEER BASE BODY")
            .expect("base-engineer body present");
        let leaf = composed.find("LEAF BODY").expect("leaf body present");
        assert!(base < mid, "base body must precede base-engineer body");
        assert!(mid < leaf, "base-engineer body must precede leaf body");
    }

    #[test]
    fn cycle_detection() {
        // A extends B, B extends A -> the chain forms a cycle.
        let tmp = TempDir::new().unwrap();
        write_agent(
            tmp.path(),
            "agent-a",
            "---\nname: agent-a\nextends: agent-b\n---\n\nA body\n",
        );
        write_agent(
            tmp.path(),
            "agent-b",
            "---\nname: agent-b\nextends: agent-a\n---\n\nB body\n",
        );
        let err = compose_agent("agent-a", tmp.path()).unwrap_err();
        match err {
            AgentBuildError::Cycle(chain) => {
                assert!(chain.contains(&"agent-a".to_string()));
                assert!(chain.contains(&"agent-b".to_string()));
            }
            other => panic!("expected Cycle, got {other:?}"),
        }
    }

    #[test]
    fn depth_exceeded() {
        // A chain longer than MAX_DEPTH must fail with DepthExceeded.
        let tmp = TempDir::new().unwrap();
        // Build level0 (root) .. level10 each extending the previous.
        write_agent(tmp.path(), "level0", "---\nname: level0\n---\n\nroot\n");
        for i in 1..=10 {
            write_agent(
                tmp.path(),
                &format!("level{i}"),
                &format!(
                    "---\nname: level{i}\nextends: level{}\n---\n\nbody{i}\n",
                    i - 1
                ),
            );
        }
        let err = compose_agent("level10", tmp.path()).unwrap_err();
        assert!(
            matches!(err, AgentBuildError::DepthExceeded(MAX_DEPTH)),
            "expected DepthExceeded, got {err:?}"
        );
    }

    #[test]
    fn compose_missing_agent() {
        // A request for a non-existent source file must surface NotFound.
        let tmp = TempDir::new().unwrap();
        let err = compose_agent("ghost", tmp.path()).unwrap_err();
        assert!(matches!(err, AgentBuildError::NotFound(name) if name == "ghost"));
    }

    #[test]
    fn missing_parent_is_not_found() {
        // A child extending an absent parent must report the parent missing.
        let tmp = TempDir::new().unwrap();
        write_agent(
            tmp.path(),
            "child",
            "---\nname: child\nextends: nowhere\n---\n\nbody\n",
        );
        let err = compose_agent("child", tmp.path()).unwrap_err();
        assert!(matches!(err, AgentBuildError::NotFound(name) if name == "nowhere"));
    }

    #[test]
    fn unterminated_frontmatter_errors() {
        // A frontmatter block missing its closing `---` is a parse error.
        let tmp = TempDir::new().unwrap();
        write_agent(tmp.path(), "broken", "---\nname: broken\n\n# No close\n");
        let err = compose_agent("broken", tmp.path()).unwrap_err();
        assert!(matches!(err, AgentBuildError::FrontmatterParse(_)));
    }

    #[test]
    fn source_chain_engineer() {
        // The resolved chain must list ancestors base-first.
        let tmp = TempDir::new().unwrap();
        write_agent(
            tmp.path(),
            "base-agent",
            "---\nname: base-agent\n---\n\nb\n",
        );
        write_agent(
            tmp.path(),
            "base-engineer",
            "---\nname: base-engineer\nextends: base-agent\n---\n\nbe\n",
        );
        write_agent(
            tmp.path(),
            "engineer",
            "---\nname: engineer\nextends: base-engineer\n---\n\ne\n",
        );
        let chain = source_chain("engineer", tmp.path()).unwrap();
        assert_eq!(chain, vec!["base-agent", "base-engineer", "engineer"]);
    }

    #[test]
    fn source_chain_base_only() {
        // A base agent's chain is just itself.
        let tmp = TempDir::new().unwrap();
        write_agent(
            tmp.path(),
            "base-agent",
            "---\nname: base-agent\n---\n\nb\n",
        );
        let chain = source_chain("base-agent", tmp.path()).unwrap();
        assert_eq!(chain, vec!["base-agent"]);
    }
}
