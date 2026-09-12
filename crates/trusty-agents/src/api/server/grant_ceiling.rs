//! The grant ceiling a turn-originated settings patch may never widen (#7396).
//!
//! Why: `ask_concierge` is registered `read_only=false` on every attended
//! assistant turn, so the model — including a turn steered by ingested document
//! text, an attached table cell, or a listener excerpt — can reach
//! `settings.patch`. The grant fields that patch writes (`[permissions].scopes`,
//! `[tools].allow`, `[skills].allow`, `[subagents].delegate_allowed`) are the
//! same lists every later permission check reads, so a self-issued
//! `scopes=["*"]` would pass every subsequent `scope_granted` call. The tool's
//! own schema already promises "Role and delegation ceilings cannot be widened";
//! this module is what makes that true. The operator-authenticated HTTP route
//! (`PATCH /api/agents/{name}`) carries no ceiling and is unchanged — narrowing
//! and widening both stay legitimate operator capabilities there.
//! What: [`GrantCeiling`] snapshots the assistant's RESOLVED manifest grants
//! (the merged `extends` chain, i.e. exactly what is enforced) before the patch,
//! and [`GrantCeiling::check`] refuses any requested entry the snapshot does not
//! already cover. A refusal is whole and structured — never a silent narrowing —
//! so the caller learns which entries were rejected.
//! Test: `super::tests::grant_ceiling` — the turn-originated `scopes=["*"]`
//! refusal, the narrowing that is still accepted, and the untouched-file
//! postcondition.

use crate::tools::registry::scope::{Scope, ScopePattern};

/// A refused grant edit: which field, why, and the exact offending entries.
///
/// Why: the caller is a model turn whose next move depends on knowing the
/// request was refused rather than silently trimmed, and an operator reading the
/// log needs the offenders named. A plain string loses the field and the list.
/// What: rendered by [`crate::api::server::agent_patch`] into a `400` body
/// carrying `error`, `field` and `refused`.
/// Test: `super::tests::grant_ceiling::turn_patch_cannot_widen_its_own_scopes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GrantRefusal {
    pub(super) field: String,
    pub(super) message: String,
    pub(super) refused: Vec<String>,
}

impl GrantRefusal {
    /// A malformed-input refusal, with no offending-entry list to report.
    pub(super) fn malformed(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
            refused: Vec::new(),
        }
    }
}

/// The resolved grants a turn-originated patch may narrow but never exceed.
///
/// Why: the ceiling has to be what is ENFORCED, not what one file declares.
/// An assistant's grants come from the merged `extends` chain, so a ceiling read
/// from the child manifest alone would refuse legitimate narrowing (the child
/// declares nothing and inherits everything) — a false refusal that would push
/// the next change back toward no ceiling at all.
/// What: four snapshots taken from a loaded [`crate::agents::AgentConfig`].
/// `None` means the field grants nothing, so only an empty list may be written:
/// every unknown answer denies (see [`GrantCeiling::check_field`]).
/// Test: `super::tests::grant_ceiling::turn_patch_may_narrow_but_not_widen`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct GrantCeiling {
    scopes: Option<Vec<String>>,
    tools: Option<Vec<String>>,
    skills: Option<Vec<String>>,
    subagents: Option<Vec<String>>,
}

impl GrantCeiling {
    /// Snapshot the ceiling from an assistant's resolved config.
    pub(super) fn from_config(config: &crate::agents::AgentConfig) -> Self {
        Self {
            scopes: crate::agents::permissions::effective_scopes(
                &config.tools,
                &config.permissions,
            ),
            tools: config.tools.allow.clone(),
            skills: config.skills.allow.clone(),
            subagents: config.subagents.delegate_allowed.clone(),
        }
    }

    /// Refuse every grant field in `req` that the ceiling does not already cover.
    ///
    /// Why: the four fields are checked together, before any of them is written,
    /// so a request that widens one and narrows another is refused whole rather
    /// than half-applied.
    /// What: `Ok(())` when every requested entry is covered; the FIRST offending
    /// field otherwise, with all of that field's offenders listed.
    /// Test: `super::tests::grant_ceiling::turn_patch_cannot_widen_its_own_scopes`.
    pub(super) fn check(
        &self,
        req: &super::agent_patch::PatchAgentRequest,
    ) -> Result<(), GrantRefusal> {
        for (field, requested, permitted, kind) in [
            (
                "permissions.scopes",
                &req.scopes,
                &self.scopes,
                Vocabulary::Scope,
            ),
            (
                "tools.allow",
                &req.tools_allow,
                &self.tools,
                Vocabulary::Glob,
            ),
            (
                "skills.allow",
                &req.skills_allow,
                &self.skills,
                Vocabulary::Glob,
            ),
            (
                "subagents.delegate_allowed",
                &req.subagents_delegate_allowed,
                &self.subagents,
                Vocabulary::Glob,
            ),
        ] {
            if let Some(requested) = requested.as_deref() {
                Self::check_field(field, requested, permitted.as_deref(), kind)?;
            }
        }
        Ok(())
    }

    /// One field's subset check.
    ///
    /// Why: the entries are glob PATTERNS, so a plain set-difference would let
    /// `memory.*` through against a ceiling of `memory.read`. A requested
    /// pattern is only covered when it cannot name anything the ceiling does
    /// not: a concrete entry must be matched by some ceiling pattern, and an
    /// entry that carries a wildcard of its own must appear in the ceiling
    /// verbatim. `*` is therefore refused unless the ceiling literally grants
    /// `*`, which is the case the tool's schema promises is impossible.
    /// What: collects every offender before returning, so one round trip tells
    /// the caller about all of them.
    fn check_field(
        field: &str,
        requested: &[String],
        permitted: Option<&[String]>,
        kind: Vocabulary,
    ) -> Result<(), GrantRefusal> {
        let permitted = permitted.unwrap_or(&[]);
        let refused: Vec<String> = requested
            .iter()
            .filter(|entry| !covers(permitted, entry, kind))
            .cloned()
            .collect();
        if refused.is_empty() {
            return Ok(());
        }
        Err(GrantRefusal {
            field: field.into(),
            message: format!(
                "{field} may only narrow this assistant's existing grants, never widen them — \
                 refused: {refused:?}"
            ),
            refused,
        })
    }
}

/// Which matcher decides whether a ceiling entry covers a requested one.
///
/// Why: `[permissions].scopes` is matched by [`ScopePattern`] (segment-boundary
/// `a.*` globs only) while the three allow-lists are matched by the persona
/// glob matcher. Checking a field with the other one's matcher would refuse
/// legitimate narrowing, so the vocabulary travels with the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Vocabulary {
    Scope,
    Glob,
}

/// Whether `permitted` already grants everything `entry` could name.
///
/// A requested entry carrying a wildcard must appear in the ceiling verbatim:
/// deciding whether one glob is contained in another is not something either
/// matcher answers, and guessing in the permissive direction is the whole
/// defect this module exists to close.
fn covers(permitted: &[String], entry: &str, kind: Vocabulary) -> bool {
    if entry.contains('*') || entry.contains('?') {
        return permitted.iter().any(|p| p == entry);
    }
    if permitted.iter().any(|p| p == entry) {
        return true;
    }
    match kind {
        Vocabulary::Scope => permitted
            .iter()
            .any(|p| ScopePattern::new(p.as_str()).matches(&Scope::new(entry))),
        Vocabulary::Glob => crate::ctrl::pm_task::match_any_glob(entry, permitted),
    }
}
