//! Palace-tool handlers for the trusty-memory MCP surface.
//!
//! Why: the `palace_*` tool handlers (create/list/delete/update/info/compact)
//! form one cohesive group split out of the former monolithic `tools.rs`
//! (issue #607).
//! What: per-tool `pub(crate) async fn handle_*` functions moved verbatim;
//! visibility widened to `pub(crate)` so the dispatcher (in `tools::mod`) and
//! the test module can reach them.
//! Test: `dispatch_palace_create_persists`, `dispatch_palace_delete_*`,
//! `dispatch_palace_update_*` in `tools::tests`.

use crate::{ActivitySource, AppState, DaemonEvent};
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use trusty_common::memory_core::palace::{Palace, PalaceId};
use uuid::Uuid;

use super::helpers::{open_palace_handle, resolve_palace};

pub(crate) async fn handle_palace_create(state: &AppState, args: Value) -> Result<Value> {
    let palace_name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("palace_create: missing 'name'"))?;

    // Issue #88 / Change 2: enforce palace = project mapping. New palaces must
    // be named after the current project slug (derived by walking up from CWD)
    // or the special `personal` sentinel. Existing palaces are unaffected —
    // this gate only applies to NEW creation requests.
    //
    // The validation cwd is, in order of preference:
    //   a. `args["cwd"]` — the MCP caller's project path. When present and the
    //      project has a `.trusty-tools/trusty-memory.yaml` pin file, the
    //      pinned slug is used for validation (correct even after a drive reorg).
    //   b. `std::env::current_dir()` — daemon's own cwd, pre-Change-2 fallback.
    //
    // Skip enforcement when invoked from a test context (tests use arbitrary
    // names against tempdir roots that are not real projects). The bypass is
    // keyed on an env var (`TRUSTY_SKIP_PALACE_ENFORCEMENT=1`) that tests set
    // locally; production deployments never set it.
    let skip_enforcement = std::env::var("TRUSTY_SKIP_PALACE_ENFORCEMENT").as_deref() == Ok("1");
    if !skip_enforcement {
        let cwd = args
            .get("cwd")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(std::path::Path::new)
            .map(|p| p.to_path_buf())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| state.data_root.clone());
        crate::project_root::validate_palace_name(palace_name, &cwd)?;
    }

    let description = args
        .get("description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let palace = Palace {
        id: PalaceId::new(palace_name),
        name: palace_name.to_string(),
        description,
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join(palace_name),
    };
    let _handle = state
        .registry
        .create_palace(&state.data_root, palace)
        .context("create_palace")?;
    // Issue #228: keep the in-memory palace-name cache in sync so
    // subsequent writes can resolve the friendly name without a disk
    // walk. The id == name pairing matches what the registry persisted.
    state
        .palace_names
        .insert(palace_name.to_string(), palace_name.to_string());
    // Issue #96: emit so MCP-driven palace creation lands in the
    // dashboard activity feed alongside HTTP-origin creates.
    state.emit(DaemonEvent::PalaceCreated {
        id: palace_name.to_string(),
        name: palace_name.to_string(),
        source: ActivitySource::Mcp,
    });
    // Issue #60: auto-seed the KG with temporal metadata so every
    // new palace has at least `created_at` + `bootstrapped_at`
    // triples anchored to the palace name. We deliberately do NOT
    // pass a project_path here — that requires an explicit user
    // decision (which directory belongs to this palace?). Failures
    // are non-fatal: the palace was already created, and the user
    // can re-run `kg_bootstrap` manually if needed.
    let bootstrap_summary = match crate::bootstrap::bootstrap_palace(state, palace_name, None).await
    {
        Ok(r) => Some(serde_json::json!({
            "triples_asserted": r.triples_asserted,
            "project_subject": r.project_subject,
        })),
        Err(e) => {
            tracing::warn!(
                palace = %palace_name,
                "auto-bootstrap on palace_create failed: {e:#}",
            );
            None
        }
    };
    Ok(json!({
        "palace_id": palace_name,
        "status": "created",
        "bootstrap": bootstrap_summary,
    }))
}

