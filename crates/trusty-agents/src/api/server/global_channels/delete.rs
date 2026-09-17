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
//!
//! WHAT THIS ROUTE DOES NOT DO (#8187, critic round): it does not stop a
//! running receiver. [`crate::listeners::poll::spawn_listeners`] is called once
//! from the API bootstrap and each poll loop owns a COPY of its channel config,
//! so a deleted `receive_enabled` channel keeps polling its provider and keeps
//! waking the assistants its captured `route_to` named, until the daemon
//! restarts. That is reported as `receiving_until_restart` on the response
//! rather than left for an operator to discover from the wake log. Stopping the
//! poller is separate work, noted as a follow-up in the changelog fragment.
//! Test: `crate::api::server::tests::global_channels` —
//! `a_referenced_global_channel_is_not_deleted_without_force`,
//! `a_global_channel_is_deleted_and_an_unknown_id_is_404`,
//! `an_unauthenticated_delete_is_refused`,
//! `a_deleted_receiving_channel_says_its_receiver_runs_until_restart`,
//! `a_forced_delete_audits_the_override_and_the_orphans`,
//! `an_unaddressable_assistant_name_is_skipped_and_a_broken_file_is_named`.

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
/// `receiving_until_restart` says whether a poller for this channel is still
/// running; see the module doc.
/// Test: `a_referenced_global_channel_is_not_deleted_without_force`,
/// `a_global_channel_is_deleted_and_an_unknown_id_is_404`,
/// `an_unauthenticated_delete_is_refused`,
/// `a_deleted_receiving_channel_says_its_receiver_runs_until_restart`.
pub(in crate::api::server) async fn delete_route(
    writer: ChannelWriter,
    axum::extract::Path(id): axum::extract::Path<String>,
    Query(query): Query<DeleteQuery>,
) -> Result<Json<Value>, Error> {
    // #8187: the existence check precedes the reference scan so an unknown id
    // answers 404 rather than a 409 about bindings that name nothing.
    let Some(receiving) = super::stored()
        .await?
        .iter()
        .find(|channel| channel.id == id)
        .map(|channel| channel.receive_enabled)
    else {
        return Err(err(StatusCode::NOT_FOUND, "No global channel has this ID"));
    };
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
    if query.force {
        audit_forced(&audited, &referencing);
    }
    stored["deleted"] = json!(audited);
    // #8187: named on every delete, not only a forced one — an empty list is
    // the client's proof that nothing was left inert.
    stored["inert_bindings"] = json!(referencing);
    // #8187 (critic round, HIGH): always present, so `false` is the client's
    // proof that no receiver was left running rather than an absent key it has
    // to interpret. See the module doc for why this route cannot stop one.
    stored["receiving_until_restart"] = json!(receiving);
    Ok(Json(stored))
}

/// The forced delete's OWN record, beside [`ChannelWriter::audit`] (#8187).
///
/// Why (critic round, MEDIUM): the shared line records the channel counts
/// either side of the write and nothing else, so a delete that overrode live
/// bindings and one that overrode nothing read identically in the log. `force`
/// is the one field of this request that changes what the operator accepted,
/// and the audit trail is where that acceptance has to survive.
/// What: one line on the forced path only — an unforced delete had nothing to
/// force past — naming the channel and the assistants left inert. The
/// assistants are joined into one field rather than logged per assistant so the
/// override stays a single greppable record.
/// Test: `a_forced_delete_audits_the_override_and_the_orphans`.
fn audit_forced(id: &str, orphaned: &[String]) {
    // #8187: recorded as a `&str`, not through `%`, so the field is QUOTED in
    // the log exactly as `audit_write`'s are — one grep shape for both lines.
    let inert = orphaned.join(",");
    tracing::info!(
        audit = "channel-write-forced",
        route = "DELETE /api/channels/{id}",
        scope = "global",
        channel = id,
        forced = true,
        inert_bindings = inert.as_str(),
        "a forced global channel delete left per-assistant bindings inert (#8187)"
    );
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
///
/// ONE EXCEPTION (critic round, MEDIUM): a roster entry whose name
/// `agent_channels::config_path` refuses on charset — a directory this API
/// cannot address at all, which `candidate_agent_names` enumerates anyway — is
/// SKIPPED with a warning. It is the one failure no operator action reachable
/// from this API can clear, so treating it as unknown state made every global
/// delete on the host a permanent 500. A malformed or unreadable channels file
/// stays fail-closed, because renaming or repairing that file does clear it.
/// Either way the assistant is named, in the log and in the 500 body, so the
/// operator is not left grepping for which of N assistants is blocking the
/// delete.
/// Test: `a_referenced_global_channel_is_not_deleted_without_force`,
/// `an_unaddressable_assistant_name_is_skipped_and_a_broken_file_is_named`.
async fn referencing_assistants(id: &str) -> Result<Vec<String>, Error> {
    let names = crate::listeners::wake::candidate_agent_names()
        .await
        .map_err(|e| unreadable(None, "The assistant roster could not be read", &e))?;
    let dirs = crate::agents::agents_dir_candidates();
    let mut referencing = Vec::new();
    for name in names {
        let overlaid = match super::super::agent_channels::overlaid_global_ids(&dirs, &name).await {
            Ok(overlaid) => overlaid,
            // #8187: a name this API cannot address — see the doc above.
            Err(e) if e.0 == StatusCode::BAD_REQUEST => {
                tracing::warn!(
                    assistant = name.as_str(),
                    "channel config: this assistant name cannot be addressed by the channels \
                     API; skipping it in the delete reference scan (#8187)"
                );
                continue;
            }
            Err(e) => {
                return Err(unreadable(
                    Some(&name),
                    "This assistant's channel bindings could not be read",
                    &e.1.0,
                ));
            }
        };
        if overlaid.iter().any(|overlaid| overlaid == id) {
            referencing.push(name);
        }
    }
    Ok(referencing)
}

/// The one refusal shape for "we could not tell what references this".
///
/// #8187 (critic round): `assistant` rides in the body as well as the log,
/// because the 500 is what the operator sees and a message naming nothing left
/// them to bisect the roster by hand.
fn unreadable(assistant: Option<&str>, what: &str, detail: &dyn std::fmt::Display) -> Error {
    tracing::warn!(
        assistant = assistant.unwrap_or("-"),
        detail = %detail,
        "channel config: {what}; nothing was deleted (#8187)"
    );
    let message = match assistant {
        Some(name) => format!("{what} ({name}); nothing was deleted"),
        None => format!("{what}; nothing was deleted"),
    };
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": message, "assistant": assistant})),
    )
}
