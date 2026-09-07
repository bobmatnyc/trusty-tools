//! Bundle keywords for `tctl install` (#4714).
//!
//! Why: `tctl install` accepts only exact member names, so an operator who
//! wants the always-on substrate has to already know it is spelled
//! `trusty-memory trusty-search`. The vocabulary for those groupings already
//! exists elsewhere in the crate — `up::manifest`'s `BootStage::Core` names
//! memory+search, and `stable_set`'s `required` flag names the members a
//! from-scratch install must always bring up — but neither is reachable from
//! the install verb. This module exposes them as typed keywords.
//!
//! What: A keyword table ([`BUNDLE_KEYWORDS`]) plus [`bundle_members`], which
//! DERIVES each keyword's membership from the existing tables rather than
//! restating it, and [`expand_bundles`], which rewrites a caller's name list
//! into explicit crate names before
//! [`super::stable_set::select_members_transitive`] resolves it. Expansion is
//! pure and order-preserving; the transitive closure and the topological order
//! stay entirely the resolver's job.
//!
//! Deriving rather than hard-coding is what keeps the keywords honest: adding
//! a member to `BootStage::Core`, or flipping a member to `required: true`,
//! moves the matching bundle with it. `stable_set`'s own note to future
//! editors — that a later lane inserts `trusty-agents` "as REQUIRED" — means
//! that lane extends `agents` with no edit here.
//!
//! Test: `tests` pins both keywords' derived membership, the identity of a
//! non-keyword name, order preservation, deduplication, and the
//! [`keywords_hint`] string the unknown-member error carries.

use super::stable_set::stable_set;
use super::up::manifest::{default_manifest, members_in_stage, BootStage};

/// Every bundle keyword `tctl install` accepts, alphabetically.
///
/// Why: One list, read by [`bundle_members`]'s dispatch, by [`keywords_hint`]
/// (so the unknown-member error can never name a stale set), and by the
/// `--help` text's test. A keyword added here without a `bundle_members` arm
/// fails `tests::every_keyword_resolves`.
///
/// What: `agents` and `core` — see [`bundle_members`] for what each expands to.
///
/// Test: `tests::every_keyword_resolves`, `tests::keywords_hint_names_all`.
pub const BUNDLE_KEYWORDS: &[&str] = &["agents", "core"];

/// Expand one bundle keyword to the explicit crate names it stands for.
///
/// Why: `tctl install core` should mean exactly what `tctl up`'s STAGE 1
/// means, and `tctl install agents` exactly what a verified from-scratch
/// install must be able to bring up. Both are already declared as data
/// elsewhere; re-typing either here would let the two drift silently.
///
/// What: `core` is every `up::manifest` member in [`BootStage::Core`] — today
/// `trusty-memory`, `trusty-search`. `agents` is every [`stable_set`] member
/// with `required: true` — today `trusty-search`, `trusty-memory`,
/// `trusty-review`, `trusty-mpm`, the set whose closure is the agent
/// orchestrator plus the daemons it and the review gate need. Returns `None`
/// for any name that is not a keyword, which is how [`expand_bundles`] tells a
/// keyword from a member name. The returned names are NOT closed over the
/// dependency graph — `select_members_transitive` does that.
///
/// Test: `tests::core_is_the_boot_stage_core_members`,
/// `tests::agents_is_the_required_members`, `tests::unknown_keyword_is_none`.
pub fn bundle_members(keyword: &str) -> Option<Vec<String>> {
    match keyword {
        "core" => Some(
            members_in_stage(&default_manifest(), BootStage::Core)
                .into_iter()
                .map(|m| m.id.clone())
                .collect(),
        ),
        "agents" => Some(
            stable_set()
                .into_iter()
                .filter(|m| m.required)
                .map(|m| m.crate_name)
                .collect(),
        ),
        _ => None,
    }
}

/// Rewrite a caller's name list, replacing every bundle keyword with the crate
/// names it stands for.
///
/// Why: The install verb's resolver only understands member names. Expanding
/// first — rather than teaching the resolver about keywords — keeps the
/// keyword vocabulary in one place and leaves `select_members_transitive`'s
/// unknown-name contract untouched, so a typo still errors exactly as before.
///
/// What: Maps each name through [`bundle_members`], splicing a keyword's
/// members in at its position and passing any other name through unchanged.
/// Duplicates are dropped, keeping first occurrence, so `tctl install core
/// trusty-search` names trusty-search once. An empty input stays empty, which
/// is how `install::run` keeps meaning "the full stable set".
///
/// Test: `tests::expands_core`, `tests::passes_member_names_through`,
/// `tests::dedups_overlapping_bundle_and_member`,
/// `tests::preserves_order_across_a_mixed_list`, `tests::empty_stays_empty`.
pub fn expand_bundles(names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |name: String| {
        if !out.contains(&name) {
            out.push(name);
        }
    };
    for name in names {
        match bundle_members(name) {
            Some(members) => members.into_iter().for_each(&mut push),
            None => push(name.clone()),
        }
    }
    out
}

