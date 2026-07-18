//! The canonical session-target resolver.
//!
//! Why: four call sites historically resolved an operator's fuzzy session
//! reference (a UUID, a friendly name, or a name prefix) to a definitive target,
//! each with subtly different precedence: the executor's `resolve_session`
//! (`executor/mod.rs`), the CLI's `classify_managed_target` and
//! `resolve_managed_id` (`bin/tm/commands/managed.rs`), and the CLI's
//! `resolve_session_id` (`bin/tm/commands/session.rs`). Divergent precedence is
//! a latent correctness bug: the same argument could resolve differently across
//! UIs. [`resolve_target`] is the one pure resolver every surface adopts so the
//! id-exact / name-exact / name-prefix precedence is defined exactly once.
//! What: a generic pure function over any list of items that expose an id-match
//! predicate and a name (via [`Resolvable`]) plus a query string. It applies a
//! fixed precedence — id-exact, then name-exact, then unambiguous name-prefix —
//! and returns the matched item. The executor's managed and project paths both
//! call it; the CLI resolvers adopt it in Phase 1B.
//! Test: `resolve_target_precedence`, `resolve_target_prefix`,
//! `resolve_target_no_match`, `resolve_target_prefix_is_unambiguous`.

/// Something a [`resolve_target`] query can match: it knows its id and name.
///
/// Why: the resolver must work over both [`crate::client::http_client::SessionRow`]
/// (project sessions, keyed by a UUID newtype + `tmux_name`) and
/// [`crate::client::http_client::ManagedSessionSummary`] (managed sessions,
/// keyed by a `String` id + `name`) without duplicating the precedence logic or
/// forcing an allocation to stringify the UUID. `id_matches` (a predicate, not
/// an accessor) lets the UUID newtype compare against the query in place.
/// What: an `id_matches` predicate for id-exact comparison and a `resolve_name`
/// accessor for the friendly name (may be empty).
/// Test: implemented for the two session shapes and exercised by the resolver
/// tests via a local stub.
pub trait Resolvable {
    /// True when `query` exactly equals this item's canonical id.
    fn id_matches(&self, query: &str) -> bool;
    /// The item's friendly name (may be empty).
    fn resolve_name(&self) -> &str;
}

/// Resolve a fuzzy session reference to a single matched item.
///
/// Why: every UI accepts "id, name, or prefix" and must resolve it the same way;
/// centralizing the precedence here removes the per-call-site drift that let the
/// same argument resolve differently across the CLI, TUI, and bot.
/// What: returns the first item whose id equals `query` (id-exact), else the
/// first whose `resolve_name` equals `query` (name-exact), else — only when
/// **exactly one** item's non-empty name starts with `query` — that unambiguous
/// prefix match. An ambiguous prefix (two or more matches) resolves to `None` so
/// a UI never silently picks the wrong session. An empty `query` never matches.
/// Test: `resolve_target_precedence`, `resolve_target_prefix`,
/// `resolve_target_prefix_is_unambiguous`, `resolve_target_no_match`.
pub fn resolve_target<'a, T: Resolvable>(items: &'a [T], query: &str) -> Option<&'a T> {
    if query.is_empty() {
        return None;
    }
    // 1. id-exact.
    if let Some(item) = items.iter().find(|i| i.id_matches(query)) {
        return Some(item);
    }
    // 2. name-exact.
    if let Some(item) = items.iter().find(|i| i.resolve_name() == query) {
        return Some(item);
    }
    // 3. unambiguous name-prefix.
    let mut prefix_matches = items
        .iter()
        .filter(|i| !i.resolve_name().is_empty() && i.resolve_name().starts_with(query));
    match (prefix_matches.next(), prefix_matches.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    }
}

