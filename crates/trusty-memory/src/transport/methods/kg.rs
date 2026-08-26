//! Knowledge-graph reads and the dream trio (#6286).
//!
//! Why: `kg_query`, `kg_assert` and `kg_gaps` were already tool methods, so
//! only the reads the KG Explorer added on top of them had no JSON-RPC
//! equivalent — the paged `all` view, the three graph shapes, the count, the
//! counted subject list, the one-triple delete — plus the two dream-status
//! reads and the on-demand run.
//! What: every handler delegates to `MemoryService`; the clamps that used to
//! sit in the axum layer stay here, because they are what stops a caller
//! pulling a whole graph by asking for one.
//! Test: `super::super::uds::tests` — `rpc_kg_*`, `rpc_dream_*`.

use serde::Deserialize;
use serde_json::{json, Value};
use trusty_common::memory_core::store::kg::ExpandDirection;

use crate::service::core_kg::{DEFAULT_KG_LIST_LIMIT, MAX_KG_LIST_LIMIT};
use crate::transport::api_error::ApiError;
use crate::AppState;

use super::{to_value, NoParams, PalaceParams};

pub use crate::service::{DreamStatusPayload, KgGraphPayload, KgNeighborsPayload, KgSeedPayload};

fn default_kg_list_limit() -> usize {
    DEFAULT_KG_LIST_LIMIT
}

/// Params for the two bounded subject/triple listings.
#[derive(Debug, Deserialize)]
pub struct KgListParams {
    /// Palace to read.
    pub palace_id: String,
    /// Page size, clamped to `[1, MAX_KG_LIST_LIMIT]`.
    #[serde(default = "default_kg_list_limit")]
    pub limit: usize,
    /// Rows to skip. Ignored by `memory.kg_subjects_with_counts`.
    #[serde(default)]
    pub offset: usize,
}

/// `memory.kg_subjects_with_counts` — distinct subjects with their triple
/// counts, in one grouped pass rather than one query per subject.
pub async fn kg_subjects_with_counts(
    state: &AppState,
    params: KgListParams,
) -> Result<Value, ApiError> {
    let limit = params.limit.clamp(1, MAX_KG_LIST_LIMIT);
    let rows = crate::service::MemoryService::new(state.clone())
        .kg_list_subjects_with_counts(&params.palace_id, limit)
        .await?;
    Ok(Value::Array(
        rows.into_iter()
            .map(|(subject, count)| json!({ "subject": subject, "count": count }))
            .collect(),
    ))
}

/// `memory.kg_all` — every active triple, paged.
///
/// `kg_query` needs a subject; this is the view that does not.
pub async fn kg_all(state: &AppState, params: KgListParams) -> Result<Value, ApiError> {
    let limit = params.limit.clamp(1, MAX_KG_LIST_LIMIT);
    to_value(
        crate::service::MemoryService::new(state.clone())
            .kg_list_all(&params.palace_id, limit, params.offset)
            .await?,
    )
}

/// `memory.kg_count` — how many triples are currently active.
///
/// A failed store read is an error, not `{"active": 0}` (#5384): a badge cannot
/// tell a real zero apart from a count that was never read.
pub async fn kg_count(state: &AppState, params: PalaceParams) -> Result<Value, ApiError> {
    let active = crate::service::MemoryService::new(state.clone())
        .kg_count(&params.palace_id)
        .await?;
    Ok(json!({ "active": active }))
}

/// `memory.kg_graph` — the whole active graph, capped and honest about it.
///
/// The payload carries `returned_triple_count` / `active_triple_count` /
/// `truncated`, so a caller that got a cap knows it did.
pub async fn kg_graph(state: &AppState, params: PalaceParams) -> Result<Value, ApiError> {
    to_value(
        crate::service::MemoryService::new(state.clone())
            .kg_graph(&params.palace_id)
            .await?,
    )
}

/// Default seed size (#4670).
///
/// The graph layout is an O(n²) force simulation; 75 nodes reaches into the
/// mid-tier hubs while leaving ~2.5x headroom under the max before layout cost
/// becomes noticeable.
const DEFAULT_KG_SEED_LIMIT: usize = 75;

/// Hard ceiling on the seed size — past 200 the picture is unreadable whatever
/// the performance.
const MAX_KG_SEED_LIMIT: usize = 200;

fn default_kg_seed_limit() -> usize {
    DEFAULT_KG_SEED_LIMIT
}

/// Params for `memory.kg_graph_seed`.
#[derive(Debug, Deserialize)]
pub struct KgSeedParams {
    /// Palace to read.
    pub palace_id: String,
    /// How many nodes to seed with, clamped to `[1, MAX_KG_SEED_LIMIT]`.
    #[serde(default = "default_kg_seed_limit")]
    pub limit: usize,
}

