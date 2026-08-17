//! The one JSONL line: what a shared memory carries between machines (#5902).
//!
//! Why: the export file is a wire format two independently-built binaries read
//! and write, so its shape and its version live in one place rather than being
//! implied by whatever the exporter happened to serialize.
//! What: [`SHARE_FORMAT_VERSION`], [`SharedMemoryRecord`], and the verification
//! a reader owes before trusting a line.
//! Test: `record_round_trips_through_json`, `verify_rejects_a_forged_digest`,
//! `verify_rejects_an_unknown_format_version` in `share::tests`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::memory_core::content_hash::{CONTENT_HASH_VERSION, ContentHash, memory_content_hash};
use crate::memory_core::palace::{Drawer, DrawerType};

/// Version of the JSONL record shape.
///
/// Why: separate from [`CONTENT_HASH_VERSION`] because the two change for
/// different reasons. Adding a field to the record is compatible and does not
/// touch identity; changing the normalization re-mints every id. A reader that
/// conflated them could not tell "I do not understand this field layout" from "I
/// cannot reproduce these digests".
/// What: `1`. A record declaring a higher version is refused rather than
/// best-effort parsed — see [`SharedMemoryRecord::verify`].
pub const SHARE_FORMAT_VERSION: u32 = 1;

/// One exported memory: its content-addressed identity plus the metadata an
/// importer needs to place it.
///
/// 🔴 MEMORIES ONLY — no derived data, by owner decision. No embedding vector,
/// no HNSW/usearch index state, no BM25 data. Anything rebuildable from the
/// bodies stays out, and stays out as an ABSENT FIELD rather than an optional
/// empty one: an empty field invites a later change to populate it without
/// re-opening this decision.
///
/// Why: an embedding is a lossy but real encoding of its source text, and these
/// files are committed to a PUBLIC repository. Excluding derived data makes what
/// gets published exactly the memory bodies we already intend to publish, with no
/// second channel carrying the same content in a form nobody inspects.
/// `trusty-agents`' `ExportRecord` does carry a vector, on the reasoning that the
/// receiver can insert without a model load; that trade is refused here. Three
/// supporting reasons: a 384-dimension `f32` array serializes to roughly 4–6 KB of
/// JSON, an order of magnitude larger than the body it describes, which makes a
/// committed file's diff unreadable and its history heavy; the receiving machine's
/// embedder, dimension, or model revision need not match the sender's, so an
/// imported vector can be silently wrong in a way no assertion here could catch;
/// and re-embedding on import is cheap against a warm process-wide embedder. The
/// accepted cost is that import depends on the embedder — see
/// [`super::import::import_palace_records`] for how that failure is handled.
///
/// Why no author, machine, or session field: the palace has none to export.
/// `Palace`, `Wing`, `Room`, and `Drawer` carry no provenance of any kind; the
/// only provenance in the model is `Triple.provenance`, a free-form optional
/// string on KG edges. Inventing one here would be a new concept smuggled in
/// through a file format.
///
/// What: the digest, the verbatim body, tags, `created_at`, the drawer type tag,
/// the room label, and importance. The room is the LABEL, not the `room_id`:
/// room ids are UUIDv5 over `(wing_id, lowercased label)` (ADR-0027), so both
/// machines mint the same id from the same label, and the label survives a
/// palace that has not seen that room yet.
/// Test: `record_round_trips_through_json`, `export_then_import_preserves_metadata`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SharedMemoryRecord {
    /// [`SHARE_FORMAT_VERSION`] at the time of export.
    pub format_version: u32,
    /// [`CONTENT_HASH_VERSION`] the digest was computed under.
    pub hash_version: u32,
    /// The content-addressed identity of [`Self::content`].
    pub content_hash: ContentHash,
    /// The memory body, verbatim as stored. Normalization applies to hashing
    /// only, so what travels is what the author wrote.
    pub content: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    /// [`DrawerType::as_str`] tag; an unrecognised value imports as `Unknown`.
    pub drawer_type: String,
    /// Room label, parsed back through `RoomType::parse` on import.
    pub room: String,
    pub importance: f32,
}