impl Resolvable for crate::client::http_client::SessionRow {
    /// Why: project sessions are keyed by their UUID `id` and friendly
    /// `tmux_name`; the resolver matches against both, comparing the UUID to the
    /// query string in place rather than allocating it.
    /// What: `id_matches` stringifies the UUID for the comparison; `resolve_name`
    /// returns `tmux_name`.
    /// Test: covered by the executor's `resolve_session_*` tests.
    fn id_matches(&self, query: &str) -> bool {
        self.id.0.to_string() == query
    }
    fn resolve_name(&self) -> &str {
        &self.tmux_name
    }
}

impl Resolvable for crate::client::http_client::ManagedSessionSummary {
    /// Why: managed sessions are keyed by their string `id` and `name`; the
    /// resolver matches against both with the shared precedence.
    /// What: `id_matches` compares the `id` string; `resolve_name` returns `name`.
    /// Test: `resolve_target_managed_precedence`.
    fn id_matches(&self, query: &str) -> bool {
        self.id == query
    }
    fn resolve_name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal stub for exercising the precedence rules directly.
    struct Stub {
        id: String,
        name: String,
    }
    impl Resolvable for Stub {
        fn id_matches(&self, query: &str) -> bool {
            self.id == query
        }
        fn resolve_name(&self) -> &str {
            &self.name
        }
    }
    fn stub(id: &str, name: &str) -> Stub {
        Stub {
            id: id.to_string(),
            name: name.to_string(),
        }
    }

    #[test]
    fn resolve_target_precedence() {
        // id-exact beats name-exact beats prefix.
        let items = vec![stub("uuid-1", "frontend"), stub("uuid-2", "backend")];
        assert_eq!(
            resolve_target(&items, "uuid-1").map(|s| s.id.as_str()),
            Some("uuid-1")
        );
        assert_eq!(
            resolve_target(&items, "backend").map(|s| s.id.as_str()),
            Some("uuid-2")
        );
    }

    #[test]
    fn resolve_target_prefix() {
        let items = vec![stub("uuid-1", "tmpm-blue-fox"), stub("uuid-2", "backend")];
        assert_eq!(
            resolve_target(&items, "tmpm-blue").map(|s| s.name.as_str()),
            Some("tmpm-blue-fox")
        );
    }

    #[test]
    fn resolve_target_prefix_is_unambiguous() {
        // Two names share a prefix → ambiguous → no match (never guess).
        let items = vec![stub("uuid-1", "frontend-a"), stub("uuid-2", "frontend-b")];
        assert!(resolve_target(&items, "frontend-").is_none());
        // But an exact name still wins even when it is a prefix of another.
        let items2 = vec![stub("uuid-1", "front"), stub("uuid-2", "frontend")];
        assert_eq!(
            resolve_target(&items2, "front").map(|s| s.id.as_str()),
            Some("uuid-1"),
            "exact name match beats the ambiguous-prefix rule"
        );
    }

    #[test]
    fn resolve_target_no_match() {
        let items = vec![stub("uuid-1", "frontend")];
        assert!(resolve_target(&items, "no-such").is_none());
        assert!(resolve_target(&items, "").is_none());
    }

    #[test]
    fn resolve_target_managed_precedence() {
        use crate::client::http_client::ManagedSessionSummary;
        let summary = |id: &str, name: &str| ManagedSessionSummary {
            id: id.to_string(),
            name: name.to_string(),
            state: "Running".to_string(),
            workspace_path: None,
            repo_url: None,
            branch: None,
            created_at: None,
            last_activity_at: None,
            pending_decision: None,
            proposed_default: None,
            source_id: None,
            task: None,
            cwd: None,
            claude_session_id: None,
            deliverable_id: None,
            pane_id: None,
            injection_status: None,
            unresumable: false,
            stale_assets: false,
        };
        let items = vec![summary("m-1", "tmpm-red-owl"), summary("m-2", "api")];
        assert_eq!(
            resolve_target(&items, "m-2").map(|s| s.id.as_str()),
            Some("m-2")
        );
        assert_eq!(
            resolve_target(&items, "tmpm-red").map(|s| s.id.as_str()),
            Some("m-1")
        );
    }
}
