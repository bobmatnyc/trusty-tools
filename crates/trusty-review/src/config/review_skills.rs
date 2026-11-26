//! `[[review.skills]]` entries of `.trusty-review.toml` (#11899, DOC-93 §2).
//!
//! Why: a repository names its review skills in the same file that already
//! picks its review template (#2995). The pipeline reads that file at the PR
//! base commit (DOC-93 RS-10, RS-20), so this module parses text, not a path.
//! What: [`parse_skill_entries`] turns the file text into one result per
//! entry, in file order: a [`SkillRequest`], or a [`SkippedSkill`] for an
//! entry the loader cannot use. A file that is not valid TOML is an `Err`;
//! the caller then loads no skill, as a malformed file does today. The typed
//! `[review]` parse in `config::review_template` ignores the `skills` key, so
//! existing configuration is read exactly as before.
//! Test: `skill_entries_parse_in_file_order`, `malformed_entries_are_named_not_fatal`,
//! `invalid_toml_is_an_error_and_no_skills_key_is_empty`.

use crate::models::{SkipKind, SkippedSkill};
use crate::pipeline::repo_skills::SkillRequest;

/// One `[[review.skills]]` entry: a usable request, or a named skip.
pub(crate) type SkillEntry = Result<SkillRequest, SkippedSkill>;

/// The keys a `[[review.skills]]` entry may carry.
const KNOWN_KEYS: &[&str] = &["name", "apply_to"];

/// Parse the `[[review.skills]]` entries of a `.trusty-review.toml` text.
///
/// Why: DOC-93 RS-6 and RS-27 — a bad entry skips that entry and is named,
/// never failing the review or the other entries.
/// What: no `review` table or no `skills` key → `Ok(vec![])`. A `skills`
/// value that is not an array → one `malformed_entry` skip. Per entry: not a
/// table → `malformed_entry`; `name` absent or not a string → `invalid_name`
/// (RS-6); an unknown key → `malformed_entry`; `apply_to` not a list of
/// strings → `malformed_entry`. The bare-identifier check (RS-5) and glob
/// parsing (RS-8) happen in the loader. An entry with no usable name is
/// named `entry <n>` (1-based).
/// Test: `skill_entries_parse_in_file_order`, `malformed_entries_are_named_not_fatal`.
pub(crate) fn parse_skill_entries(text: &str) -> Result<Vec<SkillEntry>, String> {
    let table: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
    let Some(skills) = table.get("review").and_then(|r| r.get("skills")) else {
        return Ok(Vec::new());
    };
    let Some(items) = skills.as_array() else {
        return Ok(vec![Err(skip(
            "review.skills",
            SkipKind::MalformedEntry,
            "`review.skills` is not a list of `[[review.skills]]` entries",
        ))]);
    };
    Ok(items
        .iter()
        .enumerate()
        .map(|(i, item)| parse_entry(i + 1, item))
        .collect())
}

/// Parse entry number `n` (1-based).
fn parse_entry(n: usize, item: &toml::Value) -> SkillEntry {
    let label = format!("entry {n}");
    let Some(entry) = item.as_table() else {
        return Err(skip(
            &label,
            SkipKind::MalformedEntry,
            "entry is not a table",
        ));
    };
    let Some(name) = entry.get("name").and_then(toml::Value::as_str) else {
        return Err(skip(
            &label,
            SkipKind::InvalidName,
            "`name` is absent or not a string",
        ));
    };
    if let Some(key) = entry.keys().find(|k| !KNOWN_KEYS.contains(&k.as_str())) {
        return Err(skip(
            name,
            SkipKind::MalformedEntry,
            &format!("unknown key `{key}`"),
        ));
    }
    let apply_to = match entry.get("apply_to") {
        None => None,
        Some(value) => {
            let globs: Option<Vec<String>> = value
                .as_array()
                .and_then(|a| a.iter().map(|g| g.as_str().map(str::to_string)).collect());
            let Some(globs) = globs else {
                let why = "`apply_to` is not a list of strings";
                return Err(skip(name, SkipKind::MalformedEntry, why));
            };
            Some(globs)
        }
    };
    Ok(SkillRequest {
        name: name.to_string(),
        apply_to,
    })
}

fn skip(name: &str, kind: SkipKind, reason: &str) -> SkippedSkill {
    SkippedSkill {
        name: name.to_string(),
        kind,
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_entries_parse_in_file_order() {
        let text = r#"
[review]
template = "strict-security"

[[review.skills]]
name = "rust-errors"
apply_to = ["crates/**/*.rs"]

[[review.skills]]
name = "release-notes"
"#;
        let entries = parse_skill_entries(text).expect("valid toml");
        assert_eq!(
            entries,
            vec![
                Ok(SkillRequest {
                    name: "rust-errors".into(),
                    apply_to: Some(vec!["crates/**/*.rs".into()]),
                }),
                Ok(SkillRequest {
                    name: "release-notes".into(),
                    apply_to: None,
                }),
            ]
        );
    }

    #[test]
    fn malformed_entries_are_named_not_fatal() {
        let text = r#"
[[review.skills]]
apply_to = ["x"]

[[review.skills]]
name = "typo"
aply_to = ["x"]

[[review.skills]]
name = "badglobs"
apply_to = "crates/**"

[[review.skills]]
name = "good"
"#;
        let entries = parse_skill_entries(text).expect("valid toml");
        let kinds: Vec<_> = entries
            .iter()
            .map(|e| e.as_ref().map_err(|s| (s.name.clone(), s.kind)))
            .collect();
        assert_eq!(
            kinds,
            vec![
                Err(("entry 1".to_string(), SkipKind::InvalidName)),
                Err(("typo".to_string(), SkipKind::MalformedEntry)),
                Err(("badglobs".to_string(), SkipKind::MalformedEntry)),
                Ok(&SkillRequest {
                    name: "good".into(),
                    apply_to: None
                }),
            ]
        );
        let not_a_list = parse_skill_entries("[review]\nskills = \"x\"\n").expect("valid toml");
        assert!(matches!(&not_a_list[..], [Err(s)] if s.kind == SkipKind::MalformedEntry));
    }

    #[test]
    fn invalid_toml_is_an_error_and_no_skills_key_is_empty() {
        assert!(parse_skill_entries("[review\nname=").is_err());
        let none = parse_skill_entries("[review]\ntemplate = \"strict-security\"\n");
        assert_eq!(none, Ok(Vec::new()));
    }
}
