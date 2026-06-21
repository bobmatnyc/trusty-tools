//! Project record type for the project registry.
//!
//! Why: the project registry needs a canonical, serializable representation of
//! every known project so it can survive daemon restarts and be exchanged over
//! MCP tools without ambiguity.
//! What: defines [`Project`] — the persisted project record — with all fields
//! needed to identify a project and seed session spawns.
//! Test: serde round-trips are verified in `project_serde_round_trip`; missing
//! optional fields default correctly in `project_without_optionals`.

use serde::{Deserialize, Serialize};

/// A project entry in the project registry.
///
/// Why: the registry tracks repositories an operator works with so that session
/// spawns can reference them by name and auto-registration from session history
/// can populate the registry without operator effort.
/// What: captures the project name (registry key), repository URL,
/// default branch, and optional descriptive metadata.
/// Test: `project_serde_round_trip`, `project_without_optionals`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    /// Short name used as the registry key (e.g. `trusty-tools`).
    ///
    /// Why: a human-readable stable key enables lookups by name rather than by
    /// repo URL, and matches the convention of deriving names from repo basenames.
    /// What: must be non-empty; upsert is keyed on this field.
    pub name: String,

    /// Full repository URL (e.g. `https://github.com/owner/trusty-tools`).
    ///
    /// Why: the session spawner needs the URL to clone a workspace; the registry
    /// provides it so callers do not have to supply it on every `session_new`.
    pub repo_url: String,

    /// The project's default branch (e.g. `main` or `develop`).
    ///
    /// Why: session spawns default to this branch when no explicit ref is given,
    /// reducing the caller's boilerplate.
    pub default_branch: String,

    /// Optional technology-stack hint for the project (e.g. `rust`, `python`).
    ///
    /// Why: agents that auto-configure their tooling can use this hint to select
    /// the right skill set without introspecting the repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack_hint: Option<String>,

    /// Classification tags for the project (e.g. `["backend", "production"]`).
    ///
    /// Why: tags let operators filter the project list and let agents select
    /// projects by domain without parsing descriptions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,

    /// Free-form human-readable description of the project.
    ///
    /// Why: helps operators identify a project from the registry list, especially
    /// when the name alone is ambiguous.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Derive a project name from a repository URL by stripping the `.git` suffix
/// and taking the last path segment.
///
/// Why: auto-registration from session history should produce a sensible name
/// without operator input; the repo basename is the conventional choice.
/// What: handles both HTTPS (`https://host/owner/repo`) and SSH
/// (`git@host:owner/repo.git`) URL forms. For HTTPS, requires at least three
/// non-empty slash-segments so that a bare host URL (`https://github.com/`)
/// returns `None`. For SSH, splits on `:` first, then on `/` within the path
/// component. Strips a trailing `.git` from the chosen segment.
/// Test: `derive_name_from_url_basic`, `derive_name_from_url_with_git_suffix`,
/// `derive_name_from_url_ssh`, `derive_name_from_url_no_path`.
pub fn derive_name_from_url(repo_url: &str) -> Option<String> {
    // SSH form: "git@github.com:owner/repo.git" — no "://" present but ":" is.
    if !repo_url.contains("://") && repo_url.contains(':') {
        let path_part = repo_url.split_once(':')?.1;
        let segment = path_part.trim_end_matches('/').split('/').next_back()?;
        if segment.is_empty() {
            return None;
        }
        return Some(segment.strip_suffix(".git").unwrap_or(segment).to_string());
    }
    // HTTPS / git:// form — collect non-empty slash-segments.
    // Require at least 3 (scheme "https:", host, repo) so that
    // "https://github.com/" (only 2 non-empty segments) returns None.
    let segments: Vec<&str> = repo_url.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 3 {
        return None;
    }
    let segment = segments.last()?;
    Some(segment.strip_suffix(".git").unwrap_or(segment).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_serde_round_trip() {
        let p = Project {
            name: "trusty-tools".into(),
            repo_url: "https://github.com/owner/trusty-tools".into(),
            default_branch: "main".into(),
            stack_hint: Some("rust".into()),
            tags: vec!["backend".into(), "oss".into()],
            description: Some("the unified trusty workspace".into()),
        };
        let json = serde_json::to_string(&p).expect("serialize");
        let back: Project = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(p, back);
    }

    #[test]
    fn project_without_optionals() {
        // Optional fields must default to empty/None and must not appear in JSON.
        let p = Project {
            name: "minimal".into(),
            repo_url: "https://github.com/owner/minimal".into(),
            default_branch: "main".into(),
            stack_hint: None,
            tags: vec![],
            description: None,
        };
        let json = serde_json::to_string(&p).expect("serialize");
        assert!(
            !json.contains("stack_hint"),
            "absent optional must not serialise: {json}"
        );
        assert!(
            !json.contains("tags"),
            "empty tags must not serialise: {json}"
        );
        let back: Project = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.stack_hint, None);
        assert!(back.tags.is_empty());
    }

    #[test]
    fn derive_name_from_url_basic() {
        assert_eq!(
            derive_name_from_url("https://github.com/owner/my-repo"),
            Some("my-repo".into())
        );
    }

    #[test]
    fn derive_name_from_url_with_git_suffix() {
        assert_eq!(
            derive_name_from_url("https://github.com/owner/trusty-tools.git"),
            Some("trusty-tools".into())
        );
    }

    #[test]
    fn derive_name_from_url_ssh() {
        assert_eq!(
            derive_name_from_url("git@github.com:owner/repo.git"),
            Some("repo".into())
        );
    }

    #[test]
    fn derive_name_from_url_trailing_slash() {
        assert_eq!(
            derive_name_from_url("https://github.com/owner/repo/"),
            Some("repo".into())
        );
    }

    #[test]
    fn derive_name_from_url_no_path() {
        // A bare host with no path segment has no valid name.
        assert_eq!(derive_name_from_url("https://github.com/"), None);
    }
}