/// Why a record could not be trusted.
///
/// Why this is an error and not a warning: an import that accepted a line whose
/// declared digest does not match its body would store a memory under an
/// identity no other machine can reproduce, which breaks convergence silently
/// and permanently. Refusing the line is recoverable; accepting it is not.
/// Test: `verify_rejects_a_forged_digest`, `verify_rejects_an_unknown_format_version`,
/// `import_skips_a_bad_line_and_keeps_the_rest`.
#[derive(Debug, Clone, thiserror::Error)]
pub enum RecordError {
    #[error(
        "record declares share format version {found}, but this build understands \
         up to {SHARE_FORMAT_VERSION}; upgrade before importing"
    )]
    UnknownFormatVersion { found: u32 },
    #[error(
        "record declares content-hash version {found}, but this build computes \
         version {CONTENT_HASH_VERSION}; its digests are not comparable with local ones"
    )]
    UnknownHashVersion { found: u32 },
    #[error(
        "record's declared content_hash {declared} does not match the digest of \
         its own body ({computed})"
    )]
    DigestMismatch {
        declared: ContentHash,
        computed: ContentHash,
    },
}

impl SharedMemoryRecord {
    /// Build a record from a stored drawer.
    ///
    /// Why: the digest is taken from `drawer.content_hash` rather than recomputed
    /// here so that a drift between the drawer's cached digest and its content
    /// would show up as a [`RecordError::DigestMismatch`] on the receiving side
    /// instead of being papered over at export time. Every hydration path
    /// refreshes the field, so in practice the two agree; the point is that if
    /// they ever did not, the failure is loud and on the record.
    /// What: copies the body, tags, timestamp, type tag, and importance, and
    /// stamps both version numbers.
    /// Test: `record_round_trips_through_json`, `export_then_import_preserves_metadata`.
    pub fn from_drawer(drawer: &Drawer, room_label: &str) -> Self {
        Self {
            format_version: SHARE_FORMAT_VERSION,
            hash_version: CONTENT_HASH_VERSION,
            content_hash: drawer.content_hash,
            content: drawer.content.clone(),
            tags: drawer.tags.clone(),
            created_at: drawer.created_at,
            drawer_type: drawer.drawer_type.as_str().to_string(),
            room: room_label.to_string(),
            importance: drawer.importance,
        }
    }

    /// Check that this record is one this build can act on.
    ///
    /// Why: a reader must establish three things before a line can participate in
    /// hash-keyed convergence — that it understands the field layout, that its
    /// digests live in the same space as local ones, and that the digest actually
    /// describes the body it arrived with. Skipping the third would let a
    /// hand-edited or corrupted file insert a memory under a fabricated identity.
    /// What: returns the recomputed digest on success. Version checks run before
    /// the digest check, because a version this build does not understand makes
    /// the digest comparison meaningless rather than merely wrong.
    /// Test: `verify_rejects_a_forged_digest`,
    /// `verify_rejects_an_unknown_format_version`,
    /// `verify_rejects_an_unknown_hash_version`, `verify_accepts_a_round_tripped_record`.
    pub fn verify(&self) -> Result<ContentHash, RecordError> {
        if self.format_version > SHARE_FORMAT_VERSION {
            return Err(RecordError::UnknownFormatVersion {
                found: self.format_version,
            });
        }
        if self.hash_version != CONTENT_HASH_VERSION {
            return Err(RecordError::UnknownHashVersion {
                found: self.hash_version,
            });
        }
        let computed = memory_content_hash(&self.content);
        if computed != self.content_hash {
            return Err(RecordError::DigestMismatch {
                declared: self.content_hash,
                computed,
            });
        }
        Ok(computed)
    }

    /// The record's drawer type, `Unknown` for a tag this build does not know.
    ///
    /// Why: a newer sender may name a `DrawerType` variant this build has never
    /// heard of. Falling back to `Unknown` keeps the memory importable, which is
    /// the right trade for a classification hint — unlike the digest, an
    /// unrecognised type costs nothing but a less precise label.
    /// What: delegates to [`DrawerType::from_tag`].
    /// Test: `export_then_import_preserves_metadata`,
    /// `import_of_an_unknown_drawer_type_falls_back_to_unknown`.
    pub fn parsed_drawer_type(&self) -> DrawerType {
        DrawerType::from_tag(Some(&self.drawer_type))
    }
}
