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

use super::frontmatter::{parse_kv_line, parse_list_value};

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
/// `pub(crate)` so [`crate::core::agent_metadata`] can project it into the
/// public, display-oriented [`crate::core::agent_metadata::AgentMetadata`]
/// without duplicating the frontmatter grammar.
/// Test: exercised indirectly by every `compose_*` test.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Frontmatter {
    pub(crate) name: Option<String>,
    pub(crate) role: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) extends: Option<String>,
    /// Auto-submitted first turn when the agent is spawned (claude-mpm parity).
    pub(crate) initial_prompt: Option<String>,
    /// Resource tier (`intensive`/`high`/`standard`/`lightweight`) used to
    /// derive a default `model` when none is explicitly set.
    pub(crate) resource_tier: Option<String>,
    /// Declared skill dependencies (DOC-42 §SPEC-AGENTSKILLS-01, issue #2889).
    ///
    /// Why: agents may declare which skills they depend on so co-deployment
    /// (§SPEC-AGENTSKILLS-02) and diagnostics (§SPEC-AGENTSKILLS-03) can act
    /// on the declaration instead of relying on unenforceable prose. Absent
    /// `skills:` parses to an empty `Vec` — exactly today's behavior.
    /// What: the raw skill names in source order, before de-duplication
    /// (deduplication happens in [`merge_frontmatter`] across the chain).
    pub(crate) skills: Vec<String>,
}

/// Resolve the default `model` for a `resource_tier` (HR-1 deploy enrichment).
///
/// Why: external/source agents may declare a `resource_tier` without an explicit
/// `model`; the harness must still deploy them with a correct model assignment.
/// The mapping mirrors claude-mpm's `RESOURCE_TIER_DEFAULTS` so a harness
/// provisioned from either toolchain behaves identically.
/// What: maps `intensive → opus`, `high/standard → sonnet`,
/// `lightweight → haiku`; any missing or unrecognised tier falls back to the
/// `standard` default (`sonnet`), per the HR-1 error contract. The bare names
/// `opus`/`sonnet`/`haiku` are the harness's accepted short model identifiers
/// (the same short forms the rest of trusty-mpm uses to name models).
/// Test: `tier_model_mapping`, `tier_unknown_falls_back_to_sonnet` in
/// agent_builder_tests.rs.
fn tier_to_model(resource_tier: Option<&str>) -> &'static str {
    match resource_tier {
        Some("intensive") => "opus",
        Some("lightweight") => "haiku",
        // `high`, `standard`, missing, and any unrecognised tier → sonnet.
        _ => "sonnet",
    }
}

