//! Dead scope-pattern detection for agent `[tools].scopes` (#3987, option C).
//!
//! Why: `ScopePattern` matching fails CLOSED and fails SILENTLY. An agent
//! that declares a pattern no tool can ever advertise — the base
//! `assistant`'s `google.read` is the motivating example — does not get an
//! error, a `404`, or even a log line: `agent_can_use` simply returns
//! `false` for every scoped tool and `filter_persona_tool_names` drops the
//! whole surface before the model sees the registry. The observable result
//! is indistinguishable from "this agent has no tools", which is exactly why
//! #3938 and #3987 both stayed invisible for so long and cost real debugging
//! time. This module makes that condition *nameable* so it can be logged at
//! registry-build time and rendered in the config-introspection surface.
//!
//! What: [`reachable_scopes`] assembles the set of dotted scopes a pattern
//! could plausibly match; [`dead_scope_patterns`] returns one
//! [`DeadScopePattern`] per declared pattern that matches none of them, each
//! carrying the *nearest reachable* scopes in the same namespace so the
//! diagnostic is actionable rather than merely alarming.
//!
//! ## No false positives: the namespace guard
//!
//! "Reachable" is deliberately the UNION of two sources:
//!
//! 1. the scopes the live registry actually discovered this run
//!    (`ToolRegistry::tool_scopes()`), and
//! 2. the closed compile-time vocabulary this build can ever emit for
//!    namespaces it owns — today just `google`, via
//!    [`super::google_scope::known_dotted_scopes`].
//!
//! Source 2 is what keeps a `gworkspace` daemon being *down* from making
//! every `google.*` grant look broken. But the union alone is still not
//! enough: `memory.read` / `search.read` come from endpoints whose
//! vocabulary this crate does not know statically, so with those endpoints
//! offline they would match nothing and be falsely reported dead.
//!
//! Hence the namespace guard: a pattern is only JUDGED when its first
//! segment (`google` in `google.read`) appears in the reachable set at all.
//! An unrecognised namespace is treated as "not enough information",
//! never as "dead". The diagnostic therefore only ever fires on a namespace
//! whose vocabulary is fully known — where a non-match is a genuine
//! configuration defect and not an artefact of what is running.
//!
//! ## Warning today, hard error later (deliberate sequencing)
//!
//! This is intentionally NOT a load-time error yet. At the time of writing,
//! the bundled base `assistant` template itself declares the dead
//! `google.read` pattern (`.trusty-agents/agents/assistant/agent.toml`), and
//! every `extends = "assistant"` overlay inherits it by union — so failing
//! closed here would break EVERY assistant load, including the default
//! persona, on the very commit that added the check. #3987's option B
//! removes that pattern from the shipped base; once it has landed and no
//! bundled agent trips this diagnostic, escalating
//! [`dead_scope_patterns`]'s callers from `warn!` to a hard load error is
//! the intended follow-up. Do not escalate before then.
//!
//! Test: the `tests` module below, plus
//! `ctrl::pm_task::dispatch::persona_tests::base_assistant_google_read_is_a_dead_scope_pattern`
//! which pins the diagnostic against the SHIPPED base template.

use std::collections::BTreeSet;

use super::google_scope::known_dotted_scopes;
use super::scope::{Scope, ScopePattern};

/// How many nearest-reachable suggestions a diagnostic carries.
///
/// Why: the whole point is an actionable log line; dumping an unbounded
/// scope list into a `warn!` makes it noise again. Eight is enough to name
/// every `google` family with room to spare.
const MAX_SUGGESTIONS: usize = 8;

/// One declared pattern that can match nothing reachable.
///
/// Why: the pattern alone is not actionable — an operator reading
/// `google.read matches nothing` still has to go find what *would* work.
/// `nearest` answers that in the same breath.
/// What: `pattern` is the agent's declared string verbatim; `nearest` is the
/// reachable scopes sharing its namespace, sorted, capped at
/// [`MAX_SUGGESTIONS`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadScopePattern {
    pub pattern: String,
    pub nearest: Vec<String>,
}

impl DeadScopePattern {
    /// Human-readable one-liner: `` `google.read` (nearest reachable: …) ``.
    ///
    /// Why: used both by the `warn!` at the dispatch call site and by the
    /// API route's `reason` field, so the wording cannot drift between the
    /// log an operator greps and the panel a user reads.
    pub fn describe(&self) -> String {
        if self.nearest.is_empty() {
            format!(
                "`{}` (no reachable scope shares its namespace)",
                self.pattern
            )
        } else {
            format!(
                "`{}` (nearest reachable: {})",
                self.pattern,
                self.nearest.join(", ")
            )
        }
    }
}

