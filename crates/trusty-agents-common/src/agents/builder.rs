//! Agent inheritance resolution — the `compose` half of the build pipeline.
//!
//! Why: Claude Code has no native concept of agent inheritance. trusty-mpm
//! (and, prospectively, trusty-code) source agents declare `extends:` in
//! their YAML frontmatter; to make this work, the inheritance chain must be
//! flattened at build time into a single self-contained file before Claude
//! Code ever sees it. Moved to trusty-agents-common (#2892) from
//! `trusty-mpm::core::agent_builder` so a second harness can reuse the same
//! composer instead of forking it; `trusty-mpm` re-exports every item here
//! from `core::agent_builder` for source compatibility.
//! What: [`compose_agent`] loads a source `.md` file, walks its `extends`
//! chain base-first, strips intermediate frontmatter, and returns one composed
//! Markdown document with a single merged frontmatter block on top. The chain
//! walk itself ([`resolve`]) is generic over the [`SourceLookup`] trait so a
//! second, disk-free lookup strategy can reuse it without duplicating the
//! walk/cycle/depth logic — see `agents::builder_in_memory` (issue #2958
//! Slice E1) for the embedded-asset implementation.
//! Test: `cargo test -p trusty-agents-common agents::builder` covers a
//! base-only agent, a three-deep chain, cycle detection, and the depth limit.
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
/// Test: `case_insensitive_resolve_via_map` in builder_tests.rs.
pub type SourceMap = HashMap<String, PathBuf>;

/// Abstraction over "given an `extends:`/agent name, produce its raw
/// markdown source" so [`resolve`] can walk an inheritance chain against
/// either a filesystem-backed [`SourceMap`] or an in-memory asset map
/// without duplicating the walk/cycle/depth logic.
///
/// Why: `compose_agent`'s fs implementation and `compose_agent_in_memory`'s
/// embedded-asset implementation (issue #2958 Slice E1,
/// `agents::builder_in_memory`) differ only in HOW a name resolves to raw
/// content — the chain-walking algorithm (cycle detection, depth limiting,
/// base-first ordering) is identical. Extracting that one seam into a trait
/// lets [`resolve`] be written once and shared, rather than forked.
/// What: `read_source` takes the ORIGINAL-case name (case-folding is the
/// implementor's responsibility, matching [`SourceMap`]'s existing
/// lowercased-stem convention) and returns the file's/asset's raw content, or
/// [`AgentBuildError::NotFound`] keyed on that original-case name so error
/// messages read naturally regardless of which backend resolved them.
/// `pub(crate)` — this is an internal composition seam, not a stable public
/// extension point; external callers use [`compose_agent`] or
/// `compose_agent_in_memory` instead of implementing the trait themselves.
/// Test: exercised indirectly by every `compose_agent`/`compose_agent_in_memory`
/// test via [`resolve`]; the fs impl is `case_insensitive_resolve_via_map`.
pub(crate) trait SourceLookup {
    fn read_source(&self, name: &str) -> Result<String, AgentBuildError>;
}

