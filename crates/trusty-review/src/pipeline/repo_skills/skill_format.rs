//! Agent Skills format checks for a repo `SKILL.md` (#11899).
//!
//! Why: a skill named by the reviewed repo must be a well-formed Agent Skills
//! file (agentskills.io); a malformed one is skipped, never guessed at.
//! What: [`parse_skill`] applies DOC-93 RS-11 to RS-15: YAML frontmatter
//! opening on the first line and closed by a `---` line (RS-11); a non-empty
//! string `name` and `description` (RS-12); `name` equal to the configured
//! name exactly, with no Agent Skills character-set check on top of the
//! bare-identifier rule (RS-13); a non-empty body (RS-15). UTF-8 (RS-14) is
//! checked by the caller. Other frontmatter keys are ignored.
//! Test: `skill_format_rules`.

/// The checked parts of a valid `SKILL.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SkillDoc {
    /// The markdown after the frontmatter, trimmed.
    pub(crate) body: String,
}

/// Parse and validate `text` as the `SKILL.md` of directory `dir_name`.
///
/// Why: one place decides "invalid", so every caller skips the same files.
/// What: see the module doc; `Err` carries a one-line reason.
/// Test: `skill_format_rules`.
pub(crate) fn parse_skill(dir_name: &str, text: &str) -> Result<SkillDoc, String> {
    let (front, body) = split_frontmatter(text)?;
    let map: serde_yaml::Mapping = serde_yaml::from_str(front)
        .map_err(|e| format!("frontmatter is not a YAML mapping: {e}"))?;
    let field = |key: &str| -> Result<String, String> {
        match map.get(key) {
            Some(serde_yaml::Value::String(s)) => Ok(s.trim().to_string()),
            Some(_) => Err(format!("frontmatter `{key}` is not a string")),
            None => Err(format!("frontmatter has no `{key}`")),
        }
    };
    let name = field("name")?;
    if name.is_empty() {
        return Err("frontmatter `name` is empty".to_string());
    }
    if name != dir_name {
        return Err(format!(
            "frontmatter name {name:?} differs from the directory name"
        ));
    }
    let description = field("description")?;
    if description.is_empty() {
        return Err("frontmatter `description` is empty".to_string());
    }
    let body = body.trim();
    if body.is_empty() {
        return Err("the body after the frontmatter is empty".to_string());
    }
    Ok(SkillDoc {
        body: body.to_string(),
    })
}

/// Split `text` into `(frontmatter, body)` at the `---` delimiter lines.
fn split_frontmatter(text: &str) -> Result<(&str, &str), String> {
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
        .ok_or("no YAML frontmatter: the file must open with a `---` line")?;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\n', '\r']) == "---" {
            return Ok((&rest[..offset], &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    Err("YAML frontmatter is not closed by a `---` line".to_string())
}
