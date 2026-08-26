//! The methods that had no JSON-RPC equivalent before #6286.
//!
//! Why: `transport::rpc::dispatch` already routed the whole tool surface —
//! `palace_*`, `memory_*`, `kg_*`, the MCP protocol arms — and mounts whole
//! through [`RpcFallback`]. What it did NOT route was the roughly twenty
//! endpoints that existed only as axum routes: `/api/v1/status`, `/config`,
//! `/health`, the drawer CRUD, seven KG reads, the dream trio, `/activity`,
//! `/logs/tail`, `/admin/stop`, the async remember, chat and the three message
//! endpoints. Retiring the listener without folding those would delete
//! behaviour rather than move it.
//!
//! What: one submodule per former route file, each handler converted from an
//! axum extractor signature to `(&AppState, Params) -> Result<Value, ApiError>`
//! — a plain async function with no framework in it. [`super::uds::build_router`]
//! is what binds them to names.
//!
//! Why a separate module tree rather than more arms in `transport/rpc.rs`: that
//! file is already 574 lines against the 500 SLOC cap, and these handlers have
//! nothing to do with its envelope types.
//!
//! [`RpcFallback`]: trusty_common::uds::server::RpcFallback
//!
//! Test: `super::uds::tests` — `rpc_*` over a real socket.

pub mod activity;
pub mod admin;
pub mod chat;
pub mod health;
pub mod kg;
pub mod palaces;

#[cfg(test)]
#[path = "health_tests.rs"]
mod health_tests;

use serde::{Deserialize, Serialize};

use crate::attribution::{CreatorInfo, CreatorSource, HTTP_DEFAULT_CLIENT};

/// The palace `memory.health`'s round-trip probe writes into (#185).
///
/// Why: earlier revisions probed whichever palace happened to be first on disk,
/// so the check wrote — and, when recall failed, LEAKED — a drawer in a real
/// user-facing palace. The `__` prefix is this project's convention for a
/// system palace, which `MemoryService::list_palaces` filters out, so a leaked
/// drawer is confined somewhere the user never sees.
/// Test: `health_probe_palace_is_invisible`.
pub const HEALTH_PROBE_PALACE: &str = "__health_probe__";

/// The params of a method that takes no arguments.
///
/// Why: `RpcRouter::typed` decodes `params` into the handler's request type
/// before the handler runs, and `params` is absent — `Value::Null` — on a
/// well-formed call to a no-argument method. A plain unit struct refuses
/// `null`, so every `memory.status` call would answer `invalid_params`.
/// What: accepts anything and keeps nothing. A caller that sends a stray field
/// is not refused: these methods have no arguments to get wrong.
/// Test: `rpc_status_answers_with_no_params`.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct NoParams;

impl<'de> Deserialize<'de> for NoParams {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        serde::de::IgnoredAny::deserialize(deserializer)?;
        Ok(NoParams)
    }
}

/// The params of a method that names one palace and nothing else.
///
/// The id used to be a path segment; on this wire it is a field, so the eight
/// methods that took only `{id}` share one type.
#[derive(Debug, Clone, Deserialize)]
pub struct PalaceParams {
    /// Palace id.
    pub palace_id: String,
}

/// Who is calling, as the caller itself reports.
///
/// Why: attribution used to arrive in `X-Trusty-Client-*` headers, and a
/// JSON-RPC frame has no header channel. The fields move into `params`, keeping
/// the rule the headers encoded (DOC-53 §4.3): the daemon never reads its OWN
/// environment for caller identity, because it is one shared process serving
/// every attached session. What it knows is what the caller sent.
/// What: all three optional, mirroring the headers' optionality; [`creator`]
/// applies the same precedence `CreatorInfo::new_for_caller` always did.
/// Test: `rpc_drawer_create_attributes_the_caller_it_was_given`.
///
/// [`creator`]: CallerParams::creator
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CallerParams {
    /// Client name, e.g. `trusty-console`. Defaults to [`HTTP_DEFAULT_CLIENT`].
    #[serde(default)]
    pub client: Option<String>,
    /// The caller's working directory, when it has one.
    #[serde(default)]
    pub cwd: Option<String>,
    /// The caller's workstream, when it knows it. Wins over `cwd`.
    #[serde(default)]
    pub workstream: Option<String>,
}

impl CallerParams {
    /// Build the attribution this call writes.
    ///
    /// `CreatorSource::Http` is retained deliberately: it is the value already
    /// persisted in every `creator:source=` tag written by this path, and
    /// changing it would split one caller class across two labels in the stored
    /// history for no gain a reader gets.
    pub fn creator(&self) -> CreatorInfo {
        let client = self
            .client
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(HTTP_DEFAULT_CLIENT)
            .to_string();
        CreatorInfo::new_for_caller(
            client,
            CreatorSource::Http,
            self.cwd.as_deref().filter(|s| !s.is_empty()),
            self.workstream.as_deref().filter(|s| !s.is_empty()),
        )
    }
}

/// Parse an optional ISO-8601 timestamp, refusing a value it cannot read.
///
/// Why: `since` / `until` are caller-supplied. Dropping an unparseable one
/// silently would return a correct-looking page filtered by something other
/// than what was asked for.
/// What: `None` and `""` are absent; anything else must be RFC 3339.
/// Test: `rpc_activity_refuses_an_unparseable_since`.
pub fn parse_iso_or_bad_request(
    s: Option<&str>,
    field: &str,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, super::api_error::ApiError> {
    match s {
        None | Some("") => Ok(None),
        Some(raw) => chrono::DateTime::parse_from_rfc3339(raw)
            .map(|dt| Some(dt.with_timezone(&chrono::Utc)))
            .map_err(|e| {
                super::api_error::ApiError::bad_request(format!("invalid {field} (RFC 3339): {e}"))
            }),
    }
}

/// Serialise a handler's own response type into the `result` half.
///
/// Why: every folded handler answers `Value` so one registration shape covers
/// all of them, and a handler whose response will not serialise is a programmer
/// error on this side of the wire rather than anything the caller sent.
pub fn to_value<T: Serialize>(value: T) -> Result<serde_json::Value, super::api_error::ApiError> {
    serde_json::to_value(value)
        .map_err(|e| super::api_error::ApiError::internal(format!("serialize response: {e}")))
}
