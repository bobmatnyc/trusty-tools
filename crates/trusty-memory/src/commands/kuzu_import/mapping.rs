//! Map kuzu-memory rows onto drawers and KG triples (#277).
//!
//! Why: the mapping is the contract a re-run depends on — the identity tag and
//! the hash tag decide whether a second import is a no-op, an update, or a new
//! drawer — so it lives in pure functions with no palace access.
//! What: [`map_memory`] turns one Memory row into a [`MappedMemory`] carrying
//! its identity tag `source:kuzu-memory/<Memory.id>`, its hash tag
//! `kuzu-hash:<content_hash>`, its provenance tag `kuzu-store:<store dir>`,
//! and one tag per classification column. [`entity_triples`] and the edge
//! helpers build the KG side.
//! Test: `mapping_tags_identity_hash_and_columns`,
//! `moved_store_reimports_nothing_and_shared_ids_are_reported`.

use chrono::{DateTime, NaiveDateTime, Utc};
use sha2::{Digest, Sha256};
use trusty_common::memory_core::store::kg::Triple;

use super::bridge::{KuzuEntityRow, KuzuMemoryRow};

/// Prefix of the identity tag every imported drawer carries.
pub const SOURCE_TAG_PREFIX: &str = "source:kuzu-memory/";
/// Prefix of the tag recording the source `content_hash` at import time.
pub const HASH_TAG_PREFIX: &str = "kuzu-hash:";
/// Prefix of the provenance tag naming the store a drawer was imported from.
pub const STORE_TAG_PREFIX: &str = "kuzu-store:";
/// Prefix of the tag a drawer carries between its insert and its identity
/// stamp; a re-run finishes such a drawer instead of inserting a second one.
pub const PENDING_TAG_PREFIX: &str = "kuzu-pending:";
/// Tag marking a drawer as imported from kuzu-memory, for filtering.
pub const ORIGIN_TAG: &str = "kuzu-memory";
/// Column tags [`map_memory`] generates; `--update` replaces only these.
const COLUMN_TAG_PREFIXES: &[&str] = &[
    "memory_type:",
    "knowledge_type:",
    "source_type:",
    "project:",
    "agent:",
    "user:",
    "session:",
    "meta:",
];
/// Most `meta:` tags one memory's `metadata` may contribute.
const MAX_META_TAGS: usize = 8;
/// Longest `metadata` value that becomes a tag.
const MAX_META_VALUE_CHARS: usize = 64;
/// Longest store-supplied token (a `meta:` key or a relationship type) that
/// may become part of a tag or predicate.
const MAX_TOKEN_CHARS: usize = 64;
/// Importance when the store has no `importance` column.
const DEFAULT_IMPORTANCE: f32 = 0.5;
/// Confidence for an edge with none recorded.
const DEFAULT_EDGE_CONFIDENCE: f32 = 0.8;

/// A Memory row ready to write as a drawer.
#[derive(Debug, Clone, PartialEq)]
pub struct MappedMemory {
    /// `kuzu-memory/<Memory.id>` — the identity tag without `source:`.
    pub source_key: String,
    pub memory_id: String,
    /// The canonical `.kuzu-memory` directory this row came from.
    pub store: String,
    pub content: String,
    pub hash: String,
    pub created_at: Option<DateTime<Utc>>,
    pub importance: f32,
    /// Every tag the finished drawer carries, identity and hash included.
    pub tags: Vec<String>,
}

impl MappedMemory {
    /// Tags for the first write of a new drawer: everything but the identity
    /// and hash tags, plus a [`PENDING_TAG_PREFIX`] marker.
    ///
    /// Why (#277 M2): identity is written last, in the same write that sets
    /// `created_at`, so a drawer never claims an identity while its
    /// `created_at` is still wrong. The pending marker lets a re-run find a
    /// drawer whose stamp failed and finish it rather than duplicate it.
    /// Test: `stamp_failure_leaves_a_pending_drawer_the_next_run_finishes`.
    pub fn staging_tags(&self) -> Vec<String> {
        let mut tags: Vec<String> = self
            .tags
            .iter()
            .filter(|t| !t.starts_with(SOURCE_TAG_PREFIX) && !t.starts_with(HASH_TAG_PREFIX))
            .cloned()
            .collect();
        tags.push(format!("{PENDING_TAG_PREFIX}{}", self.memory_id));
        tags
    }
}

