//! Schema, parser, and validation for an agent's `permissions:` frontmatter
//! block (#7948).
//!
//! Why: the TUI permission prompt (#3422) needs a declarative rule set to
//! prompt from, and opencode's `permission` config (see the harness
//! compatibility study) is the reference shape: tool globs mapped to
//! `allow`/`ask`/`deny`, optionally discriminated by an argument glob. This
//! module owns what an operator may write and what is rejected outright.
//! What: `RuleDecision`, `PermissionMap` (compiled tool-glob rules, each a
//! single decision or a table of argument globs), `PermissionConfigError`,
//! and `parse_permissions` — the one entry point, which lifts the
//! `permissions:` block out of an agent document's frontmatter and compiles it.
//!
//! FAIL-CLOSED: a malformed block is an error that fails the agent load. It is
//! never degraded to "no permissions", because an empty map allows every tool.
//! Test: `permissions::tests::config_tests` — the whole module.

use globset::{Glob, GlobMatcher};
use serde::{Deserialize, Serialize};

/// The three decisions a permission rule can carry.
///
/// Why: mirrors opencode's `allow | ask | deny` vocabulary verbatim so an
/// operator's existing config reads the same here.
/// What: `Allow` dispatches; `Deny` refuses without dispatching; `Ask`
/// suspends the call until a client answers (see `crate::permissions::gate`).
/// `rank` orders them for the tie-break — at equal specificity the SAFEST
/// decision wins.
/// Test: `decision_parses_the_three_words`, `decision_rank_orders_deny_over_ask_over_allow`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleDecision {
    Allow,
    Ask,
    Deny,
}

impl RuleDecision {
    /// Parse one decision word exactly as written (lowercase, no aliases).
    ///
    /// Why: a typo like `allowed` or `ALLOW` must be rejected, not coerced —
    /// guessing at intent is how a `deny` silently becomes an `allow`.
    /// Test: `decision_parses_the_three_words`, `unknown_decision_word_is_rejected`.
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "allow" => Some(Self::Allow),
            "ask" => Some(Self::Ask),
            "deny" => Some(Self::Deny),
            _ => None,
        }
    }

    /// The wire/display word for this decision.
    /// Test: `decision_parses_the_three_words`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Ask => "ask",
            Self::Deny => "deny",
        }
    }

    /// Tie-break rank — higher is safer, and wins at equal specificity.
    /// Test: `decision_rank_orders_deny_over_ask_over_allow`.
    pub fn rank(self) -> u8 {
        match self {
            Self::Allow => 0,
            Self::Ask => 1,
            Self::Deny => 2,
        }
    }
}

/// One compiled glob plus the bookkeeping the precedence rule needs.
///
/// Why: `literal_prefix_len` is computed once at parse time because the
/// "longest literal prefix wins" comparison runs on every tool call, and the
/// `pattern` text is kept because a refusal reports it back ("denied by policy
/// (`bash[rm *]`)") — a compiled `GlobMatcher` cannot be printed.
/// What: `*` deliberately spans `/` (globset's default) so `rm *` matches
/// `rm -rf /tmp/x`.
/// Test: `literal_prefix_stops_at_the_first_metacharacter`.
#[derive(Debug, Clone)]
pub struct CompiledGlob {
    pattern: String,
    matcher: GlobMatcher,
    literal_prefix_len: usize,
}

impl CompiledGlob {
    /// Compile `pattern`, or report why it is not a usable glob.
    ///
    /// `pub` because `crate::permissions::session` also compiles the pattern a
    /// client attaches to an `allow_for_session` grant.
    /// Test: `bad_glob_is_rejected`.
    pub fn compile(pattern: &str) -> Result<Self, String> {
        let glob = Glob::new(pattern).map_err(|e| e.to_string())?;
        Ok(Self {
            pattern: pattern.to_string(),
            matcher: glob.compile_matcher(),
            literal_prefix_len: literal_prefix_len(pattern),
        })
    }

