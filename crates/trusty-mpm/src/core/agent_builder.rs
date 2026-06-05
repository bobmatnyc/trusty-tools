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
//!
//! ## Case-insensitive source resolution
//!
//! The base template files are named `BASE-QA.md`, `BASE-ENGINEER.md`, etc.
//! (UPPERCASE stems), while concrete agents declare `extends: base-qa` /
//! `extends: base-engineer` (lowercase). On macOS (case-insensitive HFS+)
//! the filesystem transparently matches these; on Linux (case-sensitive
//! ext4/etc.) it does not. To remain platform-independent, [`build_source_map`]
//! scans the source directory once and builds a `HashMap` keyed by the
//! lowercased stem. All resolution then goes through this map rather than
//! constructing a raw path from the `extends:` value.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use super::frontmatter::parse_kv_line;

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

/// A case-folded index of `*.md` files in a source directory.
///
/// Why: base template files use UPPERCASE stems (`BASE-QA.md`) while
/// `extends:` values use lowercase (`base-qa`). A filesystem lookup of
/// the literal extends-value fails on case-sensitive filesystems (Linux).
/// Keying by lowercased stem lets the resolver find `BASE-QA.md` when
/// asked for `base-qa` without relying on OS case-folding behaviour.
/// What: maps `lowercased_stem -> full_path` for every `*.md` entry in
/// the directory. Non-`.md` entries and subdirectories are ignored.
/// Test: `case_insensitive_resolve_via_map` in agent_builder_tests.rs.
pub type SourceMap = HashMap<String, PathBuf>;

/// Build a [`SourceMap`] by scanning `source_dir` for `*.md` files.
///
/// Why: centralises directory scanning so `compose_agent` and `source_chain`
/// each scan the directory exactly once rather than once per resolve step.
/// What: reads directory entries, filters to `.md` files whose stem is valid
/// UTF-8, inserts `(lowercase_stem, path)` pairs. If the directory cannot be
/// read the map is returned empty — callers surface the missing-file error on
/// first lookup.
/// Test: exercised implicitly by every compose/chain test via `compose_agent`.
pub fn build_source_map(source_dir: &Path) -> SourceMap {
    let mut map = SourceMap::new();
    let entries = match std::fs::read_dir(source_dir) {
        Ok(e) => e,
        Err(_) => return map,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            map.insert(stem.to_lowercase(), path);
        }
    }
    map
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
        // Use the shared parser so colon-containing values (URLs, timestamps,
        // model ids) are preserved rather than truncated or hard-errored on.
        match parse_kv_line(line) {
            None if line.trim().is_empty() => continue,
            None => {
                return Err(AgentBuildError::FrontmatterParse(format!(
                    "expected `key: value`, got `{line}`"
                )));
            }
            Some((key, value)) => {
                fields.insert(key, value);
            }
        }
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
/// chain must be walked to its root before bodies are joined. Accepts a
/// pre-built [`SourceMap`] so lookups are case-insensitive on all platforms
/// (Linux ext4 and macOS HFS+ behave identically).
/// What: looks up `name` (lowercased) in `sources`, reads the matched file,
/// parses its frontmatter, recurses into `extends` while tracking the visited
/// path for cycle detection and enforcing the depth limit, then returns
/// `(frontmatter chain, body chain)` ordered base-first.
/// Test: `cycle_detection`, `depth_exceeded`,
///       `case_insensitive_resolve_via_map`.
fn resolve(
    name: &str,
    sources: &SourceMap,
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

    // Look up via lowercased key so `base-qa` matches `BASE-QA.md` on Linux.
    let path = sources
        .get(&name.to_lowercase())
        .ok_or_else(|| AgentBuildError::NotFound(name.to_string()))?;

    let raw = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(AgentBuildError::NotFound(name.to_string()));
        }
        Err(err) => return Err(AgentBuildError::Io(err)),
    };

    let (fm, body) = split_frontmatter(&raw)?;

    visiting.push(name.to_string());
    let (mut frontmatters, mut bodies) = match &fm.extends {
        Some(parent) => resolve(parent, sources, visiting)?,
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
/// build time so CC sees only self-contained flat files. Uses a pre-built
/// case-folded [`SourceMap`] so `extends: base-qa` finds `BASE-QA.md` on
/// case-sensitive (Linux) filesystems without any special OS behaviour.
///
/// What: scans `source_dir` once into a [`SourceMap`], then reads source
/// files, parses `extends:` frontmatter, concatenates base-first, returns
/// composed Markdown with a single merged frontmatter block at the top.
///
/// Test: `compose_engineer_chain`, `compose_base_only`, `cycle_detection`,
///       `case_insensitive_resolve_via_map`
pub fn compose_agent(name: &str, source_dir: &Path) -> Result<String, AgentBuildError> {
    let sources = build_source_map(source_dir);
    let mut visiting = Vec::new();
    let (frontmatters, bodies) = resolve(name, &sources, &mut visiting)?;

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
/// it to the operator (`base-agent -> base-engineer -> engineer`). Uses a
/// pre-built [`SourceMap`] for case-insensitive lookup on all platforms.
/// What: scans `source_dir` once into a [`SourceMap`], then walks `extends`
/// exactly like [`compose_agent`] but returns only the agent names, base-first.
/// Test: `source_chain_engineer`, `source_chain_base_only`.
pub fn source_chain(name: &str, source_dir: &Path) -> Result<Vec<String>, AgentBuildError> {
    fn walk(
        name: &str,
        sources: &SourceMap,
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
        let path = sources
            .get(&name.to_lowercase())
            .ok_or_else(|| AgentBuildError::NotFound(name.to_string()))?;
        let raw = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(AgentBuildError::NotFound(name.to_string()));
            }
            Err(err) => return Err(AgentBuildError::Io(err)),
        };
        let (fm, _) = split_frontmatter(&raw)?;
        visiting.push(name.to_string());
        if let Some(parent) = &fm.extends {
            walk(parent, sources, visiting, out)?;
        }
        visiting.pop();
        out.push(name.to_string());
        Ok(())
    }

    let sources = build_source_map(source_dir);
    let mut chain = Vec::new();
    let mut visiting = Vec::new();
    walk(name, &sources, &mut visiting, &mut chain)?;
    Ok(chain)
}

#[cfg(test)]
#[path = "agent_builder_tests.rs"]
mod tests;
