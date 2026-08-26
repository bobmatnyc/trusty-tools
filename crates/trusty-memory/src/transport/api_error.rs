//! The failure type every folded method reports, and its one mapping onto the
//! wire (#6286).
//!
//! Why: these handlers used to be axum handlers, and `ApiError` used to be an
//! HTTP status plus a message with an `IntoResponse` impl. ADR-0032 retired the
//! listener, so the status code has nothing left to set. What the status
//! ENCODED is still worth keeping — "the palace is not there" and "the palace
//! could not be read" are different answers and #5549 is the issue that proves
//! a caller acts on the difference — so the type survives as a failure KIND and
//! the single [`From<ApiError> for RpcError`] below is where a kind becomes a
//! code.
//!
//! What: [`ApiError`] carries an [`ErrorKind`] and a message; [`open_handle`]
//! is the one palace lookup every method that names a palace by id runs.
//!
//! Test: `api_error_kind_maps_to_its_rpc_code`,
//! `api_error_message_survives_the_conversion`.

use trusty_common::memory_core::palace::PalaceId;
use trusty_common::memory_core::PalaceRegistry;
#[cfg_attr(not(test), allow(unused_imports))]
use trusty_common::uds::server::{
    RpcError, CODE_INTERNAL_ERROR, CODE_INVALID_PARAMS, CODE_METHOD_NOT_FOUND,
};

use crate::AppState;

/// The request named something that is not there.
///
/// Why: `CODE_METHOD_NOT_FOUND` says the METHOD is unknown, which is a
/// different fact from a well-formed call naming a palace, drawer or session
/// that does not exist. trusty-analyze took the same code for the same
/// distinction (#5049), and a caller that already reads `-32004` from one
/// daemon reads it the same way here.
pub const CODE_NOT_FOUND: i64 = -32004;

/// The request is well formed and refused anyway.
///
/// Covers both a state conflict — deleting a palace that still holds drawers
/// without `force` — and a refusal by the multi-tenant authz seam (#1714). The
/// two were 409 and 403 over HTTP; on this wire the caller's next move is the
/// same for both, which is to read the message.
pub const CODE_REFUSED: i64 = -32006;

/// Why one call failed, at the granularity a caller acts on.
///
/// Why: not the HTTP status set it replaces. `NotFound` and `Internal` were 404
/// and 500 and stay distinct because #5549 turns on exactly that distinction;
/// `Conflict` and `Forbidden` were 409 and 403 and collapse into `Refused`
/// because nothing downstream branched on which.
/// Test: `api_error_kind_maps_to_its_rpc_code`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The caller's arguments do not describe a request this method can run.
    BadRequest,
    /// Well formed, and it names something absent.
    NotFound,
    /// Well formed, understood, and refused — a state conflict or an authz
    /// denial.
    Refused,
    /// The daemon could not complete the call.
    Internal,
}

impl ErrorKind {
    /// The JSON-RPC code this kind crosses the wire as.
    fn code(self) -> i64 {
        match self {
            Self::BadRequest => CODE_INVALID_PARAMS,
            Self::NotFound => CODE_NOT_FOUND,
            Self::Refused => CODE_REFUSED,
            Self::Internal => CODE_INTERNAL_ERROR,
        }
    }
}

/// One folded method's failure.
///
/// Why: the handlers below return `Result<Value, ApiError>` rather than
/// `Result<Value, RpcError>` so the code mapping lives in one place instead of
/// at every `return`. That is the shape trusty-analyze's `ApiError` took
/// through the same migration.
/// What: a kind and a message. The message reaches the caller verbatim.
/// Test: `api_error_message_survives_the_conversion`.
#[derive(Debug, Clone)]
pub struct ApiError {
    /// Which failure this is.
    pub kind: ErrorKind,
    /// What to tell the caller.
    pub message: String,
}

impl ApiError {
    /// The caller's arguments are wrong.
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::BadRequest,
            message: msg.into(),
        }
    }

    /// The thing named is not there.
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::NotFound,
            message: msg.into(),
        }
    }

    /// Understood, and refused. Formerly 409 Conflict.
    pub fn conflict(msg: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Refused,
            message: msg.into(),
        }
    }

    /// Understood, and denied. Formerly 403 Forbidden (#1714).
    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Refused,
            message: msg.into(),
        }
    }

    /// Structurally valid and semantically unacceptable — content too short to
    /// be worth storing (#466). Formerly 422.
    pub fn unprocessable(msg: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::BadRequest,
            message: msg.into(),
        }
    }

    /// The daemon could not complete the call.
    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Internal,
            message: msg.into(),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

/// The one place a failure kind becomes a JSON-RPC code (#6286).
///
/// Every folded method's `?` runs through here, so a change to the mapping is a
/// change to the whole surface rather than to whichever handler was edited last.
///
/// Test: `api_error_kind_maps_to_its_rpc_code`.
impl From<ApiError> for RpcError {
    fn from(e: ApiError) -> Self {
        RpcError::new(e.kind.code(), e.message)
    }
}