    /// The pattern text as the operator wrote it.
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// Whether `candidate` matches this glob.
    pub fn is_match(&self, candidate: &str) -> bool {
        self.matcher.is_match(candidate)
    }

    /// How many leading characters of the pattern are literal.
    pub fn literal_prefix_len(&self) -> usize {
        self.literal_prefix_len
    }
}

/// Count the leading characters of `pattern` before its first glob
/// metacharacter (`*`, `?`, `[`, `{`); a pattern with none scores its length.
///
/// Why: the specificity tie-break — `git status*` (10) beats `git *` (4) beats
/// `*` (0), which makes a catch-all `"*": ask` safe beside narrow allows.
/// Test: `literal_prefix_stops_at_the_first_metacharacter`.
fn literal_prefix_len(pattern: &str) -> usize {
    pattern
        .chars()
        .take_while(|c| !matches!(c, '*' | '?' | '[' | '{'))
        .count()
}

/// What a tool-glob key maps to.
///
/// Why: `read_file: allow` (the whole tool) and `bash: { "rm *": deny }` (one
/// tool, discriminated by its subject) need different precedence tiers — see
/// `crate::permissions::matcher`.
/// What: `Tool` is one decision for every call of the matching tools; `Args`
/// is a set of subject globs, each with its own decision. An `Args` body that
/// matches no subject contributes nothing.
/// Test: `tool_level_string_and_arg_level_table_both_parse`.
#[derive(Debug, Clone)]
pub enum RuleBody {
    Tool(RuleDecision),
    Args(Vec<ArgRule>),
}

/// One argument-glob rule inside a tool's table.
#[derive(Debug, Clone)]
pub struct ArgRule {
    pub glob: CompiledGlob,
    pub decision: RuleDecision,
}

/// One `<tool-glob>: <body>` entry.
#[derive(Debug, Clone)]
pub struct ToolRule {
    pub glob: CompiledGlob,
    pub body: RuleBody,
}

/// An agent's compiled permission map.
///
/// Why: the runtime artifact `crate::permissions::gate` consults before every
/// dispatch, compiled once at agent-load time so the hot path only matches.
/// What: entries in declaration order; order does NOT decide precedence (see
/// `crate::permissions::matcher`). An empty map (`permissions:` with nothing
/// under it) matches nothing, which is `allow` — the pre-#7948 behaviour.
/// Test: `permissions::tests::config_tests`, `permissions::tests::matcher_tests`.
#[derive(Debug, Clone, Default)]
pub struct PermissionMap {
    pub(crate) rules: Vec<ToolRule>,
}

impl PermissionMap {
    /// How many tool-glob entries this map carries.
    /// Test: `tool_level_string_and_arg_level_table_both_parse`.
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Whether this map carries no entries at all.
    /// Test: `empty_mapping_parses_to_an_empty_map`.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

/// Every way a `permissions:` block is rejected (#7948 fail-closed contract).
///
/// Why: each variant names the file and the key, so an operator can find the
/// defect among thirty roster agents.
/// What: `source_label` is the agent file path (or embedded agent name).
/// Test: `unknown_decision_word_is_rejected`, `bad_glob_is_rejected`,
/// `non_string_non_table_value_is_rejected`, `malformed_yaml_is_rejected`,
/// `scalar_permissions_value_is_rejected`.
#[derive(Debug, thiserror::Error)]
pub enum PermissionConfigError {
    #[error(
        "{source_label}: `permissions:` must be a mapping of tool globs to decisions, got {found}"
    )]
    NotAMapping {
        source_label: String,
        found: &'static str,
    },
    #[error(
        "{source_label}: `permissions: {key}` has unknown decision `{value}` — expected one of allow, ask, deny"
    )]
    UnknownDecision {
        source_label: String,
        key: String,
        value: String,
    },
    #[error("{source_label}: `permissions: {key}` is not a valid glob pattern: {reason}")]
    BadGlob {
        source_label: String,
        key: String,
        reason: String,
    },
    #[error(
        "{source_label}: `permissions: {key}` must be a decision string or a table of argument globs, got {found}"
    )]
    BadValue {
        source_label: String,
        key: String,
        found: &'static str,
    },
    #[error("{source_label}: `permissions:` block is not valid YAML: {reason}")]
    Yaml {
        source_label: String,
        reason: String,
    },
}

