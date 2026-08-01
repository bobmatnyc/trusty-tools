//! Which files in a NON-canonical agent tier are tm's? (issue #4442)
//!
//! Why: since #4409/#4437 bundled agents deploy into exactly one directory —
//! [`crate::agents::deployer`]'s target, `$CLAUDE_CONFIG_DIR/agents/`. Every
//! OTHER `.claude/agents/` directory on the machine is now a leftover, and a
//! leftover is not inert: Claude Code resolves the PROJECT tier before the user
//! tier, so a stale copy silently wins over the canonical deploy. That is
//! #4408 verbatim — a 32-byte project-tier stub beat the real 25 KB
//! `rust-engineer`, delegation kept "working", and nothing reported it.
//!
//! What: the reusable half of that detection — the agent-identity rule
//! ([`agent_identity`]), a canonical bundled-name roster
//! ([`bundled_agent_names`]), a pure per-file verdict
//! ([`classify_tier_resident`]), and a read-only directory scan
//! ([`audit_agent_tier`]). Nothing here writes, deletes, or renames anything.
//!
//! CONTRACT — this module is the SINGLE definition of "tm owns this file"
//! outside the canonical tier, deliberately shared by two consumers: `tm
//! doctor`'s `asset_tier` probe (#4442), which REPORTS what it returns, and the
//! quarantine/repair counterpart (#4448), which MOVES what it returns.
//!
//! Report and repair must agree file-for-file. If either side re-derives the
//! predicate locally they will drift, and the drift is asymmetric harm: doctor
//! flags a file the repair refuses to touch (noise operators learn to ignore),
//! or repair quarantines a file doctor never flagged (a project's own agent
//! silently disappears). Extend this module; do not fork it.
//!
//! Two invariants make that second hazard structurally hard, not merely
//! documented:
//!
//! 1. AGENT IDENTITY IS THE FRONTMATTER `name`, NOT THE FILENAME. Every
//!    consumer resolves agents by the declared `name:`, falling back to the
//!    stem only when it is absent (`trusty-agents`'
//!    `agents::claude_mpm_loader::parse_agent_file`). Keying on the stem would
//!    both MISS a real shadowing (`helper.md` declaring `name: rust-engineer`)
//!    and FALSELY flag a project's own agent (`rust-engineer.md` declaring
//!    `name: acme-custom`). [`agent_identity`] is applied to BOTH sides of the
//!    comparison — roster and candidate — so the two cannot key differently.
//! 2. A USER-OWNED LEDGER ENTRY OUTRANKS A NAME COLLISION. Ownership is
//!    [`TierOwnership`], a three-state value, never a bool: collapsing
//!    "untracked" and "tracked as the operator's" into one flag throws away the
//!    only positive PROOF that a file is not tm's, and a name collision then
//!    condemns it. Parity with
//!    [`crate::agents::deployer::retract_framework_agents`] is HALF, not whole,
//!    and the missing half is load-bearing for #4448. A USER-OWNED file is
//!    preserved by both: retraction skips it, and [`classify_tier_resident`]
//!    returns [`TierResidentClass::Custom`] for it BEFORE any name is compared.
//!    An UNTRACKED file is preserved by retraction — which can only see ledger
//!    entries — but is deliberately NOT preserved here:
//!    `classify_tier_resident("qa", Untracked, {"qa"})` is
//!    [`TierResidentClass::ShadowsBundled`], pinned by the
//!    `classify_bundled_name_shadows` test. That asymmetry IS the feature — an
//!    untracked copy on a bundled name is exactly what retraction cannot reach
//!    and what #4448 exists to quarantine. Do not read this invariant as a
//!    proof that untracked files are safe from the sweep; only the user-owned
//!    ledger entry is such a proof.
//!
//! [`TierResidentClass::Custom`] is the exclusion seam for everything else.
//! Project-tier agents tm never authored are legitimate — hand-placed today,
//! first-class once #4443 lands project-custom agents — and are classified, not
//! merely filtered, so a future category (e.g. a registry-sourced project
//! agent) is added as a variant here and BOTH consumers inherit the exclusion.
//!
//! Test: `tier_audit_tests.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::agents::deployer::is_agent_file;
use crate::agents::manifest::{AgentManifest, ManifestLoad};
use crate::agents::metadata::agent_metadata_from_str;