/// Whether `tag` is one [`map_memory`] generates (or the pending marker).
///
/// Why (#277 L5): `--update` must replace the importer's own tags and keep
/// every tag something else added to the drawer since.
pub fn is_generated_tag(tag: &str) -> bool {
    tag == ORIGIN_TAG
        || [
            SOURCE_TAG_PREFIX,
            HASH_TAG_PREFIX,
            STORE_TAG_PREFIX,
            PENDING_TAG_PREFIX,
        ]
        .iter()
        .chain(COLUMN_TAG_PREFIXES)
        .any(|p| tag.starts_with(p))
}

/// `current`'s tags that the importer did not generate, followed by `fresh`.
///
/// Test: `update_keeps_tags_the_importer_did_not_generate`.
pub fn merge_tags(current: &[String], fresh: &[String]) -> Vec<String> {
    let mut out: Vec<String> = current
        .iter()
        .filter(|t| !is_generated_tag(t) && !fresh.contains(t))
        .cloned()
        .collect();
    out.extend(fresh.iter().cloned());
    out
}

/// The identity key for `memory_id` (the tag minus `source:`).
///
/// Why (#277 H4): the owner ruled `Memory.id` is the identity, so a moved,
/// renamed or re-cloned store maps onto the drawers it already imported.
pub fn source_key(memory_id: &str) -> String {
    format!("kuzu-memory/{memory_id}")
}