/// `memory.kg_graph_seed` — the top-N nodes by degree, plus palace-wide totals
/// (#4670).
pub async fn kg_graph_seed(state: &AppState, params: KgSeedParams) -> Result<Value, ApiError> {
    let limit = params.limit.clamp(1, MAX_KG_SEED_LIMIT);
    to_value(
        crate::service::MemoryService::new(state.clone())
            .kg_graph_seed(&params.palace_id, limit)
            .await?,
    )
}

/// Maximum BFS depth, matching trusty-search's `graph_neighbors_handler` so an
/// operator who learned one already knows the other.
const MAX_KG_NEIGHBOR_HOPS: usize = 4;

fn default_kg_neighbor_hops() -> usize {
    1
}

/// Params for `memory.kg_graph_neighbors`.
#[derive(Debug, Deserialize)]
pub struct KgNeighborsParams {
    /// Palace to read.
    pub palace_id: String,
    /// The node to expand from.
    pub node: String,
    /// `in` | `out` | `both`; anything else is refused.
    #[serde(default)]
    pub direction: Option<String>,
    /// BFS depth, clamped to `[1, MAX_KG_NEIGHBOR_HOPS]`.
    #[serde(default = "default_kg_neighbor_hops")]
    pub max_hops: usize,
}

/// `memory.kg_graph_neighbors` — click-to-expand, and the only read that can
/// answer "what points AT this node" (#4670).
///
/// `kg_query` is a subject prefix scan and never reads the object side, so the
/// incoming half of every palace graph is reachable only here.
pub async fn kg_graph_neighbors(
    state: &AppState,
    params: KgNeighborsParams,
) -> Result<Value, ApiError> {
    let direction = match params.direction.as_deref().unwrap_or("both") {
        "in" | "inbound" => ExpandDirection::In,
        "out" | "outbound" => ExpandDirection::Out,
        "both" => ExpandDirection::Both,
        other => {
            return Err(ApiError::bad_request(format!(
                "direction must be in|out|both, got {other:?}"
            )))
        }
    };
    let max_hops = params.max_hops.clamp(1, MAX_KG_NEIGHBOR_HOPS);
    to_value(
        crate::service::MemoryService::new(state.clone())
            .kg_neighbors(&params.palace_id, &params.node, direction, max_hops)
            .await?,
    )
}

// ---------------------------------------------------------------------------
// Triple id encode/decode + delete
// ---------------------------------------------------------------------------

/// Separator inside an opaque triple id.
///
/// A triple is keyed by `(subject, predicate, object)`; encoding all three as
/// one string is what lets a caller name exactly one row. `\0` is safe because
/// no component ever contains a null byte — and [`encode_triple_id`] refuses
/// one that does rather than producing an ambiguous id (#1102).
const TRIPLE_ID_SEPARATOR: u8 = 0x00;

/// Encode a `(subject, predicate, object)` triple as a URL-safe base64 id.
///
/// # Errors
///
/// When any component contains the null-byte separator, which would make the
/// encoding non-injective.
///
/// Test: `decode_triple_id_round_trips`, `encode_triple_id_rejects_null_byte`.
pub fn encode_triple_id(subject: &str, predicate: &str, object: &str) -> Result<String, String> {
    use base64::Engine as _;
    for (field, value) in [
        ("subject", subject),
        ("predicate", predicate),
        ("object", object),
    ] {
        if value.as_bytes().contains(&TRIPLE_ID_SEPARATOR) {
            return Err(format!(
                "{field} must not contain the null-byte separator (\\0); got {value:?}"
            ));
        }
    }
    let mut buf = Vec::with_capacity(subject.len() + predicate.len() + object.len() + 2);
    buf.extend_from_slice(subject.as_bytes());
    buf.push(TRIPLE_ID_SEPARATOR);
    buf.extend_from_slice(predicate.as_bytes());
    buf.push(TRIPLE_ID_SEPARATOR);
    buf.extend_from_slice(object.as_bytes());
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&buf))
}

/// Why a triple id failed to decode, so the handler can answer differently.
///
/// A two-field id is not merely unparseable — it is the PREVIOUS format, and
/// answering it with the same "not found" a garbage id gets would read as
/// "already deleted" to a caller whose request was never understood.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TripleIdError {
    /// Not base64url, or not a three-field `\0`-separated list.
    Malformed,
    /// Decodes to `(subject, predicate)` — the pre-fix form, which cannot name
    /// a single triple.
    LegacyPair,
}

