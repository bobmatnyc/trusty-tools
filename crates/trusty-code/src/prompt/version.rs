//! Version identifier for the BASE protocol preamble.
//!
//! Why: The parity report (spec §6/D5) must record, per run, the exact BASE
//! version that was assembled so any parity claim is auditable. Pinning the
//! version to a constant — rather than deriving it from the crate version —
//! lets the preamble text and its identity evolve independently of crate
//! releases, and gives the report a stable, byte-comparable token.
//! What: A single `&str` constant naming the current BASE preamble revision.
//! Bump it whenever `BASE_PREAMBLE` (in `preamble.rs`) changes in a way that
//! affects assembled bytes.
//! Test: `prompt::tests::base_preamble_version_is_semver_shaped`.

/// Semantic version of the [`crate::prompt::BASE_PREAMBLE`] constant.
///
/// Why: Disclosed in the parity report (spec D5) so a reader can tie a run's
/// assembled prompt back to an exact, immutable preamble revision.
/// What: A `MAJOR.MINOR.PATCH` string. Increment on any change to the
/// `BASE_PREAMBLE` text so reports referencing an old run remain interpretable.
/// Test: `prompt::tests::base_preamble_version_is_semver_shaped` asserts the
/// three-component shape.
pub const BASE_PREAMBLE_VERSION: &str = "1.4.0";
