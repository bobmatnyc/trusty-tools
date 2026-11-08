//! Concept format: concept IDs, frontmatter split and render, the raw-preserving
//! round trip, the typed v0.2 view, container rules, validation, the v0.2 field
//! mapping and the strict file write (SPEC-OKF-01 §2.2, §2.2.1, §2.4, §2.8,
//! §2.11).
//!
//! No async and no I/O beyond the concept file [`write::write_concept`]
//! writes. Filled by slice S2 of #10746; the content hash (§2.10) comes with
//! `Bundle::put` in S3.
//!
//! The items SPEC-OKF-01 §4.3 names are re-exported here:
//! [`ConceptId`], [`Concept`], [`parse_concept`], [`render_concept`],
//! [`trust_tier`], [`staleness`].

pub mod concept;
pub mod container;
pub mod fields;
pub mod frontmatter;
pub mod id;
pub mod raw;
pub mod v02;
pub mod validate;
pub mod write;

pub use concept::{Concept, parse_concept, parse_concept_at, render_concept};
pub use fields::{
    Actor, Field, Frontmatter, Generated, Source, Staleness, Status, TrustTier, Verification,
    staleness, trust_tier,
};
pub use id::{ConceptId, ConceptRef, concept_ref_for_path};
pub use raw::{RawEntry, RawFrontmatter};
pub use validate::{Finding, FindingKind, Severity};
pub use write::{WriteMode, WriteOutcome, write_concept};
