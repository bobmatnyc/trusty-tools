//! Runtime "requires" dependency graph among [`super::stable_set`] members (#2036).
//!
//! Why: `tctl install trusty-mpm` used to install ONLY trusty-mpm, silently
//! skipping the runtime daemons it actually needs (trusty-memory for the
//! memory-palace MCP server, trusty-search for hybrid code search) — the
//! operator ended up with a partially-functional stack and no warning.
//! Encoding the "requires" edges as data lets [`super::stable_set::select_members_transitive`]
//! compute the closure once, in one place, instead of every call site
//! re-deriving it.
//!
//! What: A static edge table (`DEPENDENCY_EDGES`) plus pure graph functions:
//! [`direct_requires`] (one hop), [`transitive_closure`] (all hops, BFS), and
//! [`added_members`] (which crates were pulled in and by what). The edge table
//! is intentionally conservative — an edge is only added when a member spawns,
//! connects to, or otherwise requires another member's binary at runtime (not
//! merely a `Cargo.toml` library dependency, which cargo already resolves).
//! Two runtime edges are confirmed as of #2036:
//! - `trusty-mpm` → `trusty-memory` + `trusty-search`: both MCP servers are
//!   injected into every managed session by default
//!   (`crates/trusty-mpm/src/core/manifest/default.rs`), and their addresses
//!   are resolved via `crates/trusty-mpm/src/daemon/discover.rs`.
//! - `trusty-review` → `trusty-search` + `trusty-analyze`: the
//!   required-context preflight gate (#590,
//!   `crates/trusty-review/src/pipeline/context_gate.rs`) SKIPS the review
//!   entirely — "a review produced WITHOUT that context is actively harmful"
//!   — when either dependency is unreachable, unless the operator explicitly
//!   opts into a degraded run. This is a hard runtime requirement, not a soft
//!   nicety (contrast `trusty-review`'s separate, genuinely best-effort JIRA/
//!   Confluence/GitHub-Issues enrichment sources, which fail open and are NOT
//!   encoded here).
//!
//! No other stable-set member has a confirmed runtime process dependency on
//! another as of #2036. In particular: `trusty-console`'s `detect/*` connectors
//! probe search/memory/analyze/review/mpm and proxy to whichever are already
//! running, but console starts and serves fine with all of them absent (each
//! reports `Absent` gracefully) — that is service discovery, not a runtime
//! requirement, so no edge is encoded for it. `trusty-analyze` embeds
//! `trusty-review`'s pipeline as an in-process library
//! (`crates/trusty-analyze/src/mcp/review.rs`) rather than calling out to a
//! separate `trusty-review` process, so that is a `Cargo.toml`/library
//! relationship, not a process-level one — also not encoded. `tga` has no
//! references to any other stable-set member's runtime surface.
//!
//! Test: `tests` covers closure computation (multi-hop, no-op for a leaf,
//! idempotence when a dependency is named explicitly), the `added_members`
//! grouping, and an invariant test that every edge target appears strictly
//! earlier in [`super::stable_set::stable_set`] order than its dependent (so
//! filtering the master list by closure membership yields a valid topological
//! order without needing a separate sort).

use std::collections::BTreeSet;

/// Static "requires" edges: `(crate_name, [direct runtime dependencies])`.
///
/// Why: See module docs — kept conservative and evidence-based rather than
/// mirroring `Cargo.toml`.
///
/// What: `trusty-mpm` requires `trusty-memory` + `trusty-search` (both MCP
/// servers are injected into every session by default). `trusty-review`
/// requires `trusty-search` + `trusty-analyze` (the #590 required-context gate
/// skips the review when either is unreachable, absent an explicit
/// degraded-mode opt-in).
///
/// Test: `tests::mpm_requires_memory_and_search`,
/// `tests::review_requires_search_and_analyze`,
/// `tests::edges_precede_dependent_in_stable_set_order`.
const DEPENDENCY_EDGES: &[(&str, &[&str])] = &[
    ("trusty-mpm", &["trusty-memory", "trusty-search"]),
    ("trusty-review", &["trusty-search", "trusty-analyze"]),
];

/// One member added to a selection because an explicitly-requested member
/// (transitively) requires it.
///
/// Why: The CLI and picker need to tell the operator *why* extra members
/// appeared in the install set, not just silently expand it.
///
/// What: `crate_name` is the added member; `required_by` lists the
/// explicitly-requested crate names whose transitive dependency chain
/// includes it (sorted, deduplicated).
///
/// Test: `tests::added_members_reports_requester`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddedMember {
    /// The crate name that was pulled in.
    pub crate_name: String,
    /// Explicitly-requested crate names that (transitively) require it.
    pub required_by: Vec<String>,
}

