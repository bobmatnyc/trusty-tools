//! Normalize a claude-mpm agent's declared domain onto this crate's coarse
//! role vocabulary (#4502).
//!
//! Why: `claude_mpm_loader::to_agent_config` hardcoded `role = "agent"`, so a
//! claude-mpm agent's real domain was discarded at load time and it matched
//! nothing in `runtime::tool_registry::ASSISTANT_ALLOWED_DELEGATE_ROLES`.
//! Passing the declared value through VERBATIM is not the fix and would be a
//! bug in the opposite direction: the two vocabularies do not line up, so a
//! pass-through leaves most agents just as ineligible while quietly making
//! `role` — a load-bearing security discriminator that selects the
//! tool-registry branch in `build_registry_for_agent` — an arbitrary string
//! copied from an untrusted-ish file. This module is the translation layer,
//! kept separate from the loader so the mapping is reviewable as data and the
//! fail-closed property can be pinned without parsing a file.
//!
//! Two vocabularies are read, because the artifacts genuinely carry two:
//!
//! - `agent_type:` is what trusty-mpm's composer EMITS into the deployed
//!   `.claude/agents/*.md` artifacts this loader actually reads. It is the
//!   key that carries the domain in practice.
//! - `role:` appears in trusty-mpm's SOURCE assets
//!   (`crates/trusty-mpm/src/assets/agents/*.md`) and in hand-authored agent
//!   files. It is also the key #4495's shared-frontmatter work projects, so
//!   reading it here means that work inherits this normalization instead of
//!   re-deriving a second, divergent one.
//!
//! What: [`normalize_role`] — an EXPLICIT table lookup, plus [`UNMAPPED_ROLE`],
//! the sentinel every unrecognized value lands on. Deliberately NOT a
//! suffix/prefix rule (`*-engineer` -> `engineer`): a pattern rule is a
//! default-ALLOW in disguise — it would admit any future `<anything>-engineer`
//! sight unseen — whereas a table can only ever admit what a reviewer wrote
//! into it.
//!
//! This module grants nothing on its own. Role eligibility is a coarse
//! PRE-filter; a delegation must additionally clear the per-agent
//! `[subagents].delegate_allowed` whitelist AND the server-owned
//! `agents::delegation::ASSISTANT_REACHABLE_SUBAGENTS` name floor, both
//! deny-by-default and neither touched here. See
//! `claude_mpm_normalization_grants_no_new_reachable_target`.
//! Test: this module's `tests` submodule.

/// The role a claude-mpm agent gets when its declared domain is absent,
/// blank, or not in [`ROLE_MAP`] — the fail-closed sentinel.
///
/// Why: an unrecognized value must NOT become an allowlisted role. Keeping
/// the pre-#4502 literal (`"agent"`) as the sentinel makes the fail-closed
/// path byte-identical to today's behavior for every agent this module cannot
/// confidently classify, so the normalization can only ever move a KNOWN
/// value, never invent eligibility for an unknown one.
/// What: the string `"agent"`. It is not a member of
/// `runtime::tool_registry::ASSISTANT_ALLOWED_DELEGATE_ROLES`, and
/// `unmapped_sentinel_is_not_an_allowlisted_role` is the test that keeps it
/// that way if that constant is ever widened.
/// Test: `unmapped_sentinel_is_not_an_allowlisted_role`,
/// `unrecognized_or_absent_role_falls_back_to_the_sentinel`.
pub(crate) const UNMAPPED_ROLE: &str = "agent";

/// The declared-domain -> coarse-role table, derived from the REAL vocabulary
/// observed in trusty-mpm's source assets and its deployed artifacts.
///
/// Why: built from data, not from guesses. Every left-hand value below was
/// observed in `crates/trusty-mpm/src/assets/agents/*.md` (`role:`) or in a
/// composer-emitted `.claude/agents/*.md` artifact (`agent_type:`). Values
/// that were observed but are NOT here are omitted DELIBERATELY, because
/// mapping them would be a guess about a security-relevant classification:
///
/// - `security`, `version-control`, `code-analyzer`, `memory-manager`,
///   `mpm-agent-manager`, `mpm-skills-manager` — real specialists with no
///   counterpart in the coarse vocabulary. Inventing one (`security` -> `qa`?
///   -> `researcher`?) is a judgement call an owner should make, not a
///   translation.
/// - `base`, `base-engineer`, `base-ops`, `base-qa`, `base-research` —
///   composition FRAGMENTS that are appended into other agents, never
///   dispatched. These must stay ineligible; that is correct, not a gap.
/// - `universal`, `system`, `trusty-mpm`, and the literal
///   `engineer|qa|ops|universal|documentation` template placeholder — not
///   domains at all.
///
/// What: exact-match pairs, applied after trimming and lowercasing. Most
/// entries are IDENTITY mappings — the coarse word is already what the
/// artifact declares — which is the point: the table's job is to be explicit
/// about what is admitted, not to be clever. The two non-identity entries are
/// `research` -> `researcher` (the same domain under this crate's spelling,
/// which `research-agent.toml` uses) and `data-engineer` -> `engineer` (an
/// engineer that writes pipeline code; listed as a value, not derived from
/// the `-engineer` suffix, precisely so a future `foo-engineer` is not
/// admitted sight unseen).
/// Test: `every_mapped_target_is_an_allowlisted_role`,
/// `observed_trusty_mpm_vocabulary_maps_as_documented`.
const ROLE_MAP: &[(&str, &str)] = &[
    ("engineer", "engineer"),
    ("data-engineer", "engineer"),
    ("qa", "qa"),
    ("ops", "ops"),
    ("documentation", "documentation"),
    ("ticketing", "ticketing"),
    ("research", "researcher"),
    ("researcher", "researcher"),
    ("planner", "planner"),
];