/// Resolve the default `initialPrompt` for an agent `role` (HR-1 enrichment).
///
/// Why: claude-mpm injects a self-starting first turn per agent type so a
/// delegated agent begins work without an extra round-trip. trusty-mpm uses the
/// `role` field as the type discriminator. Interactive/special-purpose agents
/// are intentionally excluded so they wait for human or PM direction.
/// What: returns a per-role default prompt, or `None` when the role is excluded
/// or unknown (an unknown role simply gets no injected prompt).
/// Test: `initial_prompt_injected_by_role`, `initial_prompt_excluded_role` in
/// agent_builder_tests.rs.
fn default_initial_prompt(role: Option<&str>) -> Option<&'static str> {
    match role {
        Some("engineer") | Some("data-engineer") => {
            Some("Begin implementation. Read the task context and start coding immediately.")
        }
        Some("qa") => {
            Some("Begin verification. Read the task context and start testing immediately.")
        }
        Some("ops") | Some("version-control") => {
            Some("Begin operations. Read the task context and execute immediately.")
        }
        Some("research") | Some("code-analyzer") => Some(
            "Begin investigation. Analyze the task context, search for relevant files, \
             and report findings.",
        ),
        Some("documentation") => {
            Some("Begin documentation. Read the task context and start writing immediately.")
        }
        Some("security") => {
            Some("Begin security analysis. Read the task context and start scanning immediately.")
        }
        // Interactive / special-purpose roles (base, ticketing, prompt-engineer,
        // memory-manager, agent/skill managers) and unknown roles get none.
        _ => None,
    }
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
///
/// ## Key-case convention (in-lower / out-camel)
///
/// [`parse_kv_line`] lower-cases every key, so `initialPrompt` is read back as
/// `initialprompt` here. [`merge_frontmatter`] deliberately *emits* the
/// camelCase `initialPrompt` (the form Claude Code expects). The asymmetry is
/// intentional and round-trip-safe: feeding a composed file back through
/// `compose_agent` re-lower-cases the emitted `initialPrompt` to `initialprompt`
/// and lands on this same struct field, so a compose→deploy→re-compose cycle is
/// stable. The escaped scalar value is decoded via
/// [`unescape_yaml_double_quoted`] so the original prompt text (including any
/// embedded `"`/`\`) survives the cycle verbatim.
pub(crate) fn split_frontmatter(raw: &str) -> Result<(Frontmatter, String), AgentBuildError> {
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
        // `parse_kv_line` lower-cases keys, so `initialPrompt` arrives as
        // `initialprompt`; match that normalised form. Decode the YAML
        // double-quote escapes (`parse_kv_line` already stripped the single
        // outer quote pair) so a re-composed prompt round-trips verbatim.
        initial_prompt: fields
            .remove("initialprompt")
            .map(|v| unescape_yaml_double_quoted(&v)),
        resource_tier: fields.remove("resource_tier"),
        skills: fields
            .remove("skills")
            .map(|v| parse_list_value(&v))
            .unwrap_or_default(),
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

/// Escape a string for emission inside a YAML double-quoted scalar.
///
/// Why: `merge_frontmatter` wraps the `initialPrompt` value in double-quotes.
/// A raw `"` or `\` in the value would otherwise terminate the quote early or
/// be misread as an escape, producing malformed YAML. claude-mpm parity
/// requires the emitted frontmatter always be parseable.
/// What: replaces `\` with `\\` and `"` with `\"` (backslash first, so the
/// quote-escape's own backslash is not doubled). The exact inverse of
/// [`unescape_yaml_double_quoted`].
/// Test: `initial_prompt_with_embedded_quote_round_trips`,
/// `initial_prompt_with_backslash_round_trips` in agent_builder_tests.rs.
fn escape_yaml_double_quoted(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Reverse [`escape_yaml_double_quoted`] on a parsed scalar value.
///
/// Why: a value parsed back from emitted frontmatter still carries the `\"`
/// and `\\` escapes after [`parse_kv_line`] strips the single outer quote pair.
/// Decoding them here makes a compose→deploy→re-compose round-trip yield the
/// original `initialPrompt` string unchanged.
/// What: collapses `\\` → `\` and `\"` → `"`; any other `\x` sequence (and a
/// trailing lone `\`) is left verbatim so non-escaped backslashes survive.
/// Test: `initial_prompt_with_embedded_quote_round_trips`,
/// `initial_prompt_with_backslash_round_trips` in agent_builder_tests.rs.
fn unescape_yaml_double_quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Render a single merged frontmatter block from a resolved chain.
///
/// Why: the composed file needs exactly one frontmatter block; child fields
/// must win over base fields so a concrete agent's `model`/`description`
/// survive. Deploy-time enrichments (HR-1) are applied here so every composed
/// agent carries a tier-derived `model` default and a self-starting
/// `initialPrompt` without editing each source file.
/// What: folds the chain base-first, overlaying each file's set fields, then
/// (a) fills `model` from `resource_tier` when no explicit `model` was set
/// (explicit always wins), and (b) fills `initialPrompt` from the agent `role`
/// when the source declared none and the role is not excluded. `skills:`
/// (DOC-42, issue #2889) is a UNION across the chain rather than an override —
/// a base agent's declared skills and a child's additions both survive,
/// de-duplicated in first-seen (base-first) order — because a list of
/// dependencies naturally accumulates through inheritance the way body text
/// concatenates, unlike a scalar field where the child's value should replace
/// the parent's. Emits the populated keys in a stable order.
/// Test: `compose_engineer_chain` (merge), `tier_model_mapping`,
/// `explicit_model_wins_over_tier`, `initial_prompt_injected_by_role`,
/// `initial_prompt_explicit_wins`, `skills_union_across_chain`,
/// `skills_deduplicated_across_chain` in agent_builder_tests.rs.
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
        if fm.initial_prompt.is_some() {
            merged.initial_prompt = fm.initial_prompt.clone();
        }
        if fm.resource_tier.is_some() {
            merged.resource_tier = fm.resource_tier.clone();
        }
        // DOC-42: union, not override — de-duplicated, first-seen (base-first)
        // order, so a child agent's additions never drop a base's declared
        // skills.
        for skill in &fm.skills {
            if !merged.skills.contains(skill) {
                merged.skills.push(skill.clone());
            }
        }
    }

    // HR-1 Part C: derive `model` from `resource_tier` only when no explicit
    // `model` survived the merge. Explicit model always wins.
    if merged.model.is_none() && merged.resource_tier.is_some() {
        merged.model = Some(tier_to_model(merged.resource_tier.as_deref()).to_string());
    }

    // HR-1 Part B: inject a role-derived `initialPrompt` when the source set
    // none. A source-declared `initialPrompt` (now in `merged`) always wins.
    if merged.initial_prompt.is_none()
        && let Some(prompt) = default_initial_prompt(merged.role.as_deref())
    {
        merged.initial_prompt = Some(prompt.to_string());
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
    if let Some(v) = &merged.resource_tier {
        out.push_str(&format!("resource_tier: {v}\n"));
    }
    if !merged.skills.is_empty() {
        // DOC-42: emit the flattened, unioned skill list so a deployed agent
        // file is self-describing — `tm doctor` / `tm agent show` read it
        // straight off the deployed `.md` without re-walking `extends:`.
        out.push_str(&format!("skills: [{}]\n", merged.skills.join(", ")));
    }
    if let Some(v) = &merged.initial_prompt {
        // Emit a YAML double-quoted scalar. The value is escaped so a `"` or
        // `\` inside the prompt cannot break out of the quotes and produce
        // malformed YAML. `escape_yaml_double_quoted` is the exact inverse of
        // the `unescape_yaml_double_quoted` decode applied in
        // `split_frontmatter`, so a compose→deploy→re-compose round-trip is
        // stable (see the in-lower/out-camel note on `split_frontmatter`).
        out.push_str(&format!(
            "initialPrompt: \"{}\"\n",
            escape_yaml_double_quoted(v)
        ));
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