/// Decode a URL-safe base64 triple id back to `(subject, predicate, object)`.
///
/// # Errors
///
/// [`TripleIdError::LegacyPair`] for a two-field payload,
/// [`TripleIdError::Malformed`] for everything else.
///
/// Test: `decode_triple_id_round_trips`,
/// `decode_triple_id_returns_none_for_invalid_input`,
/// `decode_triple_id_rejects_the_legacy_pair_form`.
pub fn decode_triple_id(id: &str) -> Result<(String, String, String), TripleIdError> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(id)
        .map_err(|_| TripleIdError::Malformed)?;
    let parts = bytes
        .split(|&b| b == TRIPLE_ID_SEPARATOR)
        .map(|part| String::from_utf8(part.to_vec()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| TripleIdError::Malformed)?;
    match <[String; 3]>::try_from(parts) {
        Ok([subject, predicate, object]) => Ok((subject, predicate, object)),
        Err(parts) if parts.len() == 2 => Err(TripleIdError::LegacyPair),
        Err(_) => Err(TripleIdError::Malformed),
    }
}

/// Params for `memory.kg_delete_triple`.
#[derive(Debug, Deserialize)]
pub struct DeleteTripleParams {
    /// Palace holding the triple.
    pub palace_id: String,
    /// `base64url(subject\0predicate\0object)`.
    pub triple_id: String,
}

/// `memory.kg_delete_triple` — close exactly one active triple (#278).
///
/// The id carries the object as well as the subject and predicate, so the
/// delete targets one row rather than every object at a pair.
pub async fn kg_delete_triple(
    state: &AppState,
    params: DeleteTripleParams,
) -> Result<Value, ApiError> {
    let (subject, predicate, object) = match decode_triple_id(&params.triple_id) {
        Ok(triple) => triple,
        Err(TripleIdError::LegacyPair) => {
            return Err(ApiError::bad_request(
                "triple id names only (subject, predicate) — it must encode \
                 base64url(subject\\0predicate\\0object) so the delete targets one triple",
            ))
        }
        Err(TripleIdError::Malformed) => {
            return Err(ApiError::not_found(
                "invalid triple id — expected base64url(subject\\0predicate\\0object)",
            ))
        }
    };
    let closed = crate::service::MemoryService::new(state.clone())
        .kg_retract_triple(&params.palace_id, &subject, &predicate, &object)
        .await?;
    if closed > 0 {
        Ok(json!({ "closed": closed }))
    } else {
        Err(ApiError::not_found(format!(
            "no active triple with subject={subject:?} predicate={predicate:?} \
             object={object:?} in palace {:?}",
            params.palace_id
        )))
    }
}

// ---------------------------------------------------------------------------
// Dream cycle
// ---------------------------------------------------------------------------

/// `memory.dream_status` — aggregate dream-cycle status across every palace.
pub async fn dream_status(state: &AppState, _params: NoParams) -> Result<Value, ApiError> {
    to_value(
        crate::service::MemoryService::new(state.clone())
            .dream_status_aggregate()
            .await,
    )
}

/// `memory.palace_dream_status` — dream-cycle status for one palace.
pub async fn palace_dream_status(
    state: &AppState,
    params: PalaceParams,
) -> Result<Value, ApiError> {
    to_value(
        crate::service::MemoryService::new(state.clone())
            .dream_status_for_palace(&params.palace_id)
            .await?,
    )
}

/// `memory.dream_run` — trigger a consolidation pass now.
///
/// Answers the aggregate status after the run, so a caller does not need a
/// second call to see what changed.
pub async fn dream_run(state: &AppState, _params: NoParams) -> Result<Value, ApiError> {
    to_value(
        crate::service::MemoryService::new(state.clone())
            .dream_run()
            .await?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the id is the only thing naming the row a delete closes, so an
    /// encoding that did not round-trip would close the wrong triple.
    /// Test: itself.
    #[test]
    fn decode_triple_id_round_trips() {
        let id = encode_triple_id("s", "p", "o").expect("encode");
        assert_eq!(
            decode_triple_id(&id).expect("decode"),
            ("s".into(), "p".into(), "o".into())
        );
    }

    /// Why: a null byte in a component would make two distinct triples encode
    /// to the same id, and the caller could not tell which one it deleted.
    /// Test: itself.
    #[test]
    fn encode_triple_id_rejects_null_byte() {
        assert!(encode_triple_id("a\0b", "p", "o").is_err());
    }

    /// Why: the pre-fix two-field form names a PAIR, and answering it with
    /// "not found" would read as "already deleted".
    /// Test: itself.
    #[test]
    fn decode_triple_id_rejects_the_legacy_pair_form() {
        use base64::Engine as _;
        let legacy = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"s\0p");
        assert_eq!(
            decode_triple_id(&legacy).expect_err("a pair is not a triple"),
            TripleIdError::LegacyPair
        );
    }

    /// Why: garbage in must not decode to a triple that happens to exist.
    /// Test: itself.
    #[test]
    fn decode_triple_id_returns_none_for_invalid_input() {
        assert_eq!(
            decode_triple_id("!!!not base64!!!").expect_err("not decodable"),
            TripleIdError::Malformed
        );
    }
}