/// The namespace (first dotted segment) a pattern or scope belongs to.
///
/// Why: the namespace guard described in the module doc needs one rule
/// applied identically to patterns and scopes, so `google.*`, `google.read`
/// and `google.gmail.write` all resolve to `google`.
/// What: everything before the first `.`, or the whole string when it has
/// none. A pattern that is bare `*` has no namespace and yields `None`.
fn namespace(s: &str) -> Option<&str> {
    if s.is_empty() || s.starts_with('*') {
        return None;
    }
    Some(s.split('.').next().unwrap_or(s))
}

/// Assemble the set of dotted scopes an agent pattern could match.
///
/// Why/What: see the module doc — the union of what the live registry
/// discovered this run with the closed vocabulary this build can emit, so
/// the diagnostic is stable whether or not `gworkspace` is up.
/// Test: `reachable_includes_static_google_vocabulary_without_a_live_registry`.
pub fn reachable_scopes<I, S>(live_tool_scopes: I) -> BTreeSet<Scope>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out: BTreeSet<Scope> = live_tool_scopes
        .into_iter()
        // `method_to_tool` writes an EMPTY scope for a tool whose scope could
        // not be derived; that is a discovery defect with its own warning and
        // must not be admitted here as if it were a reachable vocabulary entry.
        .filter(|s| !s.as_ref().trim().is_empty())
        .map(|s| Scope::new(s.as_ref().to_string()))
        .collect();
    out.extend(known_dotted_scopes().into_iter().map(Scope::new));
    out
}

/// Report every declared pattern that can match no reachable scope.
///
/// Why: #3987 option C — make silent total denial loud. This is the pure,
/// side-effect-free core so the property can be unit-tested without a
/// registry, a daemon, or an LLM; the callers own the `warn!` and the JSON.
/// What: for each pattern, skip it unless its namespace appears somewhere in
/// `reachable` (the namespace guard — see the module doc), then report it
/// when no reachable scope matches. Order-preserving over `patterns`.
/// Test: `dead_pattern_is_detected`, `live_wildcard_pattern_is_not_flagged`,
/// `unknown_namespace_is_never_judged`,
/// `namespace_known_only_from_live_scopes_is_judged`,
/// `middle_wildcard_pattern_is_flagged`.
pub fn dead_scope_patterns(
    patterns: &[ScopePattern],
    reachable: &BTreeSet<Scope>,
) -> Vec<DeadScopePattern> {
    let mut out = Vec::new();
    for pattern in patterns {
        let Some(ns) = namespace(pattern.as_str()) else {
            continue;
        };
        let same_namespace: Vec<&Scope> = reachable
            .iter()
            .filter(|s| namespace(s.as_str()) == Some(ns))
            .collect();
        // Namespace guard: nothing reachable is in this namespace, so we do
        // not know its vocabulary and cannot honestly call the pattern dead.
        if same_namespace.is_empty() {
            continue;
        }
        if same_namespace.iter().any(|s| pattern.matches(s)) {
            continue;
        }
        out.push(DeadScopePattern {
            pattern: pattern.as_str().to_string(),
            nearest: same_namespace
                .iter()
                .take(MAX_SUGGESTIONS)
                .map(|s| s.as_str().to_string())
                .collect(),
        });
    }
    out
}