/// Parse the `permissions:` block out of one agent document, if it has one.
///
/// Why: the single entry point every fallible agent-loading path calls. It
/// takes the whole document because `trusty_agents_common`'s frontmatter
/// reader is flat and line-oriented, so the nested block must be lifted out
/// and handed to a real YAML parser here.
/// What: `Ok(None)` when the document has no `permissions:` key. Otherwise
/// extracts the block's own text, YAML-parses only that (the surrounding
/// frontmatter need not be valid YAML), and compiles every rule. Any defect is
/// a `PermissionConfigError`, never an empty map.
/// Test: `absent_key_parses_to_none`, `example_block_from_the_spec_parses`,
/// `malformed_yaml_is_rejected`, `frontmatter_that_is_not_yaml_still_parses_permissions`.
pub fn parse_permissions(
    source_label: &str,
    document: &str,
) -> Result<Option<PermissionMap>, PermissionConfigError> {
    let Some(block) = extract_permissions_block(document) else {
        return Ok(None);
    };
    let value: serde_yaml::Value =
        serde_yaml::from_str(&block).map_err(|e| PermissionConfigError::Yaml {
            source_label: source_label.to_string(),
            reason: e.to_string(),
        })?;
    // A bare `permissions:` line parses to null: the operator wrote an empty map.
    let body = value
        .get("permissions")
        .cloned()
        .unwrap_or(serde_yaml::Value::Null);
    compile_map(source_label, &body).map(Some)
}

/// Lift the `permissions:` key and everything indented under it out of a
/// document's frontmatter block.
///
/// Why: a composed agent's frontmatter is not guaranteed to be valid YAML
/// (values are emitted unquoted), so parsing the whole block would fail agents
/// that declare no permissions at all.
/// What: returns the block re-indented to column 0, including its own
/// `permissions:` line, or `None` when there is no frontmatter or no such key.
/// Ends at the first non-blank line indented at or below the key, or at `---`.
/// Test: `frontmatter_that_is_not_yaml_still_parses_permissions`,
/// `block_stops_at_the_next_frontmatter_key`.
fn extract_permissions_block(document: &str) -> Option<String> {
    let mut lines = document.lines();
    match lines.next() {
        Some(first) if first.trim() == "---" => {}
        _ => return None,
    }

    let mut collected: Vec<String> = Vec::new();
    let mut key_indent = 0usize;
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        let indent = line.len() - line.trim_start().len();
        if collected.is_empty() {
            let rest = line.trim_start();
            if rest.starts_with("permissions:") {
                key_indent = indent;
                collected.push(rest.to_string());
            }
            continue;
        }
        if line.trim().is_empty() {
            collected.push(String::new());
            continue;
        }
        if indent <= key_indent {
            break;
        }
        collected.push(line[key_indent..].to_string());
    }

    (!collected.is_empty()).then(|| collected.join("\n"))
}

