//! Experiment wire contracts, independent of authoritative memory storage.
use serde::{Deserialize, Serialize};

pub const VERSION: &str = "memory-deterministic-v2";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Treatment {
    Raw,
    RepairedRaw,
    Context,
    Chunks,
    Temporal,
    Kg,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    #[serde(deserialize_with = "nullable_context")]
    pub context_tokens: usize,
    #[serde(deserialize_with = "nullable_chunk")]
    pub chunk_tokens: usize,
    #[serde(deserialize_with = "nullable_freshness")]
    pub freshness_weight: f64,
    #[serde(deserialize_with = "nullable_kg")]
    pub kg_weight: f64,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            context_tokens: 64,
            chunk_tokens: 256,
            freshness_weight: 0.05,
            kg_weight: 0.15,
        }
    }
}

fn nullable_context<'de, D: serde::Deserializer<'de>>(de: D) -> Result<usize, D::Error> {
    Ok(Option::<usize>::deserialize(de)?.unwrap_or(64))
}
fn nullable_chunk<'de, D: serde::Deserializer<'de>>(de: D) -> Result<usize, D::Error> {
    Ok(Option::<usize>::deserialize(de)?.unwrap_or(256))
}
fn nullable_freshness<'de, D: serde::Deserializer<'de>>(de: D) -> Result<f64, D::Error> {
    Ok(Option::<f64>::deserialize(de)?.unwrap_or(0.05))
}
fn nullable_kg<'de, D: serde::Deserializer<'de>>(de: D) -> Result<f64, D::Error> {
    Ok(Option::<f64>::deserialize(de)?.unwrap_or(0.15))
}

fn null_default<'de, D, T>(de: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(de)?.unwrap_or_default())
}
fn importance() -> f64 {
    0.5
}
fn top_k() -> usize {
    5
}
fn nullable_importance<'de, D: serde::Deserializer<'de>>(de: D) -> Result<f64, D::Error> {
    Ok(Option::<f64>::deserialize(de)?.unwrap_or_else(importance))
}
fn nullable_top_k<'de, D: serde::Deserializer<'de>>(de: D) -> Result<usize, D::Error> {
    Ok(Option::<usize>::deserialize(de)?.unwrap_or_else(top_k))
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum Kind {
    UserFact,
    SessionEvent,
    AgentNote,
    Commit,
    #[default]
    Unknown,
    Task,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct Link {
    pub target_id: String,
    pub predicate: String,
    #[serde(default)]
    pub valid_from: Option<String>,
    #[serde(default)]
    pub valid_to: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DrawerInput {
    pub id: String,
    pub scope: String,
    pub room: String,
    pub body: String,
    #[serde(default, deserialize_with = "null_default")]
    pub tags: Vec<String>,
    #[serde(default)]
    pub fact_key: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub effective_at: Option<String>,
    #[serde(default)]
    pub verified_at: Option<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub valid_to: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub kind: Kind,
    #[serde(default = "importance", deserialize_with = "nullable_importance")]
    pub importance: f64,
    #[serde(default, deserialize_with = "null_default")]
    pub aliases: Vec<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub links: Vec<Link>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Mutation {
    Upsert {
        revision: u64,
        drawer: Box<DrawerInput>,
    },
    Remove {
        revision: u64,
        scope: String,
        id: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    pub max_documents: usize,
    pub max_bytes: usize,
    pub max_edges: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Current,
    Asof,
    General,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub id: String,
    pub text: String,
    pub scope: String,
    pub mode: Mode,
    pub as_of: String,
    #[serde(default)]
    pub knowledge_cutoff: Option<String>,
    #[serde(default = "top_k", deserialize_with = "nullable_top_k")]
    pub top_k: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema_version: u32,
    pub request_id: String,
    pub treatment: Treatment,
    pub as_of: String,
    #[serde(default)]
    pub state: Option<State>,
    #[serde(default, deserialize_with = "null_default")]
    pub policy: Policy,
    #[serde(default, deserialize_with = "null_default")]
    pub mutations: Vec<Mutation>,
    pub maintenance: Budget,
    #[serde(default, deserialize_with = "null_default")]
    pub queries: Vec<Query>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub drawer: DrawerInput,
    pub revision: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Tombstone {
    pub scope: String,
    pub id: String,
    pub revision: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub doc_id: String,
    pub text: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Derived {
    pub doc_id: String,
    pub scope: String,
    pub id: String,
    pub revision: u64,
    pub fingerprint: String,
    pub body_digest: String,
    pub byte_start: usize,
    pub byte_end: usize,
    pub line_start: usize,
    pub line_end: usize,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub schema_version: u32,
    pub policy_version: String,
    pub treatment: Treatment,
    #[serde(default, deserialize_with = "null_default")]
    pub policy: Policy,
    pub generation: u64,
    pub sources: Vec<Source>,
    pub tombstones: Vec<Tombstone>,
    pub snapshot: Vec<Document>,
    pub derived: Vec<Derived>,
    pub cursor: Option<String>,
}
impl State {
    pub fn new(treatment: Treatment, policy: Policy) -> Self {
        Self {
            schema_version: 1,
            policy_version: VERSION.into(),
            treatment,
            policy,
            generation: 0,
            sources: vec![],
            tombstones: vec![],
            snapshot: vec![],
            derived: vec![],
            cursor: None,
        }
    }
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Maintenance {
    pub processed: usize,
    pub rewritten: usize,
    pub removed: usize,
    pub bytes: usize,
    pub edges: usize,
    pub pending: usize,
    pub fresh: usize,
    pub total: usize,
    pub cursor: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Hit {
    pub id: String,
    pub scope: String,
    pub rank: usize,
    pub score: f64,
    pub bm25_score: f32,
    pub excerpt: String,
    pub body_digest: String,
    pub revision: u64,
    pub byte_start: usize,
    pub byte_end: usize,
    pub line_start: usize,
    pub line_end: usize,
    pub created_at: String,
    pub effective_at: Option<String>,
    pub verified_at: Option<String>,
    pub expires_at: Option<String>,
    pub valid_to: Option<String>,
    pub status: String,
    pub freshness_basis: String,
    pub origins: Vec<String>,
    pub index_fresh: bool,
}
#[derive(Debug, Serialize)]
pub struct QueryResult {
    pub id: String,
    pub hits: Vec<Hit>,
}
#[derive(Debug, Serialize)]
pub struct Response {
    pub schema_version: u32,
    pub request_id: String,
    pub ok: bool,
    pub state: State,
    pub maintenance: Maintenance,
    pub results: Vec<QueryResult>,
}
#[derive(Debug, thiserror::Error)]
#[error("{code}: {message}")]
pub struct ExperimentError {
    pub code: &'static str,
    pub message: String,
    pub path: Option<String>,
}
impl ExperimentError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            path: None,
        }
    }
}
pub type Result<T, E = ExperimentError> = std::result::Result<T, E>;