/// Whether `s` is a short `[A-Za-z0-9_-]` token, safe inside a tag or a
/// predicate (#277 L4).
fn is_safe_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_TOKEN_CHARS
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// kuzu-memory's own hash: SHA-256 of the lower-cased, trimmed content.
///
/// Why: stores written before kuzu-memory added `content_hash` have no column
/// to read, and change detection still needs a value. Using kuzu-memory's own
/// formula (`core/models.py`) keeps the two sources comparable.
pub fn kuzu_content_hash(content: &str) -> String {
    let digest = Sha256::digest(content.trim().to_lowercase().as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Map one Memory row. `None` when the row has no content to store.
///
/// What: content and `created_at` carry over; `importance` is clamped to
/// 0..=1; `memory_type`, `knowledge_type`, `source_type`, `project_tag`,
/// `agent_id` (unless `default`), `user_id` and `session_id` become
/// `<column>:<value>` tags; scalar `metadata` entries whose key is a safe
/// token become up to [`MAX_META_TAGS`] `meta:<key>:<value>` tags. The hash is
/// the row's `content_hash`, or [`kuzu_content_hash`] when the store has none.
/// `store` is the canonical store directory, recorded as provenance only.
/// Test: `mapping_tags_identity_hash_and_columns`.
pub fn map_memory(row: &KuzuMemoryRow, store: &str) -> Option<MappedMemory> {
    let memory_id = row.id.clone()?;
    let content = row.content.clone().filter(|c| !c.trim().is_empty())?;
    let hash = row
        .content_hash
        .clone()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| kuzu_content_hash(&content));
    let key = source_key(&memory_id);
    let mut tags = vec![
        format!("source:{key}"),
        format!("{HASH_TAG_PREFIX}{hash}"),
        format!("{STORE_TAG_PREFIX}{store}"),
        ORIGIN_TAG.to_string(),
    ];
    let columns = [
        ("memory_type", &row.memory_type),
        ("knowledge_type", &row.knowledge_type),
        ("source_type", &row.source_type),
        ("project", &row.project_tag),
        ("agent", &row.agent_id),
        ("user", &row.user_id),
        ("session", &row.session_id),
    ];
    for (name, value) in columns {
        let Some(v) = value.as_deref().map(str::trim).filter(|v| !v.is_empty()) else {
            continue;
        };
        if name == "agent" && v == "default" {
            continue;
        }
        tags.push(format!("{name}:{v}"));
    }
    tags.extend(metadata_tags(row.metadata.as_deref()));
    Some(MappedMemory {
        source_key: key,
        memory_id,
        store: store.to_string(),
        content,
        hash,
        created_at: row.created_at.as_deref().and_then(parse_timestamp),
        importance: row
            .importance
            .map(|i| (i as f32).clamp(0.0, 1.0))
            .unwrap_or(DEFAULT_IMPORTANCE),
        tags,
    })
}

/// Scalar top-level entries of a JSON `metadata` string, as `meta:` tags.
fn metadata_tags(raw: Option<&str>) -> Vec<String> {
    let Some(serde_json::Value::Object(map)) = raw.and_then(|r| serde_json::from_str(r).ok())
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (k, v) in map {
        let value = match v {
            serde_json::Value::String(s) => s,
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            _ => continue,
        };
        // #277 L4: the key comes from the store and becomes part of a tag.
        if !is_safe_token(&k) || value.is_empty() || value.chars().count() > MAX_META_VALUE_CHARS {
            continue;
        }
        out.push(format!("meta:{k}:{value}"));
        if out.len() == MAX_META_TAGS {
            break;
        }
    }
    out
}

/// Parse kuzu's timestamps: RFC 3339, or Python's naive `isoformat()` as UTC.
fn parse_timestamp(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"]
        .iter()
        .find_map(|f| NaiveDateTime::parse_from_str(s, f).ok())
        .map(|n| n.and_utc())
}

/// KG subject for a kuzu Entity.
pub fn entity_subject(entity_id: &str) -> String {
    format!("entity:{entity_id}")
}

/// KG subject for a drawer — the canonical `drawer:<uuid>` form `kg_extract` uses.
pub fn drawer_subject(id: uuid::Uuid) -> String {
    format!("drawer:{id}")
}

/// A triple with this importer's provenance and `valid_from = now`.
pub fn triple(subject: String, predicate: &str, object: String, confidence: Option<f64>) -> Triple {
    Triple {
        subject,
        predicate: predicate.to_string(),
        object,
        valid_from: Utc::now(),
        valid_to: None,
        confidence: confidence
            .map(|c| (c as f32).clamp(0.0, 1.0))
            .unwrap_or(DEFAULT_EDGE_CONFIDENCE),
        provenance: Some(ORIGIN_TAG.to_string()),
    }
}

/// `entity:<id> has_name <name>` and `entity:<id> entity_type <type>`.
///
/// Why: an Entity node's name and type are its only content, and a KG subject
/// with no triples of its own is invisible to KG queries.
pub fn entity_triples(entity: &KuzuEntityRow) -> Vec<Triple> {
    let Some(id) = entity.id.as_deref().filter(|i| !i.is_empty()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(name) = entity.name.as_deref().filter(|n| !n.trim().is_empty()) {
        out.push(triple(
            entity_subject(id),
            "has_name",
            name.to_string(),
            None,
        ));
    }
    if let Some(kind) = entity
        .entity_type
        .as_deref()
        .filter(|k| !k.trim().is_empty())
    {
        out.push(triple(
            entity_subject(id),
            "entity_type",
            kind.to_string(),
            None,
        ));
    }
    out
}

/// Predicate for a RELATES_TO edge: `relates_to:<relationship_type>`.
///
/// The fixed prefix means a relationship type copied from the store can never
/// equal a Tier S hot predicate (#4888). A type that is not a short
/// `[A-Za-z0-9_-]` token falls back to bare `relates_to` (#277 L4).
/// Test: `mapping_tags_identity_hash_and_columns`.
pub fn relates_to_predicate(relationship_type: Option<&str>) -> String {
    match relationship_type
        .map(str::trim)
        .filter(|t| is_safe_token(t))
    {
        Some(t) => format!("relates_to:{t}"),
        None => "relates_to".to_string(),
    }
}
