//! The trust label carried by every OKG-ingested entity (#4532, DOC-63 §6.3
//! `S-4.3`/`S-4.4`).
//!
//! Why: this repo has three defences against untrusted ingested content and
//! had built two. Capability reduction is built and test-pinned
//! (`bundled_personas_pin_git_reach`); prompt-level fencing is built, for
//! memory drawers (`trusty_agents::untrusted`). The third — *a trust label on
//! the content itself* — did not exist. Per-entity provenance was already
//! stamped at ingest ([`super::ingest`] writes `source_id`, `source_kind`,
//! `source_item_id`), but it is DESCRIPTIVE metadata: nothing downstream read
//! it as a trust signal, and nothing could, because "which of these source
//! kinds is trusted?" was a judgement no caller was equipped to make. This
//! module makes that judgement once, in the engine, and records the answer.
//!
//! What: [`TrustLabel`], the two-valued label; [`TrustLabel::for_source`], the
//! ENGINE-side derivation from a [`SourceSpec`]; and
//! [`TrustLabel::of_entity_file`], the fail-closed read back out of an entity's
//! frontmatter at the point of use.
//!
//! Three properties are load-bearing and each is pinned by a test:
//!
//! 1. **A connector cannot mark its own output trusted.** The label is derived
//!    from the registered [`SourceSpec`] — operator-authored configuration —
//!    never from the fetched item. [`super::ingest`] writes it into the
//!    frontmatter envelope BEFORE merging connector-supplied fields, and that
//!    merge already skips keys the envelope claimed, so a fetcher emitting
//!    `trust: user-authored` is shadowed out rather than honoured.
//!    Test: `connector_cannot_override_the_trust_label`.
//! 2. **Untrusted is the default, and the only default.** Every source kind is
//!    [`TrustLabel::UntrustedExternal`] unless the operator explicitly
//!    designated a *directory* source user-authored
//!    ([`SourceSpec::user_authored`]). A remote source can never be
//!    user-authored, whatever its row says — including a Gmail SENT-only
//!    window, which DOC-63 §6.4 `S-4.8` documents as signal quality, never a
//!    trust boundary.
//!    Test: `only_a_designated_directory_is_user_authored`,
//!    `remote_kinds_are_always_untrusted`.
//! 3. **Reading the label back fails closed.** An entity with no `trust` key,
//!    an unreadable file, or an unrecognised value resolves to
//!    [`TrustLabel::UntrustedExternal`]. Labels arrive incrementally over a
//!    corpus that already exists, so the unmigrated majority must be safe
//!    (DOC-63 `S-4.6`).
//!    Test: `unlabelled_entity_reads_back_untrusted`,
//!    `unknown_label_value_reads_back_untrusted`,
//!    `missing_file_reads_back_untrusted`.
//!
//! Test: `trust_tests.rs`.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_yaml::Value as Yaml;

use super::registry::{Locator, SourceSpec};

/// Frontmatter key holding the label.
///
/// Named once so the writer ([`super::ingest`]) and every reader agree by
/// construction rather than by two matching string literals.
pub const TRUST_KEY: &str = "trust";

/// How much an ingested entity's content may be believed.
///
/// Why/What/Test: see the module doc. Deliberately two-valued: DOC-63 §6.3
/// defines exactly one carve-out from "untrusted", and a richer lattice would
/// invite callers to invent middle grounds the threat model does not support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum TrustLabel {
    /// Content the user did not author and does not control. The default for
    /// every source kind. Fenced on retrieval.
    #[default]
    UntrustedExternal,
    /// A local directory the operator explicitly designated user-authored.
    /// The single carve-out in DOC-63 §6.3 `S-4.3`.
    UserAuthored,
}

