//! [`BundledArtifact`], [`InstallPolicy`], and the canonical [`ALL`] table.
//!
//! Why: splitting the artifact table out of `bundle.rs` kept that file under
//! the 500-line cap while the skill and agent catalogs were compiled in; since
//! #9012 the table holds only the hook policy files.
//! What: defines the two public types used by the installer and the static
//! slice enumerating every artifact in install order.
//! Test: `bundle_tests.rs` — `bundle_table_is_complete`.

use super::*;

/// One embedded framework artifact and its install location.
///
/// Why: the installer iterates a single table rather than hard-coding each
/// write, so adding a bundled artifact is a one-line change here.
/// What: a relative path (under `~/.trusty-mpm/framework/`) and the embedded
/// file contents.
/// Test: `bundle_table_is_complete`.
#[derive(Debug, Clone, Copy)]
pub struct BundledArtifact {
    /// Path relative to the framework root (e.g. `hooks/optimizer.toml`).
    pub rel_path: &'static str,
    /// Embedded file contents.
    pub contents: &'static str,
    /// Install policy: how the installer treats a pre-existing file.
    pub install: InstallPolicy,
}

/// How the installer writes a [`BundledArtifact`] when the target already exists.
///
/// Why: framework-owned files (instructions, policy) must track upgrades, but
/// user-editable stubs must not be clobbered — one enum makes the distinction
/// explicit and data-driven. Issue #3374 removed the last user-owned artifact
/// (the `CLAUDE.md` stub) and, with it, the `SeedOnce` variant; issue #3381
/// restores it because `install_to()` had never actually branched on the
/// enum at all — every entry used `Overwrite` so the single-variant enum was
/// silently equivalent to no policy. The two-way distinction is required
/// again the moment any bundled artifact is meant to be user-owned.
/// What: [`Overwrite`](InstallPolicy::Overwrite) always writes the embedded
/// contents; [`SeedOnce`](InstallPolicy::SeedOnce) writes only when absent.
/// Test: `seed_once_artifact_is_not_clobbered_without_force`,
/// `seed_once_artifact_force_resets_to_shipped_default`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallPolicy {
    /// Always write the embedded contents, replacing any existing file.
    Overwrite,
    /// Write the embedded contents only if the target file does not exist.
    /// `tm install --force` is the escape hatch: it resets a user-owned file
    /// back to the shipped default.
    SeedOnce,
}

/// Build an [`InstallPolicy::Overwrite`] artifact entry.
///
/// Why: nearly every bundled artifact is framework-owned and must track
/// upgrades. Spelling out the 5-line [`BundledArtifact`] struct literal for
/// all ~164 entries would blow the 500-SLOC production cap (issue #2903
/// alone adds 93 skill-port entries); this shorthand collapses the common
/// case to one line per entry so [`ALL`] stays a single, cap-compliant table
/// instead of being split across files (which Rust cannot do for one array
/// literal without unsafe const-array concatenation or forbidden
/// global/lazy state).
/// What: returns a [`BundledArtifact`] with `install: InstallPolicy::Overwrite`.
/// Test: `bundle_table_is_complete` (exercises every entry `ALL` produces,
/// including those built by this helper).
const fn overwrite(rel_path: &'static str, contents: &'static str) -> BundledArtifact {
    BundledArtifact {
        rel_path,
        contents,
        install: InstallPolicy::Overwrite,
    }
}

/// Build an [`InstallPolicy::SeedOnce`] artifact entry.
///
/// Why: mirrors [`overwrite`] for the user-owned case, so a genuinely
/// user-editable bundled artifact reads as a one-line table entry rather than
/// a hand-rolled struct literal. No entry in [`ALL`] currently uses it (issue
/// #3381 restores the enum shape without shipping a fake bundled entry just
/// to exercise it) — `pub(super)` so `bundle_tests.rs` can build a fixture
/// artifact with it instead.
/// What: returns a [`BundledArtifact`] with `install: InstallPolicy::SeedOnce`.
/// Test: `seed_once_constructor_builds_the_expected_artifact`,
/// `seed_once_artifact_is_not_clobbered_without_force`,
/// `seed_once_artifact_force_resets_to_shipped_default`.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) const fn seed_once(rel_path: &'static str, contents: &'static str) -> BundledArtifact {
    BundledArtifact {
        rel_path,
        contents,
        install: InstallPolicy::SeedOnce,
    }
}

/// Every bundled framework artifact, in install order.
///
/// Why: gives the installer (and tests) one canonical list to walk. (#4286
/// split A removed the `instructions/INSTRUCTIONS.md` stub entry: `tm
/// install`/`tm launch` always overwrite that same path with the assembled
/// system prompt in the same call, so the bundled stub's content never
/// reached a live session.)
/// What: the optimizer/overseer policy — code, not instructional content.
/// The agent roster (#9011) and the skills and bundled docs (#9012) are
/// runtime content, written by `tm install` from
/// [`crate::core::content_source`].
/// Test: `bundle_table_is_complete`.
pub const ALL: &[BundledArtifact] = &[
    overwrite("hooks/optimizer.toml", OPTIMIZER_TOML),
    overwrite("hooks/overseer.toml", OVERSEER_TOML),
];
