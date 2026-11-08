//! Concept format: concept IDs, frontmatter split and render, the raw-preserving
//! round trip, the typed v0.2 view, container rules, validation, the v0.2 field
//! mapping and the strict file write (SPEC-OKF-01 §2.2, §2.2.1, §2.4, §2.8,
//! §2.11).
//!
//! No async and no I/O beyond the concept file a crate-private write
//! touches; `Bundle::put` is the public write. Filled by slices S2 and S3 of
//! #10746; S4 adds the §2.13.1 protected key set.
//!
//! The items SPEC-OKF-01 §4.3 names are re-exported here:
//! [`ConceptId`], [`Concept`], [`parse_concept`], [`render_concept`],
//! [`content_hash`], [`trust_tier`], [`staleness`].

pub mod concept;
pub mod container;
pub mod fields;
pub mod frontmatter;
pub mod hash;
pub mod id;
// #10746 S4: the §2.13.1 protected key set the ingest boundary reads.
pub mod protected;
pub mod raw;
// #10746 S8: the §4.8 tombstone detection rule, restated locally.
pub mod tombstone;
pub mod v02;
pub mod validate;
pub mod write;

pub use concept::{Concept, parse_concept, parse_concept_at, render_concept};
pub use fields::{
    Actor, Field, Frontmatter, Generated, Source, Staleness, Status, TrustTier, Verification,
    staleness, trust_tier,
};
pub use hash::{ContentHash, LIFECYCLE_KEYS, content_hash};
pub use id::{ConceptId, ConceptRef, concept_ref_for_path};
pub use protected::{PROTECTED_KEYS, SOURCE_FIELDS_KEY, is_protected_key};
pub use raw::{RawEntry, RawFrontmatter};
pub use tombstone::is_detectable_tombstone;
pub use validate::{Finding, FindingKind, Severity};
pub use write::{WriteMode, WriteOutcome, check_write};
