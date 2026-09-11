//! `GET`/`PUT /api/assistants/:id/memory` — one palace per assistant, with the
//! opt-in fan-out to other assistants' palaces (#7428, product spec §1a item 3).
//!
//! Why: the fan-out is a USER setting — "reading another Assistant's palace
//! requires that Assistant to be selected in settings" — so it needs a write
//! route, not just a config file. And it needs a read route that reports the
//! RESOLVED palace rather than the stored one: an assistant's palace is derived
//! (binding, else config, else instance id), so a settings pane rendering only
//! the stored value would show an empty field for an assistant that does in
//! fact have a palace.
//! What: [`get`] answers the stored `[memory]` table, the resolution it
//! produces, and the other assistant instances that are selectable. [`put`]
//! replaces the table. Both degrade the same way the rest of this server does:
//! an unreadable home reads as "no settings", never as a 500.
//!
//! Validation is the write side's job, deliberately. A fan-out entry that is
//! not a usable [`AssistantInstanceId`], or names the assistant itself, is
//! REFUSED here — while the recall path (`crate::assistants::memory`) merely
//! drops it, because a stale setting must never break a chat turn.
//!
//! Test: `super::tests::assistant_memory`.

use std::path::{Path, PathBuf};

use axum::{
    Json,
    extract::Path as AxumPath,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::assistants::{
    AssistantHome, AssistantInstanceId, MemoryConfig, discover_instances, read_memory_config,
    resolve_palace_plan_in, write_memory_config,
};

/// The `PUT` body: the whole `[memory]` table, replaced wholesale.
///
/// Why replace rather than patch: the fan-out list is a SET the user edits in
/// one control, so a partial update has no meaning — "remove the last entry"
/// and "send no entries" would be indistinguishable under a patch.
/// Test: `put_replaces_the_whole_table`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MemoryBody {
    #[serde(default)]
    palace: Option<String>,
    #[serde(default)]
    fan_out: Vec<String>,
}

/// `GET /api/assistants/:id/memory` — HTTP entry point.
/// Test: `super::tests::assistant_memory::memory_route_is_wired_into_the_router`.
pub(super) async fn get(AxumPath(name): AxumPath<String>) -> Response {
    match live_roots() {
        Ok((dirs, root)) => read_at(&dirs, &root, &name),
        Err(response) => response,
    }
}

/// `PUT /api/assistants/:id/memory` — HTTP entry point.
/// Test: `super::tests::assistant_memory::put_rejects_self_and_bad_ids`.
pub(super) async fn put(
    AxumPath(name): AxumPath<String>,
    Json(body): Json<MemoryBody>,
) -> Response {
    match live_roots() {
        Ok((dirs, root)) => write_at(&dirs, &root, &name, body),
        Err(response) => response,
    }
}

/// The live agent directories and assistants root, or the response to send when
/// the root cannot be resolved (no `$HOME`).
fn live_roots() -> Result<(Vec<PathBuf>, PathBuf), Response> {
    let root = crate::assistants::assistants_root()
        .map_err(|e| fail(StatusCode::SERVICE_UNAVAILABLE, e))?;
    Ok((crate::agents::agents_dir_candidates(), root))
}

/// One error body, in this server's `{"error": …}` shape.
fn fail(status: StatusCode, message: impl std::fmt::Display) -> Response {
    (status, Json(json!({ "error": message.to_string() }))).into_response()
}

/// Validate `name` as an existing assistant instance.
///
/// Why: the id becomes a directory name under the assistants root, and the
/// setting belongs to user-facing Assistants only — a specialist or delegated
/// agent has no home to store it in. Same membership rule as
/// `super::knowledge_pipeline`.
/// What: `400` for a name that is not a usable instance id, `404` for one no
/// assistant declares.
/// Test: `get_rejects_an_unknown_assistant`.
fn instance(dirs: &[PathBuf], name: &str) -> Result<AssistantInstanceId, Response> {
    let id = AssistantInstanceId::new(name).map_err(|e| fail(StatusCode::BAD_REQUEST, e))?;
    if id.as_str() != name || !discover_instances(dirs).contains(&id) {
        return Err(fail(StatusCode::NOT_FOUND, "Assistant not found"));
    }
    Ok(id)
}

