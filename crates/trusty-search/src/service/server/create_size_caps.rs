//! Size caps on `POST /indexes` / `search.index.create` (#11655).
//!
//! Why: caps apply when an index is registered (see #11655),
//! and a re-registration of an existing id returns early. Without a signal, a
//! user who adds caps to the yaml for an existing index gets no effect and no
//! warning.
//! What: [`reject_invalid`] refuses an out-of-range cap with `400
//! invalid_size_cap`; [`report_unapplied`] adds `size_caps_applied: false`
//! with the registered and requested values to the `created: false` body when
//! the request names caps the index does not hold.
//! Test: `a_re_registration_with_other_caps_reports_them_not_applied`,
//! `create_refuses_an_out_of_range_size_cap_over_the_socket`.

use axum::http::StatusCode;

use super::router::CreateIndexRequest;
use crate::core::registry::IndexHandle;
use crate::service::size_caps::validate_size_caps;

/// `400 invalid_size_cap` for a size cap of 0 or over the 256 MiB ceiling.
///
/// Test: `create_refuses_an_out_of_range_size_cap`.
pub(super) fn reject_invalid(
    req: &CreateIndexRequest,
) -> Result<(), (StatusCode, serde_json::Value)> {
    validate_size_caps(req.source_file_max_bytes, req.office_file_max_bytes).map_err(|message| {
        (
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": "invalid_size_cap", "message": message }),
        )
    })
}

/// Mark a `created: false` body when the request's caps differ from the index's.
///
/// What: a requested cap equal to the registered one, or an absent one, is
/// not a difference. On a difference the body gains `size_caps_applied:
/// false`, `registered_size_caps`, `requested_size_caps` and a `message`
/// naming the remedy; the same line is logged at warning level. Otherwise
/// the body is unchanged.
/// Test: `a_re_registration_with_other_caps_reports_them_not_applied`.
pub(super) fn report_unapplied(
    req: &CreateIndexRequest,
    registered: &IndexHandle,
    body: &mut serde_json::Value,
) {
    let caps = registered.size_caps;
    let differs = |requested: Option<u64>, held: u64| requested.is_some_and(|v| v != held);
    if !differs(req.source_file_max_bytes, caps.source)
        && !differs(req.office_file_max_bytes, caps.office)
    {
        return;
    }
    let message = format!(
        "index '{}' is already registered; its size caps are set at registration and were not \
         changed. Delete the index and re-index to change the size caps.",
        req.id
    );
    tracing::warn!("create_index: {message} (#11655)");
    if let Some(map) = body.as_object_mut() {
        map.insert("size_caps_applied".into(), serde_json::json!(false));
        map.insert(
            "registered_size_caps".into(),
            serde_json::json!({
                "source_file_max_bytes": caps.source,
                "office_file_max_bytes": caps.office,
            }),
        );
        map.insert(
            "requested_size_caps".into(),
            serde_json::json!({
                "source_file_max_bytes": req.source_file_max_bytes,
                "office_file_max_bytes": req.office_file_max_bytes,
            }),
        );
        map.insert("message".into(), serde_json::json!(message));
    }
}