pub(crate) async fn handle_palace_list(state: &AppState, _args: Value) -> Result<Value> {
    let root = state.data_root.clone();
    let palaces = tokio::task::spawn_blocking(move || {
        trusty_common::memory_core::PalaceRegistry::list_palaces(&root)
    })
    .await
    .context("join list_palaces")??;
    let ids: Vec<String> = palaces.iter().map(|p| p.id.as_str().to_string()).collect();
    Ok(json!({ "palaces": ids }))
}

pub(crate) async fn handle_palace_delete(state: &AppState, args: Value) -> Result<Value> {
    // Issue #180: full palace teardown. The HTTP layer is the
    // canonical implementation; we just delegate to the same
    // `MemoryService::delete_palace` method to keep behaviour
    // (and the conflict / not-found / 204 split) identical
    // across surfaces. ServiceError variants are folded into
    // anyhow here so the MCP wire shape matches every other
    // tool's error contract.
    let palace_id = args
        .get("palace_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("palace_delete: missing 'palace_id'"))?
        .to_string();
    let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
    use crate::service::{MemoryService, ServiceError};
    let svc = MemoryService::new(state.clone());
    match svc.delete_palace(&palace_id, force).await {
        Ok(()) => Ok(json!({ "deleted": palace_id })),
        Err(ServiceError::NotFound(_)) => Err(anyhow!("Palace not found: {palace_id}")),
        Err(ServiceError::Conflict(msg)) => Err(anyhow!(msg)),
        Err(e) => Err(anyhow!("palace_delete: {e}")),
    }
}

pub(crate) async fn handle_palace_update(state: &AppState, args: Value) -> Result<Value> {
    // Issue #180 follow-up: rename a palace's display name. The HTTP
    // layer is the canonical implementation; we delegate to the
    // same `MemoryService::update_palace_name` so the
    // load-mutate-save-emit chain stays consistent across surfaces.
    // The MCP wire shape is the minimal acknowledgement payload —
    // callers needing the enriched palace info should use
    // `palace_info` (or the HTTP endpoint, which returns the full
    // shape).
    let palace_id = args
        .get("palace_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("palace_update: missing 'palace_id'"))?
        .to_string();
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("palace_update: missing 'name'"))?
        .to_string();
    use crate::service::MemoryService;
    let svc = MemoryService::new(state.clone());
    match svc.update_palace_name(&palace_id, &name).await {
        Ok(_info) => Ok(json!({ "updated": palace_id, "name": name.trim() })),
        Err(e) => Err(anyhow!("palace_update: {e}")),
    }
}

pub(crate) async fn handle_palace_info(state: &AppState, args: Value) -> Result<Value> {
    let palace = resolve_palace(state, &args, "palace_info")?;
    let handle = open_palace_handle(state, &palace)?;
    let drawer_count = handle.list_drawers(None, None, usize::MAX).len();
    let data_dir = handle
        .data_dir
        .as_ref()
        .map(|p| p.to_string_lossy().to_string());
    Ok(json!({
        "id": handle.id.as_str(),
        "name": handle.id.as_str(),
        "drawer_count": drawer_count,
        "data_dir": data_dir,
    }))
}

pub(crate) async fn handle_palace_compact(state: &AppState, args: Value) -> Result<Value> {
    let palace = resolve_palace(state, &args, "palace_compact")?;
    let handle = open_palace_handle(state, &palace)?;
    // Use the live drawer table (sourced from SQLite at palace open) as
    // the authoritative valid-id set, then run the vector store's
    // synchronous compaction on a blocking thread.
    let valid_ids: std::collections::HashSet<Uuid> =
        handle.drawers.read().iter().map(|d| d.id).collect();
    let vector_store = handle.vector_store.clone();
    let res = tokio::task::spawn_blocking(move || vector_store.compact_orphans(&valid_ids))
        .await
        .context("join palace_compact")??;
    Ok(json!({
        "palace": palace,
        "total_checked": res.total_checked,
        "orphans_removed": res.orphans_removed,
        "index_size_before": res.index_size_before,
        "index_size_after": res.index_size_after,
    }))
}
