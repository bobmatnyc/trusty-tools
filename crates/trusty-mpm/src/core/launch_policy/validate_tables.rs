//! Value checks for the owner file's `[skills]` and `[tools]` tables (#8992).
//!
//! Why: `deny_unknown_fields` rejects an unknown key, but not a value that
//! would no-op (a wildcard, a typo-shaped name) or subtract something a PM or
//! agent needs (a floor name). Each is a bad file, so it fails loudly and the
//! launch falls back to the last good policy.
//! What: [`validate`] answers the first bad value as `(key, reason)`; the
//! caller in [`super::schema`] wraps it in `OwnerPolicyError::Invalid`.
//! Test: `a_forbidden_skills_or_tools_value_is_a_bad_file_naming_its_key`,
//! `good_skills_and_tools_tables_parse`.

use super::floor::{is_skill_floor, is_tool_floor};
use super::schema::OwnerFile;

/// The key a bad `[skills] off` value is reported under.
pub const SKILLS_OFF_KEY: &str = "skills.off";

/// The key a bad `[tools] deny` value is reported under.
pub const TOOLS_DENY_KEY: &str = "tools.deny";

/// The first bad `[skills] off` or `[tools] deny` value in `file`.
///
/// What: a skill name is non-empty, has no `*`/`?`, uses only ASCII
/// alphanumerics and `.` `_` `:` `-` (`plugin:skill` allowed), and is not a
/// floor skill. A tool name is non-empty, has no `*`/`?` and no parentheses
/// (Q-C: bare names only), starts with a letter, uses only ASCII
/// alphanumerics, `_` and `-`, has a non-empty server (and tool, when given)
/// after `mcp__`, and is not a floor tool.
/// Test: `a_forbidden_skills_or_tools_value_is_a_bad_file_naming_its_key`.
pub fn validate(file: &OwnerFile) -> Result<(), (&'static str, String)> {
    let skills = file.skills.iter().flat_map(|t| t.off.iter().flatten());
    for name in skills {
        skill_name(name).map_err(|reason| (SKILLS_OFF_KEY, reason))?;
    }
    let tools = file.tools.iter().flat_map(|t| t.deny.iter().flatten());
    for rule in tools {
        tool_name(rule).map_err(|reason| (TOOLS_DENY_KEY, reason))?;
    }
    Ok(())
}

/// Why `name` cannot be an owner `[skills] off` entry, if it cannot.
fn skill_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("an empty skill name".to_string());
    }
    if name.contains(['*', '?']) {
        return Err(format!(
            "wildcard `{name}` is not allowed; name skills exactly"
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
    {
        return Err(format!("`{name}` is not a skill name"));
    }
    if is_skill_floor(name) {
        return Err(format!("`{name}` is in the never-off skill floor"));
    }
    Ok(())
}

/// Why `rule` cannot be an owner `[tools] deny` entry, if it cannot.
fn tool_name(rule: &str) -> Result<(), String> {
    if rule.trim().is_empty() {
        return Err("an empty tool name".to_string());
    }
    if rule.contains(['*', '?']) {
        return Err(format!(
            "wildcard `{rule}` is not allowed; name tools exactly"
        ));
    }
    if rule.contains(['(', ')']) {
        return Err(format!(
            "`{rule}` is a parenthesised rule; `[tools] deny` takes bare tool names only"
        ));
    }
    let shaped = rule.starts_with(|c: char| c.is_ascii_alphabetic())
        && rule
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'));
    if !shaped {
        return Err(format!("`{rule}` is not a tool name"));
    }
    if let Some(rest) = rule.strip_prefix("mcp__") {
        let mut parts = rest.splitn(2, "__");
        let server = parts.next().unwrap_or_default();
        if server.is_empty() || parts.next().is_some_and(str::is_empty) {
            return Err(format!(
                "`{rule}` is not `mcp__<server>` or `mcp__<server>__<tool>`"
            ));
        }
    }
    if is_tool_floor(rule) {
        return Err(format!("`{rule}` is in the never-deny tool floor"));
    }
    Ok(())
}
