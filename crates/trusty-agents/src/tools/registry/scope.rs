//! Scope and scope-pattern types for the OpenRPC tool registry (#453).
//!
//! Why: Operators declare *trusted* scopes per endpoint in `config.toml`
//! (e.g. `scopes = ["google.gmail.read", "google.calendar.*"]`), and agents
//! declare the scopes they may use in their TOML. The registry refuses to
//! expose any tool whose `scope` field isn't covered by the endpoint's
//! operator-declared list, and refuses to dispatch a tool to an agent whose
//! patterns don't match. This module encapsulates the matching algorithm
//! so both checks share one implementation.
//! What: `Scope` wraps a dotted scope string (`"google.gmail.read"`).
//! `ScopePattern` wraps a dotted glob (`"google.*"` or
//! `"google.gmail.read"`). Glob matching is left-to-right; trailing `*`
//! matches any remaining segments. Middle wildcards (`google.*.read`) are
//! intentionally NOT supported — they make endpoint scope auditing
//! ambiguous and are flagged as non-matching.
//! Test: Exhaustive unit tests at the bottom of this file.

use serde::{Deserialize, Serialize};

/// A concrete scope advertised by a discovered tool, e.g. `google.gmail.read`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Scope(String);

impl Scope {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for Scope {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for Scope {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// A pattern an operator or agent may declare, e.g. `google.*` or
/// `google.gmail.read` (exact). Only trailing-`*` globs are supported.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScopePattern(String);

impl ScopePattern {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Does this pattern match the given concrete scope?
    ///
    /// Rules:
    /// - Exact: `"google.gmail.read"` matches only `"google.gmail.read"`.
    /// - Trailing-wildcard: `"google.*"` matches any scope starting with
    ///   `"google."`. `"google"` alone (no trailing dot) does NOT match
    ///   `"google.gmail.read"` — the prefix must be on a segment boundary.
    /// - Middle wildcards (`"google.*.read"`) return `false` to keep the
    ///   model auditable.
    pub fn matches(&self, scope: &Scope) -> bool {
        let pat = self.0.as_str();
        let val = scope.0.as_str();

        // An empty pattern denies everything — it is not a wildcard and
        // matching it against an empty scope would falsely grant access.
        if pat.is_empty() {
            return false;
        }

        // No-wildcard case: exact match.
        if !pat.contains('*') {
            return pat == val;
        }

        // Strip trailing `.*` and check prefix on a segment boundary.
        if let Some(prefix) = pat.strip_suffix(".*") {
            // Reject middle wildcards: there must be no `*` left after
            // stripping the trailing `.*`.
            if prefix.contains('*') {
                return false;
            }
            // Empty prefix `.*` is nonsensical; deny it explicitly.
            if prefix.is_empty() {
                return false;
            }
            // `google.*` matches `google.gmail.read` (val starts with
            // `google.`) but NOT bare `google` (no trailing segment).
            let needed = format!("{prefix}.");
            return val.starts_with(&needed) && val.len() > needed.len();
        }

        // Any other `*` placement (`google.*.read`, `*.read`, `goog*le.read`)
        // is not supported.
        false
    }
}

impl From<String> for ScopePattern {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for ScopePattern {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Does *any* of an agent's declared patterns allow this tool's scope?
///
/// Why: Per-agent scope enforcement (deny by default). An empty pattern
/// list = no permission.
/// What: Returns `true` iff some pattern in `agent_patterns` matches
/// `tool_scope`.
pub fn agent_can_use(agent_patterns: &[ScopePattern], tool_scope: &Scope) -> bool {
    agent_patterns.iter().any(|p| p.matches(tool_scope))
}

/// Given operator-declared endpoint scopes and a list of discovered tools,
/// return only the tools whose scope is allowed by *any* endpoint pattern.
///
/// Why: Discovery returns whatever the remote server claims; operator
/// patterns are the trust filter. We can't let a misconfigured remote
/// expose `admin.*` tools when the operator declared only `google.*`.
/// What: Filters by matching each tool's `scope` against the patterns.
pub fn filter_by_endpoint_scopes<T>(
    endpoint_scopes: &[ScopePattern],
    tools: &[T],
    scope_of: impl Fn(&T) -> &str,
) -> Vec<T>
where
    T: Clone,
{
    tools
        .iter()
        .filter(|t| {
            let s = Scope::new(scope_of(t).to_string());
            endpoint_scopes.iter().any(|p| p.matches(&s))
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(x: &str) -> Scope {
        Scope::new(x)
    }
    fn p(x: &str) -> ScopePattern {
        ScopePattern::new(x)
    }

    #[test]
    fn exact_match() {
        assert!(p("google.gmail.read").matches(&s("google.gmail.read")));
    }

    #[test]
    fn exact_mismatch() {
        assert!(!p("google.gmail.read").matches(&s("google.gmail.write")));
        assert!(!p("google.gmail.read").matches(&s("google.gmail")));
        assert!(!p("google.gmail.read").matches(&s("google.gmail.read.extra")));
    }

    #[test]
    fn trailing_wildcard_matches_one_segment() {
        assert!(p("google.*").matches(&s("google.gmail")));
    }

    #[test]
    fn trailing_wildcard_matches_multiple_segments() {
        assert!(p("google.*").matches(&s("google.gmail.read")));
        assert!(p("google.*").matches(&s("google.calendar.write")));
        assert!(p("google.gmail.*").matches(&s("google.gmail.read")));
        assert!(p("google.gmail.*").matches(&s("google.gmail.read.attachments")));
    }

    #[test]
    fn trailing_wildcard_requires_segment_boundary() {
        // `google.*` must NOT match `googletools.foo` — the `.` is required.
        assert!(!p("google.*").matches(&s("googletools.foo")));
        // And it must not match the bare prefix.
        assert!(!p("google.*").matches(&s("google")));
    }

    #[test]
    fn trailing_wildcard_does_not_match_unrelated_prefix() {
        assert!(!p("google.gmail.*").matches(&s("google.calendar.read")));
        assert!(!p("google.*").matches(&s("microsoft.outlook.read")));
    }

    #[test]
    fn middle_wildcards_are_not_supported() {
        // Explicit denial: middle wildcards return false for everything.
        assert!(!p("google.*.read").matches(&s("google.gmail.read")));
        assert!(!p("google.*.read").matches(&s("google.calendar.read")));
        assert!(!p("*.read").matches(&s("google.read")));
    }

    #[test]
    fn empty_pattern_denies() {
        assert!(!p("").matches(&s("anything")));
        assert!(!p("").matches(&s("")));
    }

    #[test]
    fn empty_wildcard_only_denies() {
        // A bare `*` or `.*` is nonsensical; deny.
        assert!(!p(".*").matches(&s("foo.bar")));
    }

    #[test]
    fn agent_can_use_with_empty_patterns_denies_all() {
        assert!(!agent_can_use(&[], &s("google.gmail.read")));
    }

    #[test]
    fn agent_can_use_with_any_matching_pattern_allows() {
        let agent = vec![p("google.gmail.*"), p("microsoft.outlook.read")];
        assert!(agent_can_use(&agent, &s("google.gmail.read")));
        assert!(agent_can_use(&agent, &s("microsoft.outlook.read")));
        assert!(!agent_can_use(&agent, &s("microsoft.outlook.write")));
        assert!(!agent_can_use(&agent, &s("google.calendar.read")));
    }

    #[test]
    fn filter_by_endpoint_scopes_drops_unauthorized() {
        let endpoint = vec![p("google.*")];
        #[derive(Clone, Debug, PartialEq)]
        struct Tool {
            name: &'static str,
            scope: &'static str,
        }
        let tools = vec![
            Tool {
                name: "gmail_read",
                scope: "google.gmail.read",
            },
            Tool {
                name: "outlook_read",
                scope: "microsoft.outlook.read",
            },
            Tool {
                name: "cal_write",
                scope: "google.calendar.write",
            },
        ];
        let kept = filter_by_endpoint_scopes(&endpoint, &tools, |t| t.scope);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].name, "gmail_read");
        assert_eq!(kept[1].name, "cal_write");
    }

    #[test]
    fn filter_with_no_endpoint_scopes_drops_everything() {
        #[derive(Clone)]
        struct Tool {
            scope: &'static str,
        }
        let tools = vec![Tool {
            scope: "google.gmail.read",
        }];
        let kept = filter_by_endpoint_scopes::<Tool>(&[], &tools, |t| t.scope);
        assert!(kept.is_empty());
    }
}