/// Compile one YAML value into a `PermissionMap`: null is an empty map, a
/// mapping compiles entry by entry, anything else is `NotAMapping`.
/// Test: `empty_mapping_parses_to_an_empty_map`, `scalar_permissions_value_is_rejected`.
fn compile_map(
    source_label: &str,
    body: &serde_yaml::Value,
) -> Result<PermissionMap, PermissionConfigError> {
    let mapping = match body {
        serde_yaml::Value::Null => return Ok(PermissionMap::default()),
        serde_yaml::Value::Mapping(m) => m,
        other => {
            return Err(PermissionConfigError::NotAMapping {
                source_label: source_label.to_string(),
                found: yaml_kind(other),
            });
        }
    };

    let mut rules = Vec::with_capacity(mapping.len());
    for (key, value) in mapping {
        let key = scalar_key(key).ok_or_else(|| PermissionConfigError::BadValue {
            source_label: source_label.to_string(),
            key: "<non-scalar key>".to_string(),
            found: yaml_kind(key),
        })?;
        let glob = compile_glob(source_label, &key, &key)?;
        let body = compile_body(source_label, &key, value)?;
        rules.push(ToolRule { glob, body });
    }
    Ok(PermissionMap { rules })
}

/// Compile one tool-glob key's value into a `RuleBody`.
/// Test: `tool_level_string_and_arg_level_table_both_parse`,
/// `non_string_non_table_value_is_rejected`.
fn compile_body(
    source_label: &str,
    key: &str,
    value: &serde_yaml::Value,
) -> Result<RuleBody, PermissionConfigError> {
    let bad_value = |key: String, found: &'static str| PermissionConfigError::BadValue {
        source_label: source_label.to_string(),
        key,
        found,
    };
    match value {
        serde_yaml::Value::String(word) => {
            Ok(RuleBody::Tool(decision_word(source_label, key, word)?))
        }
        serde_yaml::Value::Mapping(args) => {
            let mut compiled = Vec::with_capacity(args.len());
            for (arg_key, arg_value) in args {
                let arg_pattern = scalar_key(arg_key)
                    .ok_or_else(|| bad_value(key.to_string(), yaml_kind(arg_key)))?;
                let label = format!("{key}: {arg_pattern}");
                let word = arg_value
                    .as_str()
                    .ok_or_else(|| bad_value(label.clone(), yaml_kind(arg_value)))?;
                compiled.push(ArgRule {
                    glob: compile_glob(source_label, &label, &arg_pattern)?,
                    decision: decision_word(source_label, &label, word)?,
                });
            }
            Ok(RuleBody::Args(compiled))
        }
        other => Err(bad_value(key.to_string(), yaml_kind(other))),
    }
}

/// Parse a decision word or report it against `key`.
fn decision_word(
    source_label: &str,
    key: &str,
    word: &str,
) -> Result<RuleDecision, PermissionConfigError> {
    RuleDecision::parse(word).ok_or_else(|| PermissionConfigError::UnknownDecision {
        source_label: source_label.to_string(),
        key: key.to_string(),
        value: word.to_string(),
    })
}

/// Compile a glob or report it against `key`.
fn compile_glob(
    source_label: &str,
    key: &str,
    pattern: &str,
) -> Result<CompiledGlob, PermissionConfigError> {
    CompiledGlob::compile(pattern).map_err(|reason| PermissionConfigError::BadGlob {
        source_label: source_label.to_string(),
        key: key.to_string(),
        reason,
    })
}

/// Render a scalar YAML key as a string; a sequence or mapping key is `None`.
fn scalar_key(value: &serde_yaml::Value) -> Option<String> {
    match value {
        serde_yaml::Value::String(s) => Some(s.clone()),
        serde_yaml::Value::Bool(b) => Some(b.to_string()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// A one-word name for a YAML value's shape, for error messages.
fn yaml_kind(value: &serde_yaml::Value) -> &'static str {
    match value {
        serde_yaml::Value::Null => "null",
        serde_yaml::Value::Bool(_) => "a boolean",
        serde_yaml::Value::Number(_) => "a number",
        serde_yaml::Value::String(_) => "a string",
        serde_yaml::Value::Sequence(_) => "a list",
        serde_yaml::Value::Mapping(_) => "a table",
        serde_yaml::Value::Tagged(_) => "a tagged value",
    }
}
