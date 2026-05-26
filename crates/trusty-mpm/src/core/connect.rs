//! Session connection target resolution.
//!
//! Why: `tm connect <target>` must resolve a fuzzy target (session ID, name
//! prefix, or project path) to a definitive session ID before opening the TUI.
//! Keeping the resolution logic in `trusty-mpm-core` makes it testable without
//! a daemon or terminal.
//! What: `resolve_target` searches a slice of `SessionSummary` using the
//! priority order: exact ID → name prefix → workdir prefix (most-recent-first).
//! Test: All resolution paths and ambiguity/not-found cases are unit-tested.

/// Minimal session summary for resolution (mirrors the daemon's JSON shape).
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    pub id: String,
    pub name: Option<String>,
    pub workdir: String,
    /// Unix timestamp of last activity (for recency sorting).
    pub last_active: u64,
}

/// Outcome of `resolve_target`.
#[derive(Debug, PartialEq)]
pub enum ResolveResult {
    /// Exactly one session matched.
    Found(String),
    /// Multiple sessions matched — caller should show the list.
    Ambiguous(Vec<String>),
    /// No session matched.
    NotFound,
}

/// Resolve `target` against `sessions` using priority order:
/// 1. Exact session ID match (case-sensitive)
/// 2. Session name prefix match (case-insensitive)
/// 3. Workdir prefix match — pick most recently active
///
/// Why: Operators type partial names or paths; this gives predictable,
/// documented priority so behaviour is easy to explain and test.
/// What: Returns `Found(id)` on unambiguous match, `Ambiguous(ids)` when
/// multiple sessions share a prefix, `NotFound` otherwise.
/// Test: See unit tests below.
pub fn resolve_target(target: &str, sessions: &[SessionSummary]) -> ResolveResult {
    // 1. Exact ID.
    if let Some(s) = sessions.iter().find(|s| s.id == target) {
        return ResolveResult::Found(s.id.clone());
    }

    // 2. Name prefix (case-insensitive).
    let lower = target.to_lowercase();
    let name_matches: Vec<_> = sessions
        .iter()
        .filter(|s| {
            s.name
                .as_deref()
                .map(|n| n.to_lowercase().starts_with(&lower))
                .unwrap_or(false)
        })
        .collect();
    match name_matches.len() {
        1 => return ResolveResult::Found(name_matches[0].id.clone()),
        n if n > 1 => {
            return ResolveResult::Ambiguous(name_matches.iter().map(|s| s.id.clone()).collect());
        }
        _ => {}
    }

    // 3. Workdir prefix — pick most-recent on unambiguous match.
    let mut dir_matches: Vec<_> = sessions
        .iter()
        .filter(|s| s.workdir.starts_with(target))
        .collect();
    match dir_matches.len() {
        0 => ResolveResult::NotFound,
        1 => ResolveResult::Found(dir_matches[0].id.clone()),
        _ => {
            // Sort by recency (descending) and return the most recent.
            dir_matches.sort_by_key(|m| std::cmp::Reverse(m.last_active));
            ResolveResult::Found(dir_matches[0].id.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(id: &str, name: Option<&str>, workdir: &str, last_active: u64) -> SessionSummary {
        SessionSummary {
            id: id.to_string(),
            name: name.map(str::to_string),
            workdir: workdir.to_string(),
            last_active,
        }
    }

    #[test]
    fn exact_id_wins() {
        let sessions = vec![make("abc123", Some("my-session"), "/home/user/proj", 100)];
        assert_eq!(
            resolve_target("abc123", &sessions),
            ResolveResult::Found("abc123".into())
        );
    }

    #[test]
    fn name_prefix_match() {
        let sessions = vec![
            make("aaa", Some("frontend"), "/proj/fe", 100),
            make("bbb", Some("backend"), "/proj/be", 100),
        ];
        assert_eq!(
            resolve_target("front", &sessions),
            ResolveResult::Found("aaa".into())
        );
    }

    #[test]
    fn name_prefix_ambiguous() {
        let sessions = vec![
            make("aaa", Some("feature-a"), "/proj/a", 100),
            make("bbb", Some("feature-b"), "/proj/b", 100),
        ];
        assert!(matches!(
            resolve_target("feature", &sessions),
            ResolveResult::Ambiguous(_)
        ));
    }

    #[test]
    fn workdir_prefix_most_recent() {
        let sessions = vec![
            make("old", None, "/proj/myapp", 50),
            make("new", None, "/proj/myapp/sub", 200),
        ];
        // Both match "/proj/myapp" prefix; most recent wins.
        assert_eq!(
            resolve_target("/proj/myapp", &sessions),
            ResolveResult::Found("new".into())
        );
    }

    #[test]
    fn not_found() {
        let sessions = vec![make("aaa", Some("foo"), "/proj", 100)];
        assert_eq!(resolve_target("zzz", &sessions), ResolveResult::NotFound);
    }

    #[test]
    fn case_insensitive_name_match() {
        let sessions = vec![make("aaa", Some("MySession"), "/proj", 100)];
        assert_eq!(
            resolve_target("mysession", &sessions),
            ResolveResult::Found("aaa".into())
        );
    }
}
