//! `DELETE /api/channels/{id}` — remove ONE global channel (#8187).
//!
//! Why: the global editor could create and edit a `[[channels]]` entry after
//! #8038 but never remove one, so an operator who added a wrong destination had
//! to hand-edit `~/.trusty-agents/config.toml` to take it back. Deleting is
//! also the one channel write with a victim that is not in the request: a
//! per-assistant binding may OVERLAY a global channel by id
//! ([`crate::channels::dispatch::is_overlay`]), and once the global is gone
//! `agent_channels::load_at_with` drops that overlay with a log line the
//! operator never sees. This route names those assistants instead.
//! What: the same [`ChannelWriter`] gate, the same revision compare-and-swap
//! and the same comment-preserving rewrite as the `POST`/`PUT` beside it —
//! [`super::amend`] is shared verbatim, so the three per-channel writes cannot
//! drift apart. The revision and the force flag ride in the query string
//! because a `DELETE` carries no body a client can rely on.
//! Test: `crate::api::server::tests::global_channels` —
//! `a_referenced_global_channel_is_not_deleted_without_force`,
//! `a_global_channel_is_deleted_and_an_unknown_id_is_404`,
//! `an_unauthenticated_delete_is_refused`.

use axum::{Json, extract::Query, http::StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

use super::super::channel_auth::ChannelWriter;
use super::{Error, amend, err};

/// The delete's query string: which list the client read, and whether it
/// accepts losing the overlays that name this channel.
///
/// Why `force` is explicit (#8187): silently deleting a channel an assistant
/// overlays turns that assistant's binding inert — it keeps its record, loads
/// with a warning, and addresses nothing. That is a decision the operator has
/// to take, not one this route may take for them.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::api::server) struct DeleteQuery {
    /// The revision the client read, exactly as the `POST`/`PUT` bodies carry.
    revision: String,
    /// Delete anyway when per-assistant bindings still name this channel.
    #[serde(default)]
    force: bool,
}

/// `DELETE /api/channels/{id}` — remove ONE declared global channel.
///
/// Why/What: see the module doc. 404 when no channel carries `id`, 409 when
/// the client's revision is stale, 409 naming `referenced_by` when a
/// per-assistant overlay would be orphaned and `force` is absent, 401 when the
/// caller presents no channel-write credential. A forced delete leaves those
/// bindings on disk — this route never edits another assistant's file — and
/// reports them back as `inert_bindings` so the operator knows what to repair.
/// Test: `a_referenced_global_channel_is_not_deleted_without_force`,
/// `a_global_channel_is_deleted_and_an_unknown_id_is_404`,
/// `an_unauthenticated_delete_is_refused`.
pub(in crate::api::server) async fn delete_route(
    writer: ChannelWriter,
    axum::extract::Path(id): axum::extract::Path<String>,
    Query(query): Query<DeleteQuery>,
) -> Result<Json<Value>, Error> {
    // #8187: the existence check precedes the reference scan so an unknown id
    // answers 404 rather than a 409 about bindings that name nothing.
    if !super::stored()
        .await?
        .iter()
        .any(|channel| channel.id == id)
    {
        return Err(err(StatusCode::NOT_FOUND, "No global channel has this ID"));
    }
    let referencing = referencing_assistants(&id).await?;
    if !referencing.is_empty() && !query.force {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({
                "error": format!(
                    "This channel is still bound by {}; delete it there first, or repeat with force=true",
                    referencing.join(", ")
                ),
                "referenced_by": referencing,
            })),
        ));
    }
    let audited = id.clone();
    let (mut stored, before, after) = amend(query.revision, move |channels| {
        channels.retain(|channel| channel.id != id);
        Ok(())
    })
    .await?;
    writer.audit(
        "DELETE /api/channels/{id}",
        "global",
        Some(&audited),
        Some(before),
        after,
    );
    stored["deleted"] = json!(audited);
    // #8187: named on every delete, not only a forced one — an empty list is
    // the client's proof that nothing was left inert.
    stored["inert_bindings"] = json!(referencing);
    Ok(Json(stored))
}

/// Every assistant whose stored bindings overlay the global channel `id`.
///
/// Why: this is the blast radius of the delete, and it is not in the request.
/// What: the dispatcher's own roster
/// ([`crate::listeners::wake::candidate_agent_names`]) crossed with each
/// assistant's stored overlay ids.
///
/// FAIL-CLOSED (#8187): a roster or a channels file that cannot be read is a
/// 500 that deletes nothing, in the forced path as well as the unforced one.
/// Continuing on a read failure would report "nothing references this" from
/// evidence that says "we could not tell", and `force` is the operator
/// accepting KNOWN references, never unknown state.
/// Test: `a_referenced_global_channel_is_not_deleted_without_force`.
async fn referencing_assistants(id: &str) -> Result<Vec<String>, Error> {
    let names = crate::listeners::wake::candidate_agent_names()
        .await
        .map_err(|e| unreadable("the assistant roster could not be read", &e))?;
    let dirs = crate::agents::agents_dir_candidates();
    let mut referencing = Vec::new();
    for name in names {
        let overlaid = super::super::agent_channels::overlaid_global_ids(&dirs, &name)
            .await
            .map_err(|e| unreadable("an assistant's channel bindings could not be read", &e.1.0))?;
        if overlaid.iter().any(|overlaid| overlaid == id) {
            referencing.push(name);
        }
    }
    Ok(referencing)
}

/// The one refusal shape for "we could not tell what references this".
fn unreadable(what: &str, detail: &dyn std::fmt::Display) -> Error {
    tracing::warn!(detail = %detail, "channel config: {what}; nothing was deleted (#8187)");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "The assistants bound to this channel could not be determined; nothing was deleted",
    )
}
