//! `memory.protocol` — the daemon's wire protocol version (#9288).
//!
//! Why: ADR-0066 keeps the daemon socket out of the 1.x compatibility contract,
//! so the wire may change between releases. A client that meets a daemon from
//! another release must learn so from one integer, instead of misparsing its
//! replies. This is ADR-0007's monotonic-integer pattern applied to the socket.
//!
//! What: [`PROTOCOL_VERSION`] and the handler that reports it in the shared
//! [`MemoryProtocolInfo`] shape. The client half is
//! `trusty_common::memory_rpc::check_memory_protocol_at`.
//!
//! Test: `protocol_version_is_pinned_to_its_wire_surface` in
//! `protocol_tests.rs`; `shared_client_handshake_reads_the_real_daemon_as_supported`
//! in `tests/uds_consumer_contract.rs`.

use serde_json::Value;
use trusty_common::memory_rpc::MemoryProtocolInfo;

use super::NoParams;
use crate::transport::api_error::ApiError;
use crate::AppState;

/// The daemon's wire protocol version (#9288).
///
/// Why: the one integer a client compares against
/// `trusty_common::memory_rpc::SUPPORTED_MEMORY_PROTOCOLS`.
/// What: bump it, and only it, when a change breaks a client built against
/// the previous wire — a method removed or renamed, a params or result field
/// removed, renamed or retyped, a frame budget or error code changed. Adding a
/// method or an optional field is not a bump (ADR-0007 rule 3). A bump moves
/// the client range's end in the same change and pins the new surface.
/// Test: `protocol_version_is_pinned_to_its_wire_surface`,
/// `memory_rpc_protocol_range_accepts_the_daemon`.
pub const PROTOCOL_VERSION: u64 = 1;

/// Answer `memory.protocol` with [`PROTOCOL_VERSION`] and this build's version.
///
/// # Errors
///
/// Only a serialisation failure, which this fixed shape cannot produce.
pub async fn protocol(_state: &AppState, _params: NoParams) -> Result<Value, ApiError> {
    super::to_value(MemoryProtocolInfo::new(
        PROTOCOL_VERSION,
        Some(env!("CARGO_PKG_VERSION").to_string()),
    ))
}