/// What this directory's ownership ledger says about one file.
///
/// Why: three states, never a bool. "Untracked" and "tracked as the
/// operator's" are both "not tm's", but only the second is positive PROOF —
/// and it is exactly the proof that must survive a name collision, or the
/// classifier condemns a file the deployer itself would preserve.
/// What: [`Untracked`](Self::Untracked) — no manifest entry;
/// [`FrameworkOwned`](Self::FrameworkOwned) — tracked with an origin
/// [`crate::agents::manifest::Origin::is_framework_owned`] accepts (the
/// `Overwrite` tier); [`UserOwned`](Self::UserOwned) — tracked with a
/// user-owned origin (the `SeedOnce` tier: `User` and, conservatively,
/// `Registry`).
/// Test: `ownership_of_reads_the_manifest_entry`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TierOwnership {
    /// No ledger entry names this file.
    Untracked,
    /// The ledger records tm as the author.
    FrameworkOwned,
    /// The ledger records the OPERATOR as the owner.
    UserOwned,
}

/// How one file in a non-canonical agent tier relates to tm's ownership.
///
/// Why: "delete every `.md` in `.claude/agents/`" is the wrong rule — it eats
/// a project's own agents. Ownership has to be established per file, from
/// evidence, and the evidence differs by case, so each case is named rather
/// than collapsed into a bool.
/// What: [`ShadowsBundled`](Self::ShadowsBundled) and
/// [`StrandedFrameworkOwned`](Self::StrandedFrameworkOwned) are tm-owned (report
/// them, and — for #4448 — quarantine them); [`Custom`](Self::Custom) is not
/// tm's and must never be reported or moved.
/// Test: `classify_*` cases in `tier_audit_tests.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TierResidentClass {
    /// The file resolves to a name tm currently ships, so this copy OUTRANKS
    /// and shadows the canonical user-tier deploy. The #4408 failure exactly.
    ShadowsBundled,
    /// Not a name tm currently ships, but this directory's ownership manifest
    /// records tm as the author. A retired or renamed bundled agent that an
    /// older binary wrote here and no deploy will ever refresh again.
    StrandedFrameworkOwned,
    /// Neither — a project-custom, hand-placed, or ledger-proven user-owned
    /// agent. Legitimate; excluded.
    Custom,
}

impl TierResidentClass {
    /// Whether this class means "tm authored this file".
    ///
    /// Why: both consumers need the same one-line answer, and encoding it here
    /// keeps a future variant from defaulting to the wrong side at two
    /// independent call sites.
    /// What: `true` for [`ShadowsBundled`](Self::ShadowsBundled) and
    /// [`StrandedFrameworkOwned`](Self::StrandedFrameworkOwned); `false` for
    /// [`Custom`](Self::Custom).
    /// Test: `tm_owned_is_true_for_exactly_the_two_owned_classes`.
    pub fn is_tm_owned(self) -> bool {
        !matches!(self, Self::Custom)
    }
}

/// One tm-owned file found outside the canonical tier.
///
/// Why: consumers report or move the file, so the path (to act on), the name
/// it resolves under (to explain), and the reason (to choose a severity) travel
/// together.
/// What: the path as scanned, the agent name per [`agent_identity`] — which is
/// NOT necessarily the filename stem — and the classification, never
/// [`TierResidentClass::Custom`], which [`audit_agent_tier`] filters out.
/// Test: `audit_reports_a_shadowing_stub`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MisplacedAgent {
    /// Full path of the offending file.
    pub path: PathBuf,
    /// The name this file resolves under — frontmatter `name:`, else the stem.
    pub name: String,
    /// Why this file is tm's.
    pub class: TierResidentClass,
}