/// The direct (one-hop) runtime dependencies of `crate_name`.
///
/// Why: The single lookup point into [`DEPENDENCY_EDGES`] so callers never
/// pattern-match the table directly.
///
/// What: Returns the declared dependency slice, or `&[]` when `crate_name` has
/// no entry (leaf or unknown crate).
///
/// Test: `tests::leaf_has_no_direct_requires`, `tests::mpm_requires_memory_and_search`.
pub fn direct_requires(crate_name: &str) -> &'static [&'static str] {
    DEPENDENCY_EDGES
        .iter()
        .find(|(c, _)| *c == crate_name)
        .map_or(&[], |(_, deps)| *deps)
}

/// Compute the transitive closure of `explicit` crate names over the graph.
///
/// Why: `select_members_transitive` needs the full set of crates to install —
/// everything explicitly requested plus everything they (transitively) require
/// — before it can filter the master ordered list.
///
/// What: BFS from every name in `explicit`, following [`direct_requires`] edges,
/// returning the union (including the explicit names themselves) as a
/// `BTreeSet` for stable, deduplicated iteration.
///
/// Test: `tests::transitive_closure_expands_mpm`,
/// `tests::transitive_closure_noop_for_leaf`,
/// `tests::transitive_closure_idempotent_when_dep_named_explicitly`.
pub fn transitive_closure(explicit: &[String]) -> BTreeSet<String> {
    let mut closure: BTreeSet<String> = explicit.iter().cloned().collect();
    let mut stack: Vec<String> = explicit.to_vec();
    while let Some(current) = stack.pop() {
        for dep in direct_requires(&current) {
            let dep = (*dep).to_owned();
            if closure.insert(dep.clone()) {
                stack.push(dep);
            }
        }
    }
    closure
}

/// Whether `from` transitively requires `to` (used to build `required_by`).
///
/// Why: `added_members` needs to know, for each pulled-in crate, which
/// explicitly-requested crates are responsible for pulling it in.
///
/// What: DFS from `from` over [`direct_requires`] edges; returns `true` if
/// `to` is reachable (including zero hops, i.e. `from == to`).
///
/// Test: Exercised indirectly via `tests::added_members_reports_requester`.
fn reaches(from: &str, to: &str) -> bool {
    let mut seen = BTreeSet::new();
    let mut stack = vec![from.to_owned()];
    while let Some(current) = stack.pop() {
        if current == to {
            return true;
        }
        if !seen.insert(current.clone()) {
            continue;
        }
        for dep in direct_requires(&current) {
            stack.push((*dep).to_owned());
        }
    }
    false
}

/// Describe which members of `closure` were added because of an explicit
/// request, and by whom.
///
/// Why: Powers the "adding trusty-memory, trusty-search (required by
/// trusty-mpm)" surfacing in the CLI and picker.
///
/// What: For every crate in `closure` that is NOT itself in `explicit`, finds
/// every explicitly-requested crate that transitively requires it and records
/// an [`AddedMember`]. Order follows `closure`'s (alphabetical) iteration.
///
/// Test: `tests::added_members_reports_requester`,
/// `tests::added_members_empty_when_nothing_added`.
pub fn added_members(explicit: &[String], closure: &BTreeSet<String>) -> Vec<AddedMember> {
    let mut added = Vec::new();
    for crate_name in closure {
        if explicit.iter().any(|e| e == crate_name) {
            continue;
        }
        let mut required_by: Vec<String> = explicit
            .iter()
            .filter(|e| reaches(e, crate_name))
            .cloned()
            .collect();
        required_by.sort();
        required_by.dedup();
        added.push(AddedMember {
            crate_name: crate_name.clone(),
            required_by,
        });
    }
    added
}