/// Emit the loud, actionable registry-build-time warning for `dead`.
///
/// Why: every call site should produce the SAME log line — an operator who
/// learns to grep one string must find them all — and the "an agent with a
/// dead grant looks exactly like an agent with no tools" explanation is
/// worth stating in the log itself, since that confusion is the entire
/// reason the defect class survived two issues. No-op when `dead` is empty
/// so healthy agents cost nothing.
/// What: one `warn!` naming the agent, the dead patterns and their nearest
/// reachable alternatives.
/// Test: `warn_dead_scope_patterns_is_a_noop_when_healthy` (behavioural
/// smoke — the log content itself is asserted via `describe`).
pub fn warn_dead_scope_patterns(agent: &str, dead: &[DeadScopePattern]) {
    if dead.is_empty() {
        return;
    }
    let described: Vec<String> = dead.iter().map(DeadScopePattern::describe).collect();
    tracing::warn!(
        agent = %agent,
        dead_scope_patterns = ?described,
        "DEAD SCOPE GRANT (#3987): agent '{agent}' declares [tools].scopes \
         pattern(s) that match NO scope any reachable tool advertises, so every \
         scoped tool they were meant to grant is silently denied at dispatch — \
         which looks identical to 'this agent has no tools'. Dead: {}. Replace \
         each with one of its nearest reachable scopes (or a trailing-wildcard \
         family pattern such as `google.gmail.*`); middle wildcards like \
         `google.*.read` are not supported and always match nothing.",
        described.join("; "),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patterns(v: &[&str]) -> Vec<ScopePattern> {
        v.iter().map(|s| ScopePattern::new(*s)).collect()
    }

    fn reachable(v: &[&str]) -> BTreeSet<Scope> {
        reachable_scopes(v.iter().map(|s| s.to_string()))
    }

    /// The motivating defect: `google.read` is an exact pattern and the
    /// vocabulary only ever emits `google.<family>.<access>`.
    #[test]
    fn dead_pattern_is_detected() {
        let dead = dead_scope_patterns(&patterns(&["google.read"]), &reachable(&[]));
        assert_eq!(dead.len(), 1, "{dead:?}");
        assert_eq!(dead[0].pattern, "google.read");
        assert!(
            dead[0].nearest.iter().any(|s| s == "google.gmail.write"),
            "the diagnostic must be actionable: {:?}",
            dead[0].nearest
        );
        assert!(dead[0].describe().contains("nearest reachable"));
    }

    /// A live family pattern — the shape option B will use — must never be
    /// flagged, with or without a live registry.
    #[test]
    fn live_wildcard_pattern_is_not_flagged() {
        assert!(
            dead_scope_patterns(
                &patterns(&["google.gmail.*", "google.tasks.*", "google.*"]),
                &reachable(&[])
            )
            .is_empty()
        );
    }

    #[test]
    fn live_exact_pattern_is_not_flagged() {
        assert!(
            dead_scope_patterns(
                &patterns(&["google.gmail.write", "google.accounts.read"]),
                &reachable(&[])
            )
            .is_empty()
        );
    }

    /// The namespace guard. `memory.read` is served by an endpoint whose
    /// vocabulary this crate does not know statically; with that endpoint
    /// offline it must be left alone, NOT reported dead.
    #[test]
    fn unknown_namespace_is_never_judged() {
        assert!(
            dead_scope_patterns(
                &patterns(&[
                    "memory.read",
                    "memory.write",
                    "search.read",
                    "totally.made.up"
                ]),
                &reachable(&[])
            )
            .is_empty()
        );
    }

    /// …but once the live registry HAS shown us that namespace, a pattern in
    /// it that still matches nothing is a genuine defect and is reported.
    #[test]
    fn namespace_known_only_from_live_scopes_is_judged() {
        let live = reachable(&["memory.read", "memory.write"]);
        assert!(dead_scope_patterns(&patterns(&["memory.read"]), &live).is_empty());
        let dead = dead_scope_patterns(&patterns(&["memory.recall"]), &live);
        assert_eq!(dead.len(), 1);
        assert_eq!(dead[0].nearest, vec!["memory.read", "memory.write"]);
    }

    /// `google.*.read` is the pattern someone reaches for when told
    /// "read-only Google"; `ScopePattern` rejects middle wildcards by design,
    /// so it matches nothing and must be flagged rather than silently denied.
    #[test]
    fn middle_wildcard_pattern_is_flagged() {
        let dead = dead_scope_patterns(&patterns(&["google.*.read"]), &reachable(&[]));
        assert_eq!(dead.len(), 1);
        assert_eq!(dead[0].pattern, "google.*.read");
    }

    /// A pattern with no namespace at all (bare `*`) is not judged — there is
    /// no vocabulary to judge it against.
    #[test]
    fn namespaceless_pattern_is_not_judged() {
        assert!(dead_scope_patterns(&patterns(&["*", ""]), &reachable(&[])).is_empty());
    }

    /// Reachability must not depend on a live `gworkspace` endpoint.
    #[test]
    fn reachable_includes_static_google_vocabulary_without_a_live_registry() {
        let r = reachable(&[]);
        assert!(r.contains(&Scope::new("google.gmail.write")));
        assert!(r.contains(&Scope::new("google.accounts.read")));
        assert!(!r.contains(&Scope::new("google.read")));
    }

    /// An undeducible scope is written as `""` by discovery; admitting it
    /// would create a bogus empty-namespace entry.
    #[test]
    fn reachable_drops_empty_scope_strings() {
        let r = reachable(&["", "  ", "memory.read"]);
        assert!(r.contains(&Scope::new("memory.read")));
        assert!(!r.contains(&Scope::new("")));
    }

    #[test]
    fn suggestions_are_capped() {
        let many: Vec<String> = (0..30).map(|i| format!("zz.scope{i:02}")).collect();
        let set = reachable_scopes(many);
        let dead = dead_scope_patterns(&patterns(&["zz.nope"]), &set);
        assert_eq!(dead.len(), 1);
        assert_eq!(dead[0].nearest.len(), MAX_SUGGESTIONS);
    }

    #[test]
    fn warn_dead_scope_patterns_is_a_noop_when_healthy() {
        warn_dead_scope_patterns("assistant", &[]);
    }
}