/// The name an agent file resolves under.
///
/// Why: invariant 1 of this module's contract. Every consumer keys agents by
/// the declared `name:` (see `trusty-agents`'
/// `agents::claude_mpm_loader::parse_agent_file`), so a filename-based
/// predicate is wrong in both directions — it misses `helper.md` declaring
/// `name: rust-engineer` and falsely flags `rust-engineer.md` declaring
/// `name: acme-custom`. Applied to the roster AND the candidate so both sides
/// of the comparison are keyed identically by construction.
/// What: the trimmed frontmatter `name:` when the document declares a
/// non-empty one, else `file_name` with any `.md` suffix stripped. Frontmatter
/// is read with trusty-mpm's own lenient grammar via
/// [`agent_metadata_from_str`] — the same reader #4509 settled on for this
/// tier — and a malformed or absent block simply yields the stem rather than
/// an error, since a file that declares nothing IS resolved by its stem.
/// Test: `identity_prefers_the_frontmatter_name`,
/// `identity_falls_back_to_the_stem`, `identity_ignores_a_blank_name`.
pub fn agent_identity(content: &str, file_name: &str) -> String {
    let stem = file_name.strip_suffix(".md").unwrap_or(file_name);
    agent_metadata_from_str(content)
        .name
        .map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| stem.to_owned())
}

/// The canonical bundled agent roster, as resolved NAMES.
///
/// Why: "does this file resolve to a name tm ships?" must be answered from the
/// SOURCE tm actually deploys from, not a hard-coded list that silently rots
/// the first time an agent is added or renamed. Names, not stems: the bundle
/// itself carries files whose two differ (`BASE-AGENT.md` declares
/// `name: base-agent`).
/// What: [`agent_identity`] applied to every `.md` file directly under
/// `source_dir` (typically `FrameworkPaths::agent_source_dir()`). A missing or
/// unreadable directory yields an EMPTY set — the same "missing source is a
/// no-op, not an error" convention `deploy_agents` uses. Callers must treat an
/// empty roster as "cannot classify by name", never as "nothing is bundled":
/// concluding the latter would clear every shadowing file at once.
/// Test: `bundled_names_use_the_frontmatter_name`,
/// `bundled_names_missing_dir_is_empty`.
pub fn bundled_agent_names(source_dir: &Path) -> BTreeSet<String> {
    let Ok(entries) = std::fs::read_dir(source_dir) else {
        return BTreeSet::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let file_name = entry.file_name().to_str()?.to_owned();
            if !is_agent_file(&file_name) {
                return None;
            }
            let content = std::fs::read_to_string(entry.path()).unwrap_or_default();
            Some(agent_identity(&content, &file_name))
        })
        .collect()
}

/// What `manifest` says about `file_name`.
///
/// Why: the three-state read is the whole point of [`TierOwnership`]; doing it
/// in one place keeps a caller from re-deriving it as a bool and losing the
/// user-owned proof again.
/// What: [`TierOwnership::Untracked`] when no entry exists, else
/// `FrameworkOwned`/`UserOwned` per
/// [`crate::agents::manifest::Origin::is_framework_owned`] — the same predicate
/// the deployer and `retract_framework_agents` branch on.
/// Test: `ownership_of_reads_the_manifest_entry`.
pub fn ownership_of(manifest: &AgentManifest, file_name: &str) -> TierOwnership {
    match manifest.managed.get(file_name) {
        None => TierOwnership::Untracked,
        Some(entry) if entry.origin.is_framework_owned() => TierOwnership::FrameworkOwned,
        Some(_) => TierOwnership::UserOwned,
    }
}