impl TrustLabel {
    /// The stable wire/frontmatter spelling.
    ///
    /// Why: written into markdown a human reads and hand-edits, so it is a
    /// stable contract, not a `Debug` rendering.
    /// What: kebab-case, matching the `serde` representation.
    /// Test: `label_strings_round_trip`.
    pub fn as_str(self) -> &'static str {
        match self {
            TrustLabel::UntrustedExternal => "untrusted-external",
            TrustLabel::UserAuthored => "user-authored",
        }
    }

    /// Parse a frontmatter value; `None` for anything unrecognised.
    ///
    /// Why: an unrecognised value is NOT a third trust level — it is a value
    /// this build does not understand, and understanding is a precondition for
    /// trusting. Returning `None` (rather than a default) keeps that decision
    /// with the caller, and every caller in-tree fails closed.
    /// What: exact match on [`Self::as_str`], case-insensitively, trimmed.
    /// Test: `label_strings_round_trip`, `unknown_label_value_reads_back_untrusted`.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "untrusted-external" => Some(TrustLabel::UntrustedExternal),
            "user-authored" => Some(TrustLabel::UserAuthored),
            _ => None,
        }
    }

    /// Whether content carrying this label must be fenced before a model sees
    /// it.
    ///
    /// Why: the fencing decision is asked at every retrieval site, and asking
    /// it as `label == UntrustedExternal` at each one is how a third variant
    /// would later leak through un-fenced. One predicate, one place to change.
    /// Test: `untrusted_is_the_fencing_predicate`.
    pub fn is_untrusted(self) -> bool {
        matches!(self, TrustLabel::UntrustedExternal)
    }

    /// Derive the label for everything a source ingests — the ENGINE's
    /// judgement, made from operator configuration only.
    ///
    /// Why: DOC-63 `S-4.3` — "the label is written by the engine, not the
    /// connector — a connector cannot mark its own output trusted". This
    /// function is that sentence. Its only input is the registered
    /// [`SourceSpec`], which the operator wrote; the fetched
    /// [`super::ingest::SourceItem`] is deliberately not a parameter, so no
    /// future connector can reach the decision even by accident.
    /// What: [`TrustLabel::UserAuthored`] only when the locator is a local
    /// directory AND the row carries [`SourceSpec::user_authored`]. Every
    /// remote kind is [`TrustLabel::UntrustedExternal`] unconditionally — the
    /// `user_authored` flag on a remote row is ignored, not honoured, because
    /// no remote corpus has an enforceable author constraint (DOC-63 §6.4
    /// `S-4.8`, §6.4 `S-4.9`).
    /// Test: `only_a_designated_directory_is_user_authored`,
    /// `remote_kinds_are_always_untrusted`,
    /// `undesignated_directory_is_untrusted`.
    pub fn for_source(spec: &SourceSpec) -> Self {
        match spec.locator {
            Locator::DocStore { .. } if spec.user_authored => TrustLabel::UserAuthored,
            _ => TrustLabel::UntrustedExternal,
        }
    }

    /// Read the label out of an already-parsed frontmatter mapping.
    ///
    /// Why/What: the in-memory half of [`Self::of_entity_file`], split out so
    /// a caller that already holds a parsed entity does not re-read the file.
    /// An absent or non-scalar `trust` key yields `None`.
    /// Test: `reads_the_label_out_of_frontmatter`,
    /// `unlabelled_entity_reads_back_untrusted`.
    pub fn from_frontmatter(frontmatter: &Yaml) -> Option<Self> {
        frontmatter
            .get(TRUST_KEY)
            .and_then(Yaml::as_str)
            .and_then(Self::parse)
    }

    /// Resolve the label for one entity file on disk, FAIL-CLOSED.
    ///
    /// Why: this is the point-of-use read, and DOC-63 `S-4.6` requires it to
    /// be safe for a corpus that predates the label entirely. Every failure
    /// mode — the file is gone, unreadable, not an entity, has no frontmatter,
    /// has no `trust` key, or carries a value this build does not recognise —
    /// resolves to [`TrustLabel::UntrustedExternal`]. There is no error
    /// return, because there is no caller for whom "I could not tell" should
    /// mean anything other than "fence it".
    /// What: reads the file and parses its frontmatter via [`crate::entity`],
    /// then defers to [`Self::from_frontmatter`], defaulting on `None`.
    /// Test: `reads_the_label_off_disk`, `missing_file_reads_back_untrusted`,
    /// `unlabelled_entity_reads_back_untrusted`,
    /// `unknown_label_value_reads_back_untrusted`,
    /// `unparseable_frontmatter_reads_back_untrusted`.
    pub fn of_entity_file(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return TrustLabel::UntrustedExternal;
        };
        Self::of_entity_text(&text)
    }

    /// [`Self::of_entity_file`] over content already in memory.
    ///
    /// Why: the retrieval path sometimes holds the bytes and never the path
    /// (and tests always do), and duplicating the fail-closed ladder for that
    /// case is exactly how the two would drift apart.
    /// What: parses frontmatter via [`crate::entity::Entity::from_content`],
    /// defaulting to untrusted on any failure.
    /// Test: `unparseable_frontmatter_reads_back_untrusted`,
    /// `reads_the_label_out_of_frontmatter`.
    pub fn of_entity_text(text: &str) -> Self {
        crate::entity::Entity::from_content(text)
            .ok()
            .and_then(|e| Self::from_frontmatter(&e.frontmatter))
            .unwrap_or(TrustLabel::UntrustedExternal)
    }
}

#[cfg(test)]
#[path = "trust_tests.rs"]
mod tests;
