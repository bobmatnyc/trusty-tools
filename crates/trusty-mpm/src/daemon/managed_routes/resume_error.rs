//! Typed resume-failure mapping shared by the HTTP and MCP resume transports.
//!
//! Why: extracted from `lifecycle.rs` (#2577 review — the enum split needed to
//! fix the nonexistent-`tm session rm` remedy bug pushed `lifecycle.rs` over
//! its frozen 500-SLOC-cap allowlist budget). Isolating the error-to-status
//! mapping in its own file also groups a genuinely separate concern —
//! "what HTTP shape does this resume failure take" — away from
//! `resume_managed`'s I/O orchestration.
//! What: [`ResumeManagedError`] (the typed enum resume failures map into),
//! its `From<ManagedError>` conversion, and [`unresumable_response`] (the
//! shared 422-response builder used by the `WorkspaceGone`/`PaneGone` match
//! arms in `mod.rs`'s HTTP handler).
//! Test: `resume_managed_typed_*` in `tests/session_manager_mvp.rs` drive the
//! 404/409/422 paths through the typed value; this module's own `tests`
//! submodule pins the `From` mapping and the response-header plumbing
//! directly.

use axum::http::StatusCode;
use axum::response::IntoResponse;

use crate::session_manager::ManagedError;

/// Typed failure modes for [`super::lifecycle::resume_managed`], shared across
/// transports.
///
/// Why: the prior design mapped resume failures to HTTP status codes by
/// substring-matching the `Display` string (`msg.contains("invalid state
/// transition")` → 409, `msg.contains("session not found")` → 404), which
/// silently regressed to 500 the moment any error wording changed. A typed enum
/// lets the HTTP handler match on variants (→ 404/409/500) with no stringly-typed
/// coupling, and lets the MCP path render a stable `Display` string whose
/// "not found" substring the existing MCP tests rely on.
/// What: five variants — `NotFound` (the id is absent), `InvalidState` (the
/// session is not `Stopped`/`Errored`, carrying the descriptive reason),
/// `WorkspaceGone` and `PaneGone` (both operator-actionable on-disk
/// preconditions that make a resume impossible even though the request is
/// well-formed — split into DISTINCT variants, not one shared `Unresumable`,
/// because their safe remedies differ: see each variant's doc), and `Other`
/// (any remaining genuinely-internal failure: store/I-O). The `Display`
/// strings are chosen so the not-found variant still contains the literal
/// "not found".
/// Test: `resume_managed_typed_*` in tests/session_manager_mvp.rs drive the
/// 404/409/422 paths through the typed value (no `Display` matching), and the
/// MCP `session_resume_unknown_id_errors` test asserts the rendered string.
#[derive(Debug, thiserror::Error)]
pub enum ResumeManagedError {
    /// The requested session id was not present in the store → HTTP 404.
    #[error("session not found: {0}")]
    NotFound(String),

    /// The session is not in a resumable state (only `Stopped`/`Errored` are) →
    /// HTTP 409. Carries the manager's descriptive reason.
    #[error("invalid state transition: {0}")]
    InvalidState(String),

    /// The session's workspace directory was removed
    /// ([`ManagedError::WorkspaceMissing`]) → HTTP 422. Carries the manager's
    /// full actionable message (names the vanished path).
    ///
    /// Why (#2577): a removed workspace is an OPERATOR-actionable precondition,
    /// not a daemon-internal fault — routing it through `Other` → 500 gave the
    /// CLI a bare "daemon returned an internal error (500)" with no clue the
    /// worktree had simply been removed. Kept as its own variant (not merged
    /// with `PaneGone` under one `Unresumable`) because its safe remedy is
    /// different: with no workspace left to protect, `tm session delete
    /// --force` (store-only, never touches tmux) is safe here — the SAME verb
    /// would be actively dangerous for `PaneGone` (see that variant's doc).
    #[error("{0}")]
    WorkspaceGone(String),

    /// The session's recorded tmux pane vanished while a SIBLING window keeps
    /// the tmux session alive ([`ManagedError::PaneGone`]) → HTTP 422. Carries
    /// the manager's full actionable message (names the vanished pane id).
    ///
    /// Why (#2577 review): this is the #2467/#2468 sibling-window-hijack
    /// protection firing — the tmux SESSION is still alive and may hold other
    /// live work; `tm session decommission` kills the WHOLE session (the live
    /// sibling included). A prior draft merged this with `WorkspaceGone` under
    /// one `Unresumable` variant and pointed BOTH at the same "just delete it"
    /// remedy — factually wrong here, since there is nothing missing to
    /// justify teardown, only a stale pane reference. Kept distinct so the CLI
    /// can render a remedy that tells the operator to INSPECT
    /// (`tmux list-panes`) before doing anything destructive.
    #[error("{0}")]
    PaneGone(String),

    /// Any other genuinely-internal failure (store/I-O) → HTTP 500.
    #[error("{0}")]
    Other(String),
}

