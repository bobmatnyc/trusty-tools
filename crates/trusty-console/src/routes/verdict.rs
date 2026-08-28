//! What one operator-driven action against a daemon actually did (#6360, #6371).
//!
//! Why: #6360 gave the delete routes a four-arm verdict so a refusal, an
//! unreachable daemon and a skipped no-op could not all render as "deleted".
//! #6371 adds two more actions with the same problem — compacting a palace and
//! pruning a batch of stale index registrations — and a second copy of that
//! enum is how two answers to "did it work" drift apart. This module holds the
//! ONE verdict, the ONE id guard, and the ONE response envelope; the route
//! modules decide what to call and how to read a daemon's answer, never how to
//! report it.
//!
//! What: [`ActionVerdict`] and its `IntoResponse`, [`validate_id`], and
//! [`first_line`]. Only [`ActionVerdict::Succeeded`] renders as success, and
//! `ok` is a field in the body rather than something a caller derives from the
//! status code.
//!
//! Test: `validate_id_*` and `verdict_status_codes_separate_the_four_arms` in
//! `crate::routes::deletes`.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

/// Longest resource id this console will forward.
///
/// Bounds what reaches a daemon and what reaches a log line; no real palace or
/// index id comes near it.
pub(crate) const MAX_ID_LEN: usize = 128;

/// What one action attempt actually did, as opposed to what was asked for.
///
/// Why: the daemons fail in four distinguishable ways and an operator needs
/// them apart — a refusal is fixable (pass force, empty the palace), an
/// unreachable daemon is not the same problem, and a skipped action looks like
/// success on the wire. Collapsing them into a bool is how a no-op gets
/// rendered as "done".
/// What: a closed set of verdicts, each carrying the daemon's own message where
/// there is one. Only [`ActionVerdict::Succeeded`] renders as success.
/// Test: `verdict_status_codes_separate_the_four_arms`.
#[derive(Debug)]
pub(crate) enum ActionVerdict {
    /// The daemon confirmed it did the work. `detail` is its own answer.
    Succeeded { id: String, detail: Value },
    /// The daemon answered, and did not do the work — a refusal, an error, or a
    /// no-op. `reason` is the daemon's message wherever the daemon supplied one.
    Refused {
        id: String,
        reason: String,
        detail: Value,
    },
    /// Nothing answered: the daemon is not running, the socket or address could
    /// not be resolved, or the exchange timed out.
    Unreachable { id: String, reason: String },
    /// The request never left the console — the id is not one this route will
    /// forward.
    Invalid { id: String, reason: String },
}

impl ActionVerdict {
    /// The HTTP status this verdict answers with.
    ///
    /// Why `409` for a refusal and a no-op alike: both mean the daemon's current
    /// state prevented the action, and both are retryable once the operator
    /// changes that state. `502` would claim the daemon misbehaved when it
    /// answered correctly.
    /// Test: `verdict_status_codes_separate_the_four_arms`.
    pub(crate) fn status(&self) -> StatusCode {
        match self {
            Self::Succeeded { .. } => StatusCode::OK,
            Self::Refused { .. } => StatusCode::CONFLICT,
            Self::Unreachable { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Self::Invalid { .. } => StatusCode::BAD_REQUEST,
        }
    }

    /// True only when the daemon confirmed the work.
    ///
    /// Used by the batch route, which reports one row per id and must not read
    /// its own success off a status code it never sent.
    pub(crate) fn succeeded(&self) -> bool {
        matches!(self, Self::Succeeded { .. })
    }

    /// The daemon's own words for a non-success, or an empty string.
    ///
    /// Test: `prune_reports_per_item_outcomes_for_a_partial_batch`.
    pub(crate) fn reason(&self) -> &str {
        match self {
            Self::Succeeded { .. } => "",
            Self::Refused { reason, .. }
            | Self::Unreachable { reason, .. }
            | Self::Invalid { reason, .. } => reason,
        }
    }

    /// The id this verdict is about.
    pub(crate) fn id(&self) -> &str {
        match self {
            Self::Succeeded { id, .. }
            | Self::Refused { id, .. }
            | Self::Unreachable { id, .. }
            | Self::Invalid { id, .. } => id,
        }
    }
}

impl IntoResponse for ActionVerdict {
    /// Render the verdict as the console's action-response envelope.
    ///
    /// What: every body carries `ok` and `id`; a non-success body carries
    /// `error` with the daemon's own words. `ok` is never derived from the
    /// status code by the caller — the UI reads this field, so a body that
    /// somehow reached a 2xx without a confirmed action still reads as failure.
    /// Test: `index_delete_reports_a_skipped_delete_as_a_failure`.
    fn into_response(self) -> Response {
        let status = self.status();
        let body = match self {
            Self::Succeeded { id, detail } => json!({ "ok": true, "id": id, "detail": detail }),
            Self::Refused { id, reason, detail } => {
                json!({ "ok": false, "id": id, "error": reason, "detail": detail })
            }
            Self::Unreachable { id, reason } | Self::Invalid { id, reason } => {
                json!({ "ok": false, "id": id, "error": reason })
            }
        };
        (status, axum::Json(body)).into_response()
    }
}

/// Accept an id only if it is safe to place in a URL path and a JSON field.
///
/// Why: the id arrives from the network and is appended to a trusty-search URL
/// path. Nothing here ever reaches a shell or a filesystem path — the console
/// does no deletion itself — but an id that can carry `/`, `..`, `?`, or a
/// control byte could still steer the upstream request at a different route on
/// the daemon. An allowlist refuses that at the console boundary instead of
/// relying on the daemon to.
/// What: non-empty, at most [`MAX_ID_LEN`] bytes, every character in
/// `[A-Za-z0-9._-]`, and no `..` anywhere. `Err` carries a reason safe to render.
/// Test: `validate_id_accepts_ordinary_ids`, `validate_id_rejects_traversal`,
/// `validate_id_rejects_separators_and_control_bytes`.
pub(crate) fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("the resource id is empty".to_string());
    }
    if id.len() > MAX_ID_LEN {
        return Err(format!(
            "the resource id is longer than the {MAX_ID_LEN}-byte limit"
        ));
    }
    if id.contains("..") {
        return Err("the resource id contains '..'".to_string());
    }
    if let Some(bad) = id
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
    {
        return Err(format!(
            "the resource id contains {bad:?}; only letters, digits, '.', '_' and '-' are accepted"
        ));
    }
    Ok(())
}

/// The first line of `body`, truncated, for a one-line error message.
///
/// Keeps a daemon's multi-line or oversized error body from being pasted whole
/// into a JSON field the dashboard renders inline.
pub(crate) fn first_line(body: &str) -> String {
    const MAX: usize = 300;
    let line = body.lines().next().unwrap_or("").trim();
    if line.len() <= MAX {
        return line.to_string();
    }
    // Cut on a char boundary — `str::floor_char_boundary` is still unstable, and
    // slicing mid-codepoint would panic on a daemon message containing any
    // non-ASCII byte.
    let cut = line
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|i| *i <= MAX)
        .last()
        .unwrap_or(0);
    format!("{}…", &line[..cut])
}