/// The stored settings, their resolution, and the selectable assistants.
///
/// Why: see the module doc — the resolved palace is the only honest thing to
/// render, and the selectable list has to come from the server because the
/// client has no way to know which agents are Assistant instances.
/// Test: `get_reports_the_resolved_palace_and_selectable_assistants`.
pub(super) fn read_at(dirs: &[PathBuf], root: &Path, name: &str) -> Response {
    let id = match instance(dirs, name) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let config = read_memory_config(&AssistantHome::under(root, id.clone()));
    Json(body_for(dirs, root, &id, &config)).into_response()
}

/// Replace the `[memory]` table after validating every entry.
///
/// Why/What: see the module doc. A `palace` that contradicts the assistant's
/// `[[stores]]` binding is refused rather than silently ignored — the binding
/// wins by rule, so accepting the write would leave the user looking at a value
/// nothing reads.
/// Test: `put_replaces_the_whole_table`, `put_rejects_self_and_bad_ids`,
/// `put_refuses_a_palace_the_binding_pins`.
pub(super) fn write_at(dirs: &[PathBuf], root: &Path, name: &str, body: MemoryBody) -> Response {
    let id = match instance(dirs, name) {
        Ok(id) => id,
        Err(response) => return response,
    };

    let mut fan_out: Vec<String> = Vec::new();
    for raw in &body.fan_out {
        let target = match AssistantInstanceId::new(raw) {
            Ok(target) => target,
            Err(e) => return fail(StatusCode::BAD_REQUEST, e),
        };
        if target == id {
            return fail(
                StatusCode::BAD_REQUEST,
                format!(
                    "`{id}` already reads its own palace; fan-out selects OTHER assistants only"
                ),
            );
        }
        if !fan_out.contains(&target.to_string()) {
            fan_out.push(target.to_string());
        }
    }

    let bound = crate::stores::binding::load_stores(dirs, id.as_str())
        .unwrap_or_default()
        .primary()
        .and_then(|b| b.palace.clone());
    if let (Some(requested), Some(bound)) = (body.palace.as_deref(), bound.as_deref())
        && requested.trim() != bound
    {
        return fail(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "`{id}` binds palace `{bound}` in its `[[stores]]` table, which wins over this \
                 setting; change the binding in `agent.toml` instead"
            ),
        );
    }

    let config = MemoryConfig {
        palace: body.palace,
        fan_out,
    };
    let home = AssistantHome::under(root, id.clone());
    if let Err(e) = home
        .ensure()
        .and_then(|_| write_memory_config(&home, &config))
    {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    Json(body_for(dirs, root, &id, &config)).into_response()
}

/// The response body shared by both routes: stored settings plus what they
/// resolve to plus what else is selectable.
fn body_for(
    dirs: &[PathBuf],
    root: &Path,
    id: &AssistantInstanceId,
    config: &MemoryConfig,
) -> Value {
    let binding = crate::stores::binding::load_stores(dirs, id.as_str()).unwrap_or_default();
    let plan = resolve_palace_plan_in(dirs, root, id.as_str(), binding.primary());
    let available: Vec<String> = discover_instances(dirs)
        .into_iter()
        .filter(|other| other != id)
        .map(|other| other.to_string())
        .collect();
    json!({
        "assistant": id.as_str(),
        "palace": config.palace,
        "fan_out": config.fan_out,
        "resolved": {
            "own": plan.own,
            "source": plan.source,
            "fan_out_palaces": plan.fan_out,
        },
        "available": available,
    })
}