/// Classify one file in a non-canonical tier. Pure — no I/O.
///
/// Why: the verdict is the shared contract between #4442 and #4448, so it is
/// separated from the scan that feeds it: both consumers can unit-test every
/// branch without staging a directory, and neither can quietly re-implement it.
/// What: a [`TierOwnership::UserOwned`] ledger entry wins FIRST and yields
/// [`TierResidentClass::Custom`] — the operator's own agent stays theirs even
/// when its name collides with a bundled one, matching
/// [`crate::agents::deployer::retract_framework_agents`], which preserves that
/// tier. Otherwise [`TierResidentClass::ShadowsBundled`] when `bundled`
/// contains `identity` (a shadowing file is shadowing whether or not the ledger
/// tracks it); otherwise [`TierResidentClass::StrandedFrameworkOwned`] when the
/// ledger records tm as the author; otherwise [`TierResidentClass::Custom`].
/// Test: `classify_bundled_name_shadows`,
/// `classify_user_owned_survives_a_bundled_name_collision`,
/// `classify_manifest_framework_owned_is_stranded`,
/// `classify_unknown_name_is_custom`,
/// `classify_prefers_shadowing_over_stranded`.
pub fn classify_tier_resident(
    identity: &str,
    ownership: TierOwnership,
    bundled: &BTreeSet<String>,
) -> TierResidentClass {
    if ownership == TierOwnership::UserOwned {
        return TierResidentClass::Custom;
    }
    if bundled.contains(identity) {
        return TierResidentClass::ShadowsBundled;
    }
    if ownership == TierOwnership::FrameworkOwned {
        return TierResidentClass::StrandedFrameworkOwned;
    }
    TierResidentClass::Custom
}

/// Scan one non-canonical agent tier and return the tm-owned files in it.
///
/// Why: the shared scan both consumers run, so "what doctor flags" and "what
/// repair quarantines" are the same list by construction.
/// What: READ-ONLY. Lists `.md` files directly under `dir` (never recursing —
/// the tier is flat), reads each one's frontmatter for [`agent_identity`] and
/// this directory's ownership manifest for [`ownership_of`], classifies with
/// [`classify_tier_resident`], and returns the
/// non-[`TierResidentClass::Custom`] results sorted by name for stable output.
/// A missing/unreadable `dir` returns an empty vec. A CORRUPT manifest degrades
/// to [`TierOwnership::Untracked`] for every file rather than failing: shadowing
/// is established by the roster alone, so the load-bearing signal survives.
/// Note the asymmetry that makes that safe — an unreadable ledger can only
/// LOSE the user-owned exemption, so #4448 must re-read the ledger under its
/// own write lock before moving anything; this read-only probe does not hold
/// one. Never call this on the canonical deploy directory — every file there
/// would classify as tm-owned, correctly and uselessly.
/// Test: `audit_reports_a_shadowing_stub`, `audit_ignores_a_custom_agent`,
/// `audit_ignores_a_user_owned_entry_on_a_bundled_name`,
/// `audit_reports_a_stranded_framework_entry`, `audit_missing_dir_is_empty`,
/// `audit_tolerates_a_corrupt_manifest`,
/// `audit_flags_a_renamed_file_that_declares_a_bundled_name`,
/// `audit_ignores_a_bundled_filename_that_declares_a_custom_name`.
pub fn audit_agent_tier(dir: &Path, bundled: &BTreeSet<String>) -> Vec<MisplacedAgent> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let manifest = match AgentManifest::load_checked(dir) {
        ManifestLoad::Ok(m) => m,
        // Corrupt ledger: keep the roster-based half of the verdict rather than
        // reporting nothing. See the doc comment.
        ManifestLoad::Corrupt(_) => AgentManifest::default(),
    };

    let mut found: Vec<MisplacedAgent> = entries
        .flatten()
        .filter_map(|entry| {
            let file_name = entry.file_name().to_str()?.to_owned();
            if !is_agent_file(&file_name) || !entry.path().is_file() {
                return None;
            }
            let content = std::fs::read_to_string(entry.path()).unwrap_or_default();
            let name = agent_identity(&content, &file_name);
            let class = classify_tier_resident(&name, ownership_of(&manifest, &file_name), bundled);
            class.is_tm_owned().then(|| MisplacedAgent {
                path: entry.path(),
                name,
                class,
            })
        })
        .collect();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

#[cfg(test)]
#[path = "tier_audit_tests.rs"]
mod tests;