impl SourceLookup for SourceMap {
    fn read_source(&self, name: &str) -> Result<String, AgentBuildError> {
        // Look up via lowercased key so `base-qa` matches `BASE-QA.md` on
        // case-sensitive filesystems (Linux); macOS/HFS+ would already match
        // either way.
        let path = self
            .get(&name.to_lowercase())
            .ok_or_else(|| AgentBuildError::NotFound(name.to_string()))?;
        match std::fs::read_to_string(path) {
            Ok(content) => Ok(content),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                Err(AgentBuildError::NotFound(name.to_string()))
            }
            Err(err) => Err(AgentBuildError::Io(err)),
        }
    }
}

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
/// `pub(crate)` so [`crate::agents::metadata`] can project it into the
/// public, display-oriented [`crate::agents::metadata::AgentMetadata`]
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
    /// Maximum-output-tokens budget, mirroring tcode's TOML `AgentConfig`
    /// (issue #2897, epic #2892).
    ///
    /// Why: tcode's agent format is a superset of trusty-mpm's — this field
    /// carries no meaning for trusty-mpm (which never sets it), but making
    /// `Frontmatter` a superset lets both harnesses share one composer
    /// instead of forking it.
    /// What: parsed from a scalar `max_tokens:` key. Merges SCALAR CHILD-WINS
    /// across an `extends` chain, same as `model:`.
    pub(crate) max_tokens: Option<u32>,
    /// Allowed tool names, mirroring tcode's TOML `AgentConfig` (issue #2897,
    /// epic #2892).
    ///
    /// Why: unlike `skills:` (which accumulates across a chain, since a list
    /// of dependencies naturally grows through inheritance), `tools:` must
    /// let a restrictive leaf agent NARROW a permissive base's tool set — so
    /// it merges by OVERRIDE, not union (Bob's explicit decision, #2897). The
    /// field is `Option<Vec<String>>`, not a bare `Vec`, because "unset" and
    /// "explicitly set to zero tools" are semantically distinct and must
    /// stay distinguishable: `tools: []` is a deliberate deny-all override
    /// that must NOT collapse into the "inherit the parent" case an absent
    /// key produces (code-critic finding on PR #2952 — an `is_empty()` check
    /// cannot tell "omitted" from "empty list" apart, silently breaking
    /// deny-all). Mirrors tcode's `ToolsConfig.allowed: Option<Vec<String>>`
    /// (`None` = all tools allowed, `Some(vec![])` = none) so Slice B's
    /// projection maps cleanly.
    /// What: `None` when the source declares no `tools:` key at all. `Some`
    /// (populated via [`parse_list_value`], same grammar as `skills:`) when
    /// the key is present, including `Some(vec![])` for `tools: []`.
    pub(crate) tools: Option<Vec<String>>,
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
/// builder_tests.rs.
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
/// builder_tests.rs.
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

    // DOC-42 block-style `skills:` list support (issue #2906 review, CRITICAL
    // finding). `docs/specs/agent-bundled-skills.md` §1.1's motivating
    // example — and every realistic upstream `claude-mpm-agents` asset — uses
    // the YAML BLOCK form:
    //   skills:
    //     - code-review-standards
    //     - code-production-process
    // not the inline flow form `skills: [a, b]`. A bare `skills:` line has an
    // EMPTY value, so each following `- item` continuation line has no colon
    // at all — before this fix that unconditionally hit the generic
    // `expected key: value` hard error below, in violation of
    // §SPEC-AGENTSKILLS-01's "malformed → WARN and treat as empty, never
    // halt the load" contract. `in_skills_block` tracks whether the line
    // just parsed opened a bare `skills:` key (or a prior continuation line
    // is still being consumed); `skills_block`, once `Some`, wins over any
    // later inline `skills:` value (there cannot legally be both in one
    // document, but if there were, the block form — which is what this
    // parser actively consumed line-by-line — is the more specific match).
    let mut in_skills_block = false;
    let mut skills_block: Option<Vec<String>> = None;

    for line in lines {
        consumed += line.len() + 1; // +1 for the newline `lines()` strips.
        if line.trim() == "---" {
            closed = true;
            break;
        }

        if in_skills_block {
            let trimmed = line.trim_start();
            if let Some(item_text) = trimmed.strip_prefix('-') {
                // A block-sequence item under `skills:` — reuse
                // `parse_list_value` per-item so quoting/whitespace rules
                // stay identical to the inline flow-array form.
                skills_block
                    .get_or_insert_with(Vec::new)
                    .extend(parse_list_value(item_text.trim()));
                continue;
            } else if line.trim().is_empty() {
                // Blank lines are legal inside a YAML block sequence; they
                // do not terminate it.
                continue;
            }
            // Any other content ends the block; fall through to parse THIS
            // line normally (it may be the next key, or the closing fence,
            // already handled above).
            in_skills_block = false;
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
            Some((key, value)) if key == "skills" && value.trim().is_empty() => {
                // A bare `skills:` line — either the start of a block-style
                // list (continuation lines follow) or a deliberately empty
                // declaration. Either way, ensure `skills_block` is `Some`
                // so the empty-inline-value path below is never consulted.
                in_skills_block = true;
                skills_block.get_or_insert_with(Vec::new);
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

    // DOC-42 issue #2906 review, MEDIUM finding: a malformed *inline*
    // `skills:` value (e.g. `skills: ,` or `skills: [,]`) silently degraded
    // to an empty list with no signal. Warn explicitly whenever the raw
    // value was non-empty and not the deliberate `[]` empty-array spelling,
    // yet parsed to nothing — this is the "malformed" branch
    // §SPEC-AGENTSKILLS-01 requires be logged, not silently swallowed.
    let inline_skills = fields.remove("skills").map(|raw_value| {
        let parsed = parse_list_value(&raw_value);
        let trimmed_raw = raw_value.trim();
        if parsed.is_empty() && !trimmed_raw.is_empty() && trimmed_raw != "[]" {
            tracing::warn!(
                value = %raw_value,
                "agent frontmatter `skills:` value could not be parsed as a list — \
                 treating as empty"
            );
        }
        parsed
    });

    // #2897: `tools:` uses the same inline flow-array grammar as `skills:`
    // (via `parse_list_value`) — same malformed-value warn-and-degrade
    // behaviour, just a distinct merge policy (override, not union) applied
    // downstream in `merge_frontmatter`. Unlike `skills:`, the OUTER `Option`
    // here is load-bearing: `fields.remove("tools")` is `None` only when the
    // key is entirely absent, which is what lets `merge_frontmatter`
    // distinguish "inherit the parent" (key absent) from "explicit deny-all"
    // (`tools: []`, which still parses to `Some(vec![])`).
    let tools = fields.remove("tools").map(|raw_value| {
        let parsed = parse_list_value(&raw_value);
        let trimmed_raw = raw_value.trim();
        if parsed.is_empty() && !trimmed_raw.is_empty() && trimmed_raw != "[]" {
            tracing::warn!(
                value = %raw_value,
                "agent frontmatter `tools:` value could not be parsed as a list — \
                 treating as empty"
            );
        }
        parsed
    });

    // #2897: `max_tokens:` is a scalar integer, merged child-wins like
    // `model:`. A malformed value (non-integer) is warned and dropped rather
    // than hard-erroring the whole frontmatter parse.
    let max_tokens = fields.remove("max_tokens").and_then(|raw_value| {
        let trimmed = raw_value.trim();
        if trimmed.is_empty() {
            return None;
        }
        match trimmed.parse::<u32>() {
            Ok(v) => Some(v),
            Err(_) => {
                tracing::warn!(
                    value = %raw_value,
                    "agent frontmatter `max_tokens:` value could not be parsed as an integer — \
                     ignoring"
                );
                None
            }
        }
    });

    let fm = Frontmatter {
        // #3556: `merge_frontmatter`/`render_scalar` now double-quotes and
        // escapes any of these scalars that needed it on emit (mirroring the
        // pre-existing `initialPrompt` treatment below); decode that escaping
        // here so a compose -> deploy -> re-compose cycle round-trips the
        // original value verbatim. `unescape_yaml_double_quoted` is a no-op
        // for a value with no backslash escapes, so plain (unquoted-on-emit)
        // values are unaffected.
        name: fields
            .remove("name")
            .map(|v| unescape_yaml_double_quoted(&v)),
        role: fields
            .remove("role")
            .map(|v| unescape_yaml_double_quoted(&v)),
        description: fields
            .remove("description")
            .map(|v| unescape_yaml_double_quoted(&v)),
        model: fields
            .remove("model")
            .map(|v| unescape_yaml_double_quoted(&v)),
        extends: fields.remove("extends"),
        // `parse_kv_line` lower-cases keys, so `initialPrompt` arrives as
        // `initialprompt`; match that normalised form. Decode the YAML
        // double-quote escapes (`parse_kv_line` already stripped the single
        // outer quote pair) so a re-composed prompt round-trips verbatim.
        initial_prompt: fields
            .remove("initialprompt")
            .map(|v| unescape_yaml_double_quoted(&v)),
        resource_tier: fields
            .remove("resource_tier")
            .map(|v| unescape_yaml_double_quoted(&v)),
        skills: skills_block.or(inline_skills).unwrap_or_default(),
        max_tokens,
        tools,
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
/// Why: `merge_frontmatter` wraps the `initialPrompt` value — and, since
/// issue #3556, any other scalar [`render_scalar`] decided needs quoting —
/// in double-quotes. A raw `"` or `\` in the value would otherwise terminate
/// the quote early or be misread as an escape, and a raw embedded newline
/// would break the single-line frontmatter grammar entirely; either produces
/// malformed YAML. claude-mpm parity requires the emitted frontmatter always
/// be parseable.
/// What: a single pass over `value`'s characters that escapes `\` → `\\`,
/// `"` → `\"`, and a literal newline → the two-character sequence `\n` (so a
/// multi-line description/model/etc. value still composes to one physical
/// frontmatter line). Every other character passes through unchanged. The
/// exact inverse of [`unescape_yaml_double_quoted`].
/// Test: `initial_prompt_with_embedded_quote_round_trips`,
/// `initial_prompt_with_backslash_round_trips`,
/// `description_with_embedded_newline_round_trips` in builder_tests.rs.
fn escape_yaml_double_quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

/// Reverse [`escape_yaml_double_quoted`] on a parsed scalar value.
///
/// Why: a value parsed back from emitted frontmatter still carries the `\"`
/// and `\\` escapes after [`parse_kv_line`] strips the single outer quote pair.
/// Decoding them here makes a compose→deploy→re-compose round-trip yield the
/// original `initialPrompt` string unchanged.
/// What: collapses `\\` → `\`, `\"` → `"`, and `\n` (the two-character
/// escape sequence) → a literal newline; any other `\x` sequence (and a
/// trailing lone `\`) is left verbatim so non-escaped backslashes survive.
/// Test: `initial_prompt_with_embedded_quote_round_trips`,
/// `initial_prompt_with_backslash_round_trips`,
/// `description_with_embedded_newline_round_trips` in builder_tests.rs.
fn unescape_yaml_double_quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('n') => out.push('\n'),
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

/// Whether a frontmatter scalar value requires YAML quoting to stay parseable
/// by a strict YAML reader.
///
/// Why (issue #3556): `merge_frontmatter` used to emit every scalar field
/// (`name`, `role`, `description`, `model`, `resource_tier`) as a bare plain
/// scalar regardless of content, even though `parse_kv_line` had already
/// stripped any quotes the SOURCE file used. `split_frontmatter`'s own
/// lenient parser (first-colon-only split) tolerates a colon anywhere in the
/// value, so a source template quoting `description: 'Rust 2024 edition
/// specialist: memory-safe systems...'` composed to the exact same UNQUOTED
/// `description: Rust 2024 edition specialist: memory-safe systems...` line
/// regardless of the source's quoting style — invalid YAML a strict parser
/// (`serde_yaml`, used by `trusty-agents::agents::registry::md_agent::parse_md_agent`)
/// rejects with "mapping values are not allowed in this context". Because
/// compose output was invariant to source quoting, re-provisioning alone
/// could never have fixed the 11 affected agents — recomposing reproduced
/// byte-identical broken output. Quoting-on-emit whenever a value is unsafe
/// as a plain scalar fixes it at the one place all consumers share.
/// What: `true` when `value` is empty, opens with a YAML indicator
/// character, has leading/trailing whitespace, contains an embedded newline,
/// is one of the YAML core-schema null tokens (`null`/`Null`/`NULL`/`~`,
/// which would otherwise round-trip through `Option<String>` as `None`
/// instead of the literal string), or contains a `": "` / trailing `:` / a
/// mid-string `" #"` sequence a plain scalar cannot represent. The `" #"`
/// check exists because a space followed by `#` starts a YAML comment
/// ANYWHERE in a plain scalar, not just at the start of the line — the
/// leading-character check above only catches `#` as the very first
/// character, so a value like `Model context protocol #1 tool for
/// delegating` silently truncated to `Model context protocol` at the first
/// `" #"` with no error anywhere in the pipeline (code-critic review of
/// #3556's PR #3565): `compose_agent` succeeds, `validate_frontmatter`
/// accepts it (truncated-but-still-valid YAML is syntactically fine), and
/// the real consumer (`trusty-agents`' `.md` loader) silently drops
/// everything from the `#` onward. Same blind spot as the #3556 root cause
/// (trusty-mpm's own lenient `parse_kv_line` only treats `#` as a comment
/// marker at the start of the trimmed line, so it never notices either) —
/// just a different trigger character.
/// Test: `needs_quoting_true_for_colon_space`, `needs_quoting_true_for_empty`,
/// `needs_quoting_false_for_plain_value`, `needs_quoting_true_for_mid_string_hash_comment`,
/// `needs_quoting_true_for_embedded_newline`, `needs_quoting_true_for_null_tokens`,
/// `needs_quoting_indicator_characters` (table-driven) in builder_tests.rs.
fn needs_quoting(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    if matches!(value, "null" | "Null" | "NULL" | "~") {
        return true;
    }
    let first = value.chars().next().expect("checked non-empty above");
    if "!&*-?|>%@`\"'#,[]{}:".contains(first) {
        return true;
    }
    if value.starts_with(' ') || value.ends_with(' ') {
        return true;
    }
    if value.contains('\n') {
        return true;
    }
    value.contains(": ") || value.ends_with(':') || value.contains(" #")
}

/// Render one frontmatter scalar value, quoting it when [`needs_quoting`]
/// says a plain scalar would not survive a strict YAML parse.
///
/// Why: shared by every scalar field `merge_frontmatter` emits (`name`,
/// `role`, `description`, `model`, `resource_tier`) so the quote-when-needed
/// policy is applied uniformly rather than ad hoc per field (issue #3556).
/// What: returns `value` unchanged when it is safe as a plain scalar;
/// otherwise a double-quoted, escaped scalar via
/// [`escape_yaml_double_quoted`] — the same quoting style `initialPrompt`
/// already used, so `split_frontmatter`'s existing `unescape_yaml_double_quoted`
/// decode (now applied to these fields too) round-trips it.
/// Test: `compose_description_with_colon_is_quoted_and_strict_yaml_valid`,
/// `compose_description_without_colon_is_unquoted` in builder_tests.rs.
fn render_scalar(value: &str) -> String {
    if needs_quoting(value) {
        format!("\"{}\"", escape_yaml_double_quoted(value))
    } else {
        value.to_string()
    }
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
/// the parent's. `max_tokens:` (#2897) is a scalar and merges CHILD-WINS,
/// identically to `model:`. `tools:` (#2897) is a list but, unlike `skills:`,
/// merges by OVERRIDE, keyed on `Option` presence rather than emptiness — a
/// child whose `tools:` key was present (`Some`, including the deliberate
/// deny-all `Some(vec![])` from `tools: []`) replaces the parent's list
/// entirely, so a restrictive leaf agent can narrow a permissive base's tool
/// set down to zero; a child that omits `tools:` (`None`) inherits the
/// parent's list unchanged. Emits the populated keys in a stable order.
/// Test: `compose_engineer_chain` (merge), `tier_model_mapping`,
/// `explicit_model_wins_over_tier`, `initial_prompt_injected_by_role`,
/// `initial_prompt_explicit_wins`, `skills_union_across_chain`,
/// `skills_deduplicated_across_chain`, `max_tokens_child_wins_across_chain`,
/// `tools_override_across_chain`, `tools_override_contrasts_with_skills_union`,
/// `tools_empty_list_overrides_to_deny_all` in builder_tests.rs.
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
        // #2897: scalar child-wins, identical to `model:`.
        if fm.max_tokens.is_some() {
            merged.max_tokens = fm.max_tokens;
        }
        // #2897: OVERRIDE, not union (contrast with `skills:` above) — a
        // child's `tools:` (whenever the key was present at all, i.e.
        // `Some`) replaces the parent's list entirely, so a restrictive leaf
        // can narrow a permissive base. Critically this branches on `Some`,
        // NOT on non-empty: `tools: []` is `Some(vec![])` and MUST override
        // to zero tools (deny-all) rather than being treated the same as an
        // omitted key — collapsing "empty" and "absent" here would silently
        // resurrect the parent's full tool list for an operator who
        // deliberately locked an agent down (code-critic finding, PR #2952).
        // An omitted child `tools:` (`None`) leaves the inherited value
        // untouched.
        if let Some(t) = &fm.tools {
            merged.tools = Some(t.clone());
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
        out.push_str(&format!("name: {}\n", render_scalar(v)));
    }
    if let Some(v) = &merged.role {
        out.push_str(&format!("role: {}\n", render_scalar(v)));
    }
    if let Some(v) = &merged.description {
        out.push_str(&format!("description: {}\n", render_scalar(v)));
    }
    if let Some(v) = &merged.model {
        out.push_str(&format!("model: {}\n", render_scalar(v)));
    }
    if let Some(v) = &merged.max_tokens {
        out.push_str(&format!("max_tokens: {v}\n"));
    }
    if let Some(v) = &merged.resource_tier {
        out.push_str(&format!("resource_tier: {}\n", render_scalar(v)));
    }
    if !merged.skills.is_empty() {
        // DOC-42: emit the flattened, unioned skill list so a deployed agent
        // file is self-describing — `tm doctor` / `tm agent show` read it
        // straight off the deployed `.md` without re-walking `extends:`.
        out.push_str(&format!("skills: [{}]\n", merged.skills.join(", ")));
    }
    if let Some(v) = &merged.tools {
        // #2897: emit whenever `Some` — including `Some(vec![])`, which
        // renders as the explicit empty array `tools: []` (an empty `join`
        // yields an empty string between the brackets). This is the deny-all
        // case and round-trips cleanly: re-parsing `tools: []` via
        // `parse_list_value` yields an empty `Vec` again, and
        // `split_frontmatter` sees the KEY as present, so it comes back as
        // `Some(vec![])` — not `None` — preserving the override on a
        // compose→deploy→re-compose cycle. A `None` (key never declared)
        // emits no `tools:` line at all, mirroring the `skills:` emission
        // above but keyed on presence rather than non-emptiness.
        out.push_str(&format!("tools: [{}]\n", v.join(", ")));
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
/// chain must be walked to its root before bodies are joined. Generic over
/// [`SourceLookup`] so both the fs-backed [`SourceMap`] and an in-memory
/// asset map (`agents::builder_in_memory::InMemorySources`, issue #2958
/// Slice E1) share this one walk instead of forking it.
/// What: resolves `name` via `sources.read_source`, parses its frontmatter,
/// recurses into `extends` while tracking the visited path for cycle
/// detection and enforcing the depth limit, then returns
/// `(frontmatter chain, body chain)` ordered base-first.
/// Test: `cycle_detection`, `depth_exceeded`,
///       `case_insensitive_resolve_via_map` (fs); `compose_in_memory_*`,
///       `in_memory_cycle_detection` (in-memory, builder_in_memory.rs).
pub(crate) fn resolve<S: SourceLookup>(
    name: &str,
    sources: &S,
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

    let raw = sources.read_source(name)?;
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

/// Render a resolved, base-first chain into one composed Markdown document.
///
/// Why: [`compose_agent`] and `compose_agent_in_memory` (issue #2958 Slice
/// E1) both need to turn a `(frontmatter chain, body chain)` pair from
/// [`resolve`] into the exact same output shape — one merged frontmatter
/// block followed by the concatenated, blank-line-separated bodies.
/// Extracting this shared tail keeps the two entry points byte-identical for
/// identical input instead of risking silent drift between two copies.
/// What: delegates frontmatter merging to [`merge_frontmatter`], then joins
/// non-empty, newline-trimmed bodies with a blank line between each.
/// Test: `compose_engineer_chain`, `compose_base_only` (fs);
/// `fs_and_in_memory_compose_are_byte_equivalent` (builder_in_memory.rs).
pub(crate) fn render_composed(frontmatters: &[Frontmatter], bodies: &[String]) -> String {
    let mut out = merge_frontmatter(frontmatters);
    out.push('\n');
    let joined: Vec<String> = bodies
        .iter()
        .map(|b| b.trim_matches('\n').to_string())
        .filter(|b| !b.is_empty())
        .collect();
    out.push_str(&joined.join("\n\n"));
    out.push('\n');
    out
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
    Ok(render_composed(&frontmatters, &bodies))
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
#[path = "builder_tests.rs"]
mod tests;