/// Render `added` as human-readable lines, grouped by identical requester sets.
///
/// Why: One line per distinct requester group reads better than one line per
/// added crate ("adding trusty-memory, trusty-search (required by
/// trusty-mpm)" instead of two separate lines).
///
/// What: Groups `added` by `required_by`, joins each group's crate names with
/// `", "`, and formats `"adding {crates} (required by {requesters})"` per
/// group. Groups are ordered by their sorted requester key for determinism.
///
/// Test: `tests::describe_added_groups_by_requester`,
/// `tests::describe_added_empty_is_empty`.
pub fn describe_added(added: &[AddedMember]) -> Vec<String> {
    use std::collections::BTreeMap;

    let mut groups: BTreeMap<Vec<String>, Vec<String>> = BTreeMap::new();
    for a in added {
        groups
            .entry(a.required_by.clone())
            .or_default()
            .push(a.crate_name.clone());
    }
    groups
        .into_iter()
        .map(|(required_by, crates)| {
            format!(
                "adding {} (required by {})",
                crates.join(", "),
                required_by.join(", ")
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::stable_set::stable_set;

    /// Why: Pins the one confirmed runtime edge so a future accidental table
    /// edit is caught.
    /// What: Asserts `direct_requires("trusty-mpm")` is exactly
    /// `[trusty-memory, trusty-search]`.
    /// Test: This is the test.
    #[test]
    fn mpm_requires_memory_and_search() {
        assert_eq!(
            direct_requires("trusty-mpm"),
            &["trusty-memory", "trusty-search"]
        );
    }

    /// Why: A crate with no table entry (leaf or unknown) must not panic or
    /// synthesize an edge.
    /// What: Asserts `direct_requires` returns `&[]` for a leaf and an unknown name.
    /// Test: This is the test.
    #[test]
    fn leaf_has_no_direct_requires() {
        assert!(direct_requires("trusty-search").is_empty());
        assert!(direct_requires("not-a-real-crate").is_empty());
    }

    /// Why: trusty-review's #590 required-context gate is a hard runtime
    /// dependency on search + analyze (it SKIPS the review outright when
    /// either is unreachable); pin the edge.
    /// What: Asserts `direct_requires("trusty-review")` is exactly
    /// `[trusty-search, trusty-analyze]`.
    /// Test: This is the test.
    #[test]
    fn review_requires_search_and_analyze() {
        assert_eq!(
            direct_requires("trusty-review"),
            &["trusty-search", "trusty-analyze"]
        );
    }

    /// Why: trusty-console's service-discovery probes (detect/*) are
    /// deliberately NOT a hard requires edge — console runs fine with every
    /// daemon absent — so this must stay a leaf even though it has detectors
    /// for every other member.
    /// What: Asserts `direct_requires("trusty-console")` is empty.
    /// Test: This is the test.
    #[test]
    fn console_service_discovery_is_not_a_requires_edge() {
        assert!(direct_requires("trusty-console").is_empty());
    }

    /// Why: The core transitive-expansion contract for #2036 — installing
    /// trusty-mpm must pull in its runtime deps.
    /// What: `transitive_closure(["trusty-mpm"])` includes trusty-mpm,
    /// trusty-memory, and trusty-search (and nothing else).
    /// Test: This is the test.
    #[test]
    fn transitive_closure_expands_mpm() {
        let closure = transitive_closure(&["trusty-mpm".to_owned()]);
        assert_eq!(
            closure,
            BTreeSet::from([
                "trusty-mpm".to_owned(),
                "trusty-memory".to_owned(),
                "trusty-search".to_owned(),
            ])
        );
    }

    /// Why: A leaf crate (no dependents) must expand to exactly itself.
    /// What: `transitive_closure(["tga"])` is `{"tga"}`.
    /// Test: This is the test.
    #[test]
    fn transitive_closure_noop_for_leaf() {
        let closure = transitive_closure(&["tga".to_owned()]);
        assert_eq!(closure, BTreeSet::from(["tga".to_owned()]));
    }

    /// Why: The #590 gate scenario — installing trusty-review alone must pull
    /// in both trusty-search and trusty-analyze.
    /// What: `transitive_closure(["trusty-review"])` includes all three
    /// crate_names (and nothing else).
    /// Test: This is the test.
    #[test]
    fn transitive_closure_expands_review() {
        let closure = transitive_closure(&["trusty-review".to_owned()]);
        assert_eq!(
            closure,
            BTreeSet::from([
                "trusty-review".to_owned(),
                "trusty-search".to_owned(),
                "trusty-analyze".to_owned(),
            ])
        );
    }

    /// Why: Requesting two dependents that share a dependency (trusty-mpm and
    /// trusty-review both require trusty-search) must union correctly rather
    /// than duplicating or dropping the shared dependency.
    /// What: `transitive_closure(["trusty-mpm", "trusty-review"])` is exactly
    /// the five-member union.
    /// Test: This is the test.
    #[test]
    fn transitive_closure_unions_shared_dependency() {
        let closure = transitive_closure(&["trusty-mpm".to_owned(), "trusty-review".to_owned()]);
        assert_eq!(
            closure,
            BTreeSet::from([
                "trusty-mpm".to_owned(),
                "trusty-memory".to_owned(),
                "trusty-search".to_owned(),
                "trusty-review".to_owned(),
                "trusty-analyze".to_owned(),
            ])
        );
    }

    /// Why: Naming a dependency explicitly alongside its dependent must not
    /// duplicate it or change the closure.
    /// What: `transitive_closure(["trusty-mpm", "trusty-memory"])` is still
    /// exactly `{trusty-mpm, trusty-memory, trusty-search}`.
    /// Test: This is the test.
    #[test]
    fn transitive_closure_idempotent_when_dep_named_explicitly() {
        let closure = transitive_closure(&["trusty-mpm".to_owned(), "trusty-memory".to_owned()]);
        assert_eq!(
            closure,
            BTreeSet::from([
                "trusty-mpm".to_owned(),
                "trusty-memory".to_owned(),
                "trusty-search".to_owned(),
            ])
        );
    }

    /// Why: `added_members` must attribute a pulled-in crate to the explicit
    /// request that caused it, not list every explicit crate blindly.
    /// What: Requesting only trusty-mpm reports trusty-memory and
    /// trusty-search as added, each `required_by: ["trusty-mpm"]`.
    /// Test: This is the test.
    #[test]
    fn added_members_reports_requester() {
        let explicit = vec!["trusty-mpm".to_owned()];
        let closure = transitive_closure(&explicit);
        let mut added = added_members(&explicit, &closure);
        added.sort_by(|a, b| a.crate_name.cmp(&b.crate_name));
        assert_eq!(
            added,
            vec![
                AddedMember {
                    crate_name: "trusty-memory".to_owned(),
                    required_by: vec!["trusty-mpm".to_owned()],
                },
                AddedMember {
                    crate_name: "trusty-search".to_owned(),
                    required_by: vec!["trusty-mpm".to_owned()],
                },
            ]
        );
    }

    /// Why: When nothing was pulled in (e.g. a leaf, or all deps named
    /// explicitly), `added_members` must be empty — no spurious announcements.
    /// What: Asserts an empty result for a leaf-only request.
    /// Test: This is the test.
    #[test]
    fn added_members_empty_when_nothing_added() {
        let explicit = vec!["tga".to_owned()];
        let closure = transitive_closure(&explicit);
        assert!(added_members(&explicit, &closure).is_empty());
    }

    /// Why: When two explicitly-requested members share a dependency (mpm and
    /// review both require trusty-search), the shared dependency's
    /// `required_by` must list BOTH requesters, not just the first one found.
    /// What: Requesting `["trusty-mpm", "trusty-review"]`; asserts
    /// trusty-search's `required_by` is `["trusty-mpm", "trusty-review"]`.
    /// Test: This is the test.
    #[test]
    fn added_members_reports_all_requesters_for_shared_dependency() {
        let explicit = vec!["trusty-mpm".to_owned(), "trusty-review".to_owned()];
        let closure = transitive_closure(&explicit);
        let added = added_members(&explicit, &closure);
        let search = added
            .iter()
            .find(|a| a.crate_name == "trusty-search")
            .expect("trusty-search reported as added");
        assert_eq!(
            search.required_by,
            vec!["trusty-mpm".to_owned(), "trusty-review".to_owned()]
        );
    }

    /// Why: The CLI/picker message format is a load-bearing contract (#2036
    /// acceptance criteria); pin its exact shape.
    /// What: Asserts the grouped message text for the mpm case.
    /// Test: This is the test.
    #[test]
    fn describe_added_groups_by_requester() {
        let explicit = vec!["trusty-mpm".to_owned()];
        let closure = transitive_closure(&explicit);
        let added = added_members(&explicit, &closure);
        let lines = describe_added(&added);
        assert_eq!(
            lines,
            vec!["adding trusty-memory, trusty-search (required by trusty-mpm)".to_owned()]
        );
    }

    /// Why: No added members must produce no message lines.
    /// What: Asserts `describe_added(&[])` is empty.
    /// Test: This is the test.
    #[test]
    fn describe_added_empty_is_empty() {
        assert!(describe_added(&[]).is_empty());
    }

    /// Why: `select_members_transitive` relies on filtering the master
    /// `stable_set()` order to already be a valid topological order; that only
    /// holds if every edge target appears strictly earlier in the list than
    /// its dependent. This test guards the invariant so a future edge addition
    /// that violates it fails loudly instead of silently mis-ordering installs.
    /// What: For every `(dependent, deps)` in `DEPENDENCY_EDGES`, asserts each
    /// dep's index in `stable_set()` is less than the dependent's index.
    /// Test: This is the test.
    #[test]
    fn edges_precede_dependent_in_stable_set_order() {
        let order: Vec<String> = stable_set().into_iter().map(|m| m.crate_name).collect();
        let index_of = |name: &str| {
            order
                .iter()
                .position(|n| n == name)
                .unwrap_or_else(|| panic!("{name} not in stable_set()"))
        };
        for (dependent, deps) in DEPENDENCY_EDGES {
            let dependent_idx = index_of(dependent);
            for dep in *deps {
                assert!(
                    index_of(dep) < dependent_idx,
                    "{dep} must precede {dependent} in stable_set() order"
                );
            }
        }
    }
}