impl From<ManagedError> for ResumeManagedError {
    /// Why: `SessionManager::resume` returns a typed [`ManagedError`]; mapping its
    /// variants here (rather than at each call site) keeps the not-found/invalid-state
    /// HTTP distinction in one place and prevents a wording change from regressing
    /// a 404/409 to a 500.
    /// What: maps `SessionNotFound` → `NotFound`, `InvalidState` → `InvalidState`
    /// (preserving the descriptive reason), `WorkspaceMissing` → `WorkspaceGone`,
    /// `PaneGone` → `PaneGone` (each preserving the manager's full actionable
    /// Display message verbatim), and every remaining variant → `Other`.
    /// Test: covered transitively by the resume handler 404/409/422 tests
    /// (`resume_managed_typed_*` in tests/session_manager_mvp.rs).
    fn from(e: ManagedError) -> Self {
        match e {
            ManagedError::SessionNotFound(id) => ResumeManagedError::NotFound(id),
            ManagedError::InvalidState(_, reason) => ResumeManagedError::InvalidState(reason),
            // The Display impls of these two variants already carry the vanished
            // path/pane and the concrete remedy — preserve them verbatim so the
            // 422 body is fully actionable at the CLI.
            e @ ManagedError::WorkspaceMissing(..) => {
                ResumeManagedError::WorkspaceGone(e.to_string())
            }
            e @ ManagedError::PaneGone(..) => ResumeManagedError::PaneGone(e.to_string()),
            other => ResumeManagedError::Other(other.to_string()),
        }
    }
}

/// Build the HTTP 422 response for an on-disk-precondition resume failure
/// (`ResumeManagedError::WorkspaceGone`/`PaneGone`), tagging it with a
/// machine-readable `x-trusty-resume-reason` header.
///
/// Why (#2577 review): the two failure classes need DIFFERENT operator remedies
/// (see [`ResumeManagedError::WorkspaceGone`]/[`ResumeManagedError::PaneGone`]
/// docs) but share the same status code and a human-readable body. Without a
/// machine-readable discriminant, the CLI's only option would be to
/// substring-match the body text to pick a remedy — precisely the
/// stringly-typed anti-pattern `ResumeManagedError` was introduced to
/// eliminate for the 404/409 cases. A response header keeps the body free for
/// the operator-facing message while giving the caller a stable, typed signal.
/// What: sets status 422, body = `msg` (the manager's full Display message),
/// header `x-trusty-resume-reason: <reason>` (`"workspace_missing"` or
/// `"pane_gone"`).
/// Test: `unresumable_response_tags_reason_header_per_failure_class` below;
/// `resume_managed_typed_pane_gone_is_unprocessable`,
/// `resume_managed_typed_missing_workspace_is_unprocessable` in
/// `tests/session_manager_mvp.rs` exercise the full round trip.
fn unresumable_response(msg: String, reason: &'static str) -> axum::response::Response {
    let mut resp = (StatusCode::UNPROCESSABLE_ENTITY, msg).into_response();
    resp.headers_mut().insert(
        axum::http::HeaderName::from_static("x-trusty-resume-reason"),
        axum::http::HeaderValue::from_static(reason),
    );
    resp
}

impl IntoResponse for ResumeManagedError {
    /// Why: centralising the FULL status mapping here (rather than a match in
    /// `mod.rs`'s handler) keeps the "which variant maps to which HTTP status /
    /// header" decision beside the variants it decides for — `mod.rs`'s handler
    /// becomes a two-line `Ok`/`Err(e) => e.into_response()` dispatch (#2577
    /// review: this also keeps `mod.rs` comfortably under its 500-SLOC cap
    /// after the WorkspaceGone/PaneGone split grew this enum).
    /// What: `NotFound` → 404, `InvalidState` → 409, `WorkspaceGone`/`PaneGone`
    /// → 422 via [`unresumable_response`] (tagged with the matching
    /// `x-trusty-resume-reason`), `Other` → 500.
    /// Test: `resume_managed_typed_*` in `tests/session_manager_mvp.rs` drive
    /// this transitively through the real HTTP handler.
    fn into_response(self) -> axum::response::Response {
        match self {
            ResumeManagedError::NotFound(id) => {
                (StatusCode::NOT_FOUND, format!("session {id} not found")).into_response()
            }
            ResumeManagedError::InvalidState(reason) => {
                (StatusCode::CONFLICT, reason).into_response()
            }
            ResumeManagedError::WorkspaceGone(msg) => {
                unresumable_response(msg, "workspace_missing")
            }
            ResumeManagedError::PaneGone(msg) => unresumable_response(msg, "pane_gone"),
            ResumeManagedError::Other(msg) => {
                (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #2577 review: `unresumable_response` must tag the 422 with the CORRECT
    /// machine-readable `x-trusty-resume-reason` header per failure class, so
    /// the CLI can select a remedy WITHOUT parsing the human-readable body
    /// text.
    ///
    /// Why: this is the exact plumbing that lets `WorkspaceGone` (safe to
    /// `tm session delete --force`) and `PaneGone` (unsafe to delete/decommission
    /// without first inspecting — a sibling tmux window may still be live) render
    /// DIFFERENT operator guidance from the SAME HTTP status.
    /// What: calls `unresumable_response` directly for each reason string and
    /// asserts status 422 plus the exact header value.
    /// Test: this function IS the test.
    #[test]
    fn unresumable_response_tags_reason_header_per_failure_class() {
        for (reason, msg) in [
            (
                "workspace_missing",
                "workspace directory /gone no longer exists",
            ),
            ("pane_gone", "recorded pane %42 no longer exists"),
        ] {
            let resp = unresumable_response(msg.to_string(), reason);
            assert_eq!(
                resp.status(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "unresumable_response must always answer 422, got {:?} for reason {reason:?}",
                resp.status()
            );
            let header = resp
                .headers()
                .get("x-trusty-resume-reason")
                .and_then(|v| v.to_str().ok());
            assert_eq!(
                header,
                Some(reason),
                "x-trusty-resume-reason header must carry the exact reason passed in"
            );
        }
    }
}