/// Normalize a claude-mpm agent's declared domain onto the coarse role
/// vocabulary, fail-closed.
///
/// Why: THE single translation point. `role` selects the tool-registry branch
/// in `runtime::tool_registry::build_registry_for_agent` and is checked
/// against `ASSISTANT_ALLOWED_DELEGATE_ROLES` at every delegation, so the
/// value a `.md` file contributes to it must pass through a reviewed table
/// rather than reaching those gates verbatim.
/// What: prefers `role` (the source-asset and hand-authored spelling, also
/// what #4495's shared reader projects) and falls back to `agent_type` (what
/// trusty-mpm's composer emits into the deployed artifacts) — first non-blank
/// wins, so a file carrying both is read the way its author most likely meant.
/// The winning value is trimmed and lowercased, then looked up in
/// [`ROLE_MAP`]. Anything absent, blank, or unlisted yields [`UNMAPPED_ROLE`].
/// Note the fallback ORDER is a preference, not a union: a file declaring an
/// unmappable `role` does NOT get a second chance through `agent_type`,
/// because a declared-but-unrecognized domain is exactly the case that must
/// fail closed rather than search for something that matches.
/// Test: `role_wins_over_agent_type`, `agent_type_is_the_deployed_fallback`,
/// `unrecognized_or_absent_role_falls_back_to_the_sentinel`,
/// `declared_but_unmappable_role_does_not_fall_through_to_agent_type`,
/// `normalization_is_case_and_whitespace_insensitive`.
pub(crate) fn normalize_role(role: Option<&str>, agent_type: Option<&str>) -> String {
    let declared = [role, agent_type]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|v| !v.is_empty());
    let Some(declared) = declared else {
        return UNMAPPED_ROLE.to_string();
    };
    let key = declared.to_ascii_lowercase();
    ROLE_MAP
        .iter()
        .find(|(from, _)| *from == key)
        .map(|(_, to)| (*to).to_string())
        .unwrap_or_else(|| UNMAPPED_ROLE.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::tool_registry::ASSISTANT_ALLOWED_DELEGATE_ROLES;

    /// The property the whole module exists to guarantee: the fallback can
    /// never be an allowlisted role. If `ASSISTANT_ALLOWED_DELEGATE_ROLES` is
    /// ever widened to include `"agent"`, this fails and the sentinel must
    /// change — silently turning every unmappable claude-mpm agent eligible is
    /// the failure this test exists to prevent.
    #[test]
    fn unmapped_sentinel_is_not_an_allowlisted_role() {
        assert!(
            !ASSISTANT_ALLOWED_DELEGATE_ROLES.contains(&UNMAPPED_ROLE),
            "the fail-closed sentinel must never be role-eligible"
        );
    }

    /// The other half: everything the table CAN produce is a real member of
    /// the coarse vocabulary. A typo on a right-hand side would otherwise
    /// produce a role that is neither eligible nor the sentinel — ineligible
    /// by accident rather than by design, which is a different (and harder to
    /// notice) bug than being ineligible on purpose.
    #[test]
    fn every_mapped_target_is_an_allowlisted_role() {
        for (from, to) in ROLE_MAP {
            assert!(
                ASSISTANT_ALLOWED_DELEGATE_ROLES.contains(to),
                "{from:?} maps to {to:?}, which is not in the coarse vocabulary"
            );
        }
    }

    /// The mapping, pinned against the vocabulary actually observed in
    /// trusty-mpm's assets and deployed artifacts — including, critically, the
    /// values that must NOT map. A future edit that quietly admits `security`
    /// or a `base-*` composition fragment fails here.
    #[test]
    fn observed_trusty_mpm_vocabulary_maps_as_documented() {
        // Mapped: the coarse words the artifacts already use, plus the two
        // reviewed translations.
        for (declared, expected) in [
            ("engineer", "engineer"),
            ("data-engineer", "engineer"),
            ("qa", "qa"),
            ("ops", "ops"),
            ("documentation", "documentation"),
            ("ticketing", "ticketing"),
            ("research", "researcher"),
        ] {
            assert_eq!(
                normalize_role(Some(declared), None),
                expected,
                "{declared:?} must normalize to {expected:?}"
            );
        }
        // Unmapped, on purpose. Specialists with no coarse counterpart,
        // composition fragments that are never dispatched, and values that
        // are not domains at all.
        for declared in [
            "security",
            "version-control",
            "code-analyzer",
            "memory-manager",
            "mpm-agent-manager",
            "mpm-skills-manager",
            "base",
            "base-engineer",
            "base-ops",
            "base-qa",
            "base-research",
            "universal",
            "system",
            "trusty-mpm",
            "engineer|qa|ops|universal|documentation",
        ] {
            assert_eq!(
                normalize_role(Some(declared), None),
                UNMAPPED_ROLE,
                "{declared:?} must stay unmapped — admitting it is a reviewed \
                 decision, not a translation"
            );
        }
    }

    /// Absent, blank and unrecognized all land on the sentinel — the
    /// fail-closed default that preserves the pre-#4502 behavior exactly.
    #[test]
    fn unrecognized_or_absent_role_falls_back_to_the_sentinel() {
        for (role, agent_type) in [
            (None, None),
            (Some(""), None),
            (Some("   "), Some("  ")),
            (Some("totally-made-up"), None),
            (None, Some("also-made-up")),
        ] {
            assert_eq!(normalize_role(role, agent_type), UNMAPPED_ROLE);
        }
    }

    /// `role` is the preferred key; `agent_type` is consulted only when it is
    /// absent or blank.
    #[test]
    fn role_wins_over_agent_type() {
        assert_eq!(normalize_role(Some("qa"), Some("engineer")), "qa");
    }

    /// The deployed artifacts carry `agent_type` and no `role` at all, so the
    /// fallback is the path that actually runs in production.
    #[test]
    fn agent_type_is_the_deployed_fallback() {
        assert_eq!(normalize_role(None, Some("engineer")), "engineer");
        assert_eq!(normalize_role(Some("  "), Some("ops")), "ops");
    }

    /// A declared-but-unmappable `role` must NOT get a second chance through
    /// `agent_type`. Falling through would make the pair a UNION — "try every
    /// key until one is eligible" — which is a widening dressed up as a
    /// fallback.
    #[test]
    fn declared_but_unmappable_role_does_not_fall_through_to_agent_type() {
        assert_eq!(
            normalize_role(Some("security"), Some("engineer")),
            UNMAPPED_ROLE,
            "an explicit unmappable role must fail closed, not search for an \
             eligible sibling key"
        );
    }

    /// Frontmatter is hand-authored, so tolerate case and padding on the
    /// LOOKUP — while still requiring an exact table entry, so tolerance never
    /// becomes fuzzy matching.
    #[test]
    fn normalization_is_case_and_whitespace_insensitive() {
        assert_eq!(normalize_role(Some("  Engineer "), None), "engineer");
        assert_eq!(normalize_role(Some("QA"), None), "qa");
        // Still exact: a near-miss is not a match.
        assert_eq!(normalize_role(Some("engineers"), None), UNMAPPED_ROLE);
        assert_eq!(normalize_role(Some("qa-agent"), None), UNMAPPED_ROLE);
    }

    /// The claim this PR rests on: normalization changes ROLE eligibility
    /// only, and role eligibility alone reaches nothing. A delegation must
    /// also clear the server-owned name floor, which admits exactly two names
    /// — neither of which any claude-mpm agent can occupy, because both
    /// resolve to bundled TOMLs ahead of the claude-mpm fallback tier.
    #[test]
    fn claude_mpm_normalization_grants_no_new_reachable_target() {
        for name in crate::agents::delegation::ASSISTANT_REACHABLE_SUBAGENTS {
            assert!(
                name.ends_with("-agent"),
                "the floor is a NAME list resolved from the bundled TOML roster \
                 ({name}); if a claude-mpm artifact could occupy one of these \
                 names, normalization would stop being capability-neutral"
            );
        }
        // And the sentinel keeps every unmappable agent exactly where it was.
        assert_eq!(normalize_role(None, None), UNMAPPED_ROLE);
    }
}