impl From<crate::service::ServiceError> for ApiError {
    fn from(e: crate::service::ServiceError) -> Self {
        match e {
            crate::service::ServiceError::BadRequest(m) => ApiError::bad_request(m),
            crate::service::ServiceError::NotFound(m) => ApiError::not_found(m),
            crate::service::ServiceError::Conflict(m) => ApiError::conflict(m),
            crate::service::ServiceError::Internal(m) => ApiError::internal(m),
            crate::service::ServiceError::Forbidden(m) => ApiError::forbidden(m),
        }
    }
}

/// Open a palace handle by id, telling absence apart from an open that failed.
///
/// Why: every method that references a palace by id runs the same registry
/// lookup, and #5549 (ADR-0045) is what makes the distinction load-bearing:
/// mapping every open failure to "not found" sent an operator looking for a
/// deleted palace when what they had was a denied read or a jammed redb lock.
/// What: calls `PalaceRegistry::open_palace`, then asks
/// `PalaceRegistry::open_error_is_absent` which failure it got —
/// [`ApiError::not_found`] only for a genuine absence, [`ApiError::internal`]
/// otherwise.
/// Test: `unreadable_palace_is_internal_not_not_found_at_open_handle`.
pub fn open_handle(
    state: &AppState,
    id: &str,
) -> Result<std::sync::Arc<trusty_common::memory_core::PalaceHandle>, ApiError> {
    state
        .registry
        .open_palace(&state.data_root, &PalaceId::new(id))
        .map_err(|e| {
            if PalaceRegistry::open_error_is_absent(&e) {
                ApiError::not_found(format!("palace not found: {id} ({e:#})"))
            } else {
                ApiError::internal(format!("palace could not be loaded: {id} ({e:#})"))
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the mapping is the whole contract this type exists to hold, and a
    /// silent change to it would look identical on the happy path. The two
    /// codes that are not JSON-RPC standard are the ones a consumer has to
    /// learn, so both are asserted by value rather than by name.
    /// Test: itself.
    #[test]
    fn api_error_kind_maps_to_its_rpc_code() {
        let cases = [
            (ApiError::bad_request("x"), CODE_INVALID_PARAMS),
            (ApiError::unprocessable("x"), CODE_INVALID_PARAMS),
            (ApiError::not_found("x"), -32004),
            (ApiError::conflict("x"), -32006),
            (ApiError::forbidden("x"), -32006),
            (ApiError::internal("x"), CODE_INTERNAL_ERROR),
        ];
        for (error, expected) in cases {
            let kind = error.kind;
            let rpc: RpcError = error.into();
            assert_eq!(rpc.code, expected, "{kind:?} must map to {expected}");
        }
        // A method-not-found is the router's to report, never a handler's; this
        // asserts no kind claims that code.
        assert!(
            !cases_map_to(CODE_METHOD_NOT_FOUND),
            "no handler failure may impersonate method_not_found"
        );
    }

    fn cases_map_to(code: i64) -> bool {
        [
            ErrorKind::BadRequest,
            ErrorKind::NotFound,
            ErrorKind::Refused,
            ErrorKind::Internal,
        ]
        .iter()
        .any(|k| k.code() == code)
    }

    /// Why: the message is what an operator reads; a conversion that dropped
    /// or rewrote it would leave every failure looking the same on the wire.
    /// Test: itself.
    #[test]
    fn api_error_message_survives_the_conversion() {
        let rpc: RpcError = ApiError::not_found("palace not found: alpha").into();
        assert_eq!(rpc.message, "palace not found: alpha");
    }

    /// Why (#5549, ADR-0045): mapping every open failure to `NotFound` sent an
    /// operator looking for a deleted palace when what they had was a palace
    /// that could not be READ. The two answers call for different next moves,
    /// and the only thing separating them is this branch.
    /// What: creates a palace directory the process cannot enter (`0o000`),
    /// then opens it by id and asserts the failure is `Internal` rather than
    /// `NotFound` — the palace is plainly there.
    /// Test: itself.
    #[tokio::test]
    async fn unreadable_palace_is_internal_not_not_found_at_open_handle() {
        use std::os::unix::fs::PermissionsExt as _;

        // Running as root defeats the mode bits — the open would succeed and
        // the test would assert nothing.
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("SKIP: running as root, so 0o000 does not deny this process");
            return;
        }

        let tmp = tempfile::tempdir().expect("tempdir");
        let state = AppState::new(tmp.path().to_path_buf());
        let dir = tmp.path().join("unreadable");
        std::fs::create_dir_all(&dir).expect("create the palace directory");
        std::fs::write(dir.join("palace.json"), "{}").expect("seed metadata");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000))
            .expect("deny access");

        let failure = open_handle(&state, "unreadable")
            .err()
            .expect("an unreadable palace cannot be opened");

        // Restore before the tempdir drop, or the cleanup cannot descend.
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));

        assert_eq!(
            failure.kind,
            ErrorKind::Internal,
            "a palace that is present but unreadable must not report as absent: {}",
            failure.message
        );
    }
}