/// The parenthetical the unknown-member error appends, naming every keyword.
///
/// Why: Before #4714 a mistyped `tctl install cores` said only "unknown
/// member(s): cores", leaving the operator no way to discover that `core` was
/// the word. The hint reads off [`BUNDLE_KEYWORDS`] so it cannot go stale.
///
/// What: `"bundle keywords: agents, core"`.
///
/// Test: `tests::keywords_hint_names_all`.
pub fn keywords_hint() -> String {
    format!("bundle keywords: {}", BUNDLE_KEYWORDS.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    /// Why: `core` must stay the same set `tctl up` boots as STAGE 1; a silent
    /// divergence would make the two verbs disagree about what "core" is.
    /// What: Pins the derived membership against the manifest's own filter.
    /// Test: This is the test.
    #[test]
    fn core_is_the_boot_stage_core_members() {
        assert_eq!(
            bundle_members("core"),
            Some(owned(&["trusty-memory", "trusty-search"]))
        );
    }

    /// Why: `agents` is derived from `stable_set`'s `required` flag, so the
    /// pin doubles as a guard on that classification changing unnoticed.
    /// What: Pins the derived membership in stable-set order.
    /// Test: This is the test.
    #[test]
    fn agents_is_the_required_members() {
        assert_eq!(
            bundle_members("agents"),
            Some(owned(&[
                "trusty-search",
                "trusty-memory",
                "trusty-review",
                "trusty-mpm",
            ]))
        );
    }

    /// Why: A member name must NOT be treated as a keyword, or expansion would
    /// shadow the stable set.
    /// What: Asserts `None` for a member name and for a bogus name.
    /// Test: This is the test.
    #[test]
    fn unknown_keyword_is_none() {
        assert_eq!(bundle_members("trusty-search"), None);
        assert_eq!(bundle_members("cores"), None);
    }

    /// Why: Every advertised keyword must resolve, or the `--help` text and
    /// the error hint would promise a word the code rejects.
    /// What: Asserts each [`BUNDLE_KEYWORDS`] entry yields a non-empty set.
    /// Test: This is the test.
    #[test]
    fn every_keyword_resolves() {
        for kw in BUNDLE_KEYWORDS {
            let members = bundle_members(kw).unwrap_or_else(|| panic!("{kw} must resolve"));
            assert!(
                !members.is_empty(),
                "{kw} must expand to at least one member"
            );
        }
    }

    /// Why: The expansion is what `install::run` actually calls.
    /// What: `["core"]` becomes the two core member names.
    /// Test: This is the test.
    #[test]
    fn expands_core() {
        assert_eq!(
            expand_bundles(&owned(&["core"])),
            owned(&["trusty-memory", "trusty-search"])
        );
    }

    /// Why: Naming members directly must keep working unchanged.
    /// What: A non-keyword list passes through identically.
    /// Test: This is the test.
    #[test]
    fn passes_member_names_through() {
        let names = owned(&["trusty-mpm", "not-a-real-tool"]);
        assert_eq!(expand_bundles(&names), names);
    }

    /// Why: `tctl install core trusty-search` must not name trusty-search
    /// twice — a duplicate would be resolved twice by the picker-free path.
    /// What: Asserts the overlap collapses, first occurrence winning.
    /// Test: This is the test.
    #[test]
    fn dedups_overlapping_bundle_and_member() {
        assert_eq!(
            expand_bundles(&owned(&["core", "trusty-search"])),
            owned(&["trusty-memory", "trusty-search"])
        );
    }

    /// Why: Expansion happens in place, so a mixed list must keep the caller's
    /// order — the resolver re-sorts topologically afterwards, but a reordering
    /// here would scramble the unknown-name diagnostics.
    /// What: A member, a keyword, then another member.
    /// Test: This is the test.
    #[test]
    fn preserves_order_across_a_mixed_list() {
        assert_eq!(
            expand_bundles(&owned(&["tga", "core", "trusty-mpm"])),
            owned(&["tga", "trusty-memory", "trusty-search", "trusty-mpm"])
        );
    }

    /// Why: `install::run` treats an empty list as "the full stable set";
    /// expansion must not invent members for it.
    /// What: Asserts the empty input stays empty.
    /// Test: This is the test.
    #[test]
    fn empty_stays_empty() {
        assert!(expand_bundles(&[]).is_empty());
    }

    /// Why: The error hint is the operator's only discovery path from a typo.
    /// What: Asserts it names every keyword.
    /// Test: This is the test.
    #[test]
    fn keywords_hint_names_all() {
        let hint = keywords_hint();
        for kw in BUNDLE_KEYWORDS {
            assert!(hint.contains(kw), "hint must name {kw}: {hint}");
        }
    }
}
