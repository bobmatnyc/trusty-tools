//! The `metadata:` frontmatter map: parse, merge and emit (#9011).
//!
//! Why: ADR-0064 versions every agent under `content/agents/` with
//! `metadata: {version: "x.y.z"}`, never a top-level `version:` (that key marks
//! a claude-mpm file in [`super::agent_schema`]). The composer dropped every
//! key it did not model, so a composed agent lost its version, and its
//! line-based parser read the indented `version:` child as a top-level key.
//! `builder.rs` sits at the 500-SLOC cap, so the map lives here.
//! What: [`open`] and [`consume_block_line`] read the flow form
//! (`metadata: {version: "1.0.0"}`) and the block form (a bare `metadata:`
//! followed by indented `key: value` lines) into a [`MetadataMap`]; [`set`]
//! merges one entry child-wins; [`render`] emits the block form with every
//! value double-quoted, so `1.0` stays a string. Keys are lower-cased by
//! [`parse_kv_line`], like every other frontmatter key.
//! Test: `metadata_block_parsed_merged_and_emitted`,
//! `metadata_flow_form_is_parsed`, `metadata_child_wins_across_chain`,
//! `metadata_non_map_value_is_rejected` in builder_tests.rs.

use super::builder::AgentBuildError;
use super::builder_yaml::{escape_yaml_double_quoted, unescape_yaml_double_quoted};
use super::frontmatter::parse_kv_line;

/// One `metadata:` map as `(key, value)` pairs in first-declared order.
pub(crate) type MetadataMap = Vec<(String, String)>;

/// Insert `key`, or overwrite its value in place (child-wins, order kept).
pub(crate) fn set(map: &mut MetadataMap, key: String, value: String) {
    match map.iter_mut().find(|(k, _)| *k == key) {
        Some(entry) => entry.1 = value,
        None => map.push((key, value)),
    }
}

/// Handle the value of a top-level `metadata:` line.
///
/// Why: the key opens either a block (empty value) or carries a flow map.
/// What: returns `Ok(true)` when the value is empty, so the caller reads the
/// following indented lines through [`consume_block_line`]; parses a
/// `{k: v, ...}` flow map into `map` and returns `Ok(false)`; rejects any other
/// value as [`AgentBuildError::FrontmatterParse`].
/// Test: `metadata_flow_form_is_parsed`, `metadata_non_map_value_is_rejected`.
pub(crate) fn open(value: &str, map: &mut MetadataMap) -> Result<bool, AgentBuildError> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(true);
    }
    let inner = value
        .strip_prefix('{')
        .and_then(|v| v.strip_suffix('}'))
        .ok_or_else(|| {
            AgentBuildError::FrontmatterParse(format!("`metadata:` must be a map, got `{value}`"))
        })?;
    for item in inner.split(',').filter(|i| !i.trim().is_empty()) {
        let (key, raw) = parse_kv_line(item).ok_or_else(|| {
            AgentBuildError::FrontmatterParse(format!(
                "`metadata:` entry `{item}` is not `key: value`"
            ))
        })?;
        set(map, key, unescape_yaml_double_quoted(&raw));
    }
    Ok(false)
}

/// Consume one line while a block-form `metadata:` map is open.
///
/// Why: the block ends at the first line that is not indented, and that line
/// must reach the caller's ordinary key parser.
/// What: `Ok(true)` when the line belonged to the block (an indented
/// `key: value`, a blank line, or an indented comment) and was recorded;
/// `Ok(false)` when it is not indented, which closes the block; an indented
/// line that is not `key: value` is [`AgentBuildError::FrontmatterParse`].
/// Test: `metadata_block_parsed_merged_and_emitted`.
pub(crate) fn consume_block_line(
    line: &str,
    map: &mut MetadataMap,
) -> Result<bool, AgentBuildError> {
    let trimmed = line.trim();
    if trimmed.is_empty() || (line.starts_with([' ', '\t']) && trimmed.starts_with('#')) {
        return Ok(true);
    }
    if !line.starts_with([' ', '\t']) {
        return Ok(false);
    }
    let (key, raw) = parse_kv_line(line).ok_or_else(|| {
        AgentBuildError::FrontmatterParse(format!(
            "`metadata:` entry `{trimmed}` is not `key: value`"
        ))
    })?;
    set(map, key, unescape_yaml_double_quoted(&raw));
    Ok(true)
}

/// Render `map` as a YAML block, or nothing when it is empty.
pub(crate) fn render(map: &MetadataMap) -> String {
    if map.is_empty() {
        return String::new();
    }
    let mut out = String::from("metadata:\n");
    for (key, value) in map {
        out.push_str(&format!(
            "  {key}: \"{}\"\n",
            escape_yaml_double_quoted(value)
        ));
    }
    out
}
