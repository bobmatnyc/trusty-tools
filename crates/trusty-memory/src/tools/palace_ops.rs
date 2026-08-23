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

/// Validate that a palace slug is a safe, well-formed filesystem name.
///
/// Why: `force=true` bypasses the project-slug enforcement gate but must not
/// allow arbitrary strings that could cause path traversal, redb table-name
/// collisions, or filesystem issues. This guard runs unconditionally.
///
/// The rule itself is [`trusty_common::palace_id::palace_id_is_valid`], not a
/// copy of it. This function used to restate the shape independently, and the
/// deriving side in `trusty_common::palace_id` stated a different one — it had
/// no length cap — so a long repo or directory name derived an id this gate
/// refused, and trusty-code's turn recorder stayed fail-open for that project
/// (#2443). One statement of the rule is what stops the two drifting again.
/// What: delegates the accept/reject decision, then names which half failed —
/// length or character shape — in the error text callers already match on.
/// Test: `force_flag_rejects_unsafe_slugs`, `a_derived_palace_id_is_accepted`
/// (both in `tests/palace_force.rs`).
fn validate_slug_format(slug: &str) -> Result<()> {
    use trusty_common::palace_id::{palace_id_is_valid, PALACE_ID_MAX_LEN};

    if palace_id_is_valid(slug) {
        return Ok(());
    }
    if slug.is_empty() || slug.len() > PALACE_ID_MAX_LEN {
        return Err(anyhow!(
            "palace slug must be 1–{PALACE_ID_MAX_LEN} characters (got {}): {slug:?}",
            slug.len()
        ));
    }
    Err(anyhow!(
        "palace slug must match [a-z0-9][a-z0-9-]{{0,62}} \
         (lowercase letters, digits, hyphens only): {slug:?}"
    ))
}

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
    // spec-001 / Phase 1: `force=true` bypasses slug validation so an
    // application can create a palace under an arbitrary slug (e.g. one palace
    // per app/tenant for chat-session storage). The env-var bypass remains for
    // test contexts; either short-circuits the same validation call.
    let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
    let skip_enforcement = std::env::var("TRUSTY_SKIP_PALACE_ENFORCEMENT").as_deref() == Ok("1");
    // Issue #1714: `force=true` is a privileged bypass; gate it behind the
    // minimal authz seam (no-op in the default single-tenant mode, fails
    // closed in multi-tenant mode). See `crate::authz` for the full design.
    if force {
        crate::authz::authorize_force_palace_create(state)?;
    }
    // Even when `force=true`, validate that the slug is a safe filesystem name:
    // lowercase letters, digits, and hyphens only; must start with a letter or
    // digit; max 63 chars. This prevents path traversal and redb table-name
    // collisions regardless of the project-slug enforcement bypass.
    // The test-context bypass (TRUSTY_SKIP_PALACE_ENFORCEMENT=1) also skips
    // the format gate so unit tests that use historical slug names keep passing.
    if !skip_enforcement {
        validate_slug_format(palace_name)?;
    }
    if !skip_enforcement && !force {
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
    // ADR-0027 D6 / #4807: report how many rooms the palace has. #4809 (T9)
    // adds `wing_count` alongside it — #4807 left it out rather than report a
    // constant, and the Wing entity it was waiting for is now here, so this is
    // a real count from the `WINGS` registry.
    let store = handle.kg.store();
    let (room_count, wing_count) = tokio::task::spawn_blocking(move || {
        anyhow::Ok((store.list_rooms()?.len(), store.list_wings()?.len()))
    })
    .await
    .context("join palace_info counts")?
    .context("count rooms and wings")?;
    Ok(json!({
        "id": handle.id.as_str(),
        "name": handle.id.as_str(),
        "drawer_count": drawer_count,
        "room_count": room_count,
        "wing_count": wing_count,
        "data_dir": data_dir,
    }))
}

/// `palace_reembed` — report, and optionally repair, drawers with no vector.
///
/// Why (#4906): the deferred-embed lane used to drop failures silently, leaving
/// drawers durable in redb and permanently invisible to vector recall — 39 of
/// 1,241 on the live `trusty-tools` palace. Fixing the write path forward
/// repairs none of those, and the repair has to run INSIDE the daemon: the
/// daemon holds the palace's writer lock, so a CLI would only ever get a
/// read-only snapshot it cannot write vectors into.
/// What: `dry_run` (the default) returns the exact set of vectorless drawer ids
/// without touching the embedder; `dry_run: false` re-embeds them through the
/// same primitive the write path uses. Idempotent — a second run over a
/// repaired palace reports zero missing and does no work.
/// Test: `dispatch_palace_reembed_dry_run_reports_counts` in `tools::tests`.
pub(crate) async fn handle_palace_reembed(state: &AppState, args: Value) -> Result<Value> {
    use trusty_common::memory_core::retrieval::VectorBackfillOptions;
    let palace = resolve_palace(state, &args, "palace_reembed")?;
    let handle = open_palace_handle(state, &palace)?;
    // Defaults to a dry run on purpose: the first thing an operator wants is
    // the number, and #4834 deletes source files on the strength of it.
    let dry_run = args
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);
    let report = handle
        .backfill_missing_vectors(VectorBackfillOptions {
            dry_run,
            limit,
            ..Default::default()
        })
        .await?;
    let health = handle.embed_health();
    Ok(json!({
        "palace": report.palace_id,
        "dry_run": report.dry_run,
        "drawer_count": report.drawer_count,
        "vector_count": report.vector_count,
        "missing": report.missing,
        "attempted": report.attempted,
        "repaired": report.repaired,
        "still_failing": report.still_failing,
        "still_missing_ids": report.still_missing_ids
            .iter().map(|i| i.to_string()).collect::<Vec<_>>(),
        // Without this a shortfall is unexplained: "no embedder on this host"
        // reads identically to "the embedder is dropping writes".
        "embedder_ready": health.embedder_ready,
        "recorded_failures": health.recorded_failures.len(),
        // #5005 / #5000: `missing` counts drawers with no vector key. An
        // aliased drawer HAS a key and is still unretrievable, so `missing: 0`
        // was a false all-clear on the palace that lost four of them. Gate
        // deletions on `alias_audit == "clean"` as well as `missing == 0`:
        // `"unavailable"` means the scan failed and nothing is known, which is
        // a block, not a pass. `vector_key_rows` / `distinct_vector_ids` are
        // null in that case rather than 0, so no zero can be misread as clean.
        "alias_audit": alias_audit_state(&report.alias_audit),
        "alias_audit_error": report.alias_audit.unavailable_reason(),
        "vector_key_rows": report.alias_audit.counts().map(|(rows, _)| rows),
        "distinct_vector_ids": report.alias_audit.counts().map(|(_, ids)| ids),
        // `aliased` reported 0 for an unreadable audit in the first cut, while
        // the two fields above it correctly reported null. Every count-shaped
        // field in this object is now absent rather than zero when nothing was
        // read — a lone zero is exactly the misreading #5005 documents.
        "aliased": report.alias_audit.aliased_drawer_ids().map(<[Uuid]>::len),
        "aliased_ids": report.alias_audit.aliased_drawer_ids()
            .map(|ids| ids.iter().map(|i| i.to_string()).collect::<Vec<_>>()),
    }))
}

/// `palace_unalias` — free drawers destroyed by a vector-id collision so a
/// re-embed can repair them.
///
/// Why (#5005): the allocator fix stops NEW aliasing and `palace_reembed` now
/// makes existing aliasing visible, but neither repairs it — `unalias` had no
/// caller at all, so an operator could see the damage and not act on it. The
/// three drawers still blocking #4834 need this surface. It runs inside the
/// daemon for the same reason `palace_reembed` does: the daemon holds the
/// palace's writer lock, so a CLI would only get a read-only snapshot.
/// What: `dry_run` (the default) names the exact drawer ids it would free and
/// writes nothing. `dry_run: false` frees the whole collision group, then
/// re-audits — `outcome` is `"repaired"` only when that verification ran and
/// came back clean. Idempotent: a second run reports `"clean"` and frees
/// nothing. The freed drawers still need a `palace_reembed` run to become
/// findable, which `reembed_required` says outright.
///
/// 🔴 `outcome` is the field to branch on, never `freed_ids.len()`. `"partial"`
/// and `"unavailable"` both carry ids and neither is a success.
/// Test: `dispatch_palace_unalias_dry_run_names_ids_and_writes_nothing`, and
/// `dispatch_palace_unalias_frees_a_real_collision_and_is_idempotent` for the
/// write path (#5005 review: the success path had only ever run empty).
pub(crate) async fn handle_palace_unalias(state: &AppState, args: Value) -> Result<Value> {
    use trusty_common::memory_core::retrieval::{AliasRepairOptions, AliasRepairOutcome};
    let palace = resolve_palace(state, &args, "palace_unalias")?;
    let handle = open_palace_handle(state, &palace)?;
    // Defaults to a dry run for the same reason `palace_reembed` does, and with
    // more at stake: this one deletes vector keys.
    let dry_run = args
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let report =
        tokio::task::spawn_blocking(move || handle.repair_aliases(AliasRepairOptions { dry_run }))
            .await
            .context("join palace_unalias")??;

    let ids = |v: &[Uuid]| v.iter().map(|i| i.to_string()).collect::<Vec<_>>();
    let (still_aliased_ids, not_freed_ids, unparsed_keys) = match &report.outcome {
        AliasRepairOutcome::Partial {
            still_aliased,
            not_freed,
            unparsed_keys,
        } => (
            Some(ids(still_aliased)),
            Some(ids(not_freed)),
            Some(unparsed_keys.clone()),
        ),
        _ => (None, None, None),
    };
    let error = match &report.outcome {
        AliasRepairOutcome::Unavailable { reason } => Some(reason.as_str()),
        _ => None,
    };
    Ok(json!({
        "palace": report.palace_id,
        "dry_run": report.dry_run,
        // Branch on this. `"clean"` and `"repaired"` are the only successes.
        "outcome": report.outcome.as_str(),
        "success": report.outcome.is_success(),
        // The id SET, never a bare count: #5005 was a count reporting all-clear
        // over real loss, and these ids are also the re-embed worklist.
        "freed_ids": ids(&report.freed_ids),
        "aliased_before_ids": report.before.aliased_drawer_ids().map(ids),
        // #5005 review HIGH: a collision group whose keys are not uuids names
        // no drawer, so `aliased_before_ids` can be EMPTY over a real
        // collision. Non-empty here means that id list is short — read
        // `vector_key_rows` vs `distinct_vector_ids` off `palace_reembed`, and
        // branch on `outcome`, never on the id counts.
        "unnameable_keys": report.unnameable_keys.clone(),
        // Present only on a partial repair, which is exactly when a caller must
        // not read the run as done.
        "still_aliased_ids": still_aliased_ids,
        "not_freed_ids": not_freed_ids,
        "unparsed_keys": unparsed_keys,
        "error": error,
        // Freeing a group turns an invisible drawer into an ordinary missing
        // one; only `palace_reembed` makes it retrievable again.
        "reembed_required": report.reembed_required(),
    }))
}

/// One word for how the #5005 alias audit went, for the `palace_reembed` payload.
///
/// Why: a caller has to be able to tell "no drawer is aliased" from "the scan
/// failed and nothing is known" without inspecting counts — the second must
/// never read as the first.
/// What: `"clean"`, `"aliased"`, or `"unavailable"`.
/// Test: `dispatch_palace_reembed_dry_run_reports_counts` in `tools::tests`.
fn alias_audit_state(audit: &trusty_common::memory_core::retrieval::AliasAudit) -> &'static str {
    if audit.unavailable_reason().is_some() {
        "unavailable"
    } else if audit.is_clean() {
        "clean"
    } else {
        "aliased"
    }
}

pub(crate) async fn handle_palace_compact(state: &AppState, args: Value) -> Result<Value> {
    let palace = resolve_palace(state, &args, "palace_compact")?;
    let handle = open_palace_handle(state, &palace)?;
    // #6208: route through the handle's locked reclamation. It snapshots the
    // valid-id set and reclaims orphans while holding the palace write mutex,
    // so a concurrent `remember` (vector upserted, drawer not yet registered)
    // cannot have its brand-new vector reclaimed as a false orphan. Snapshotting
    // the valid-ids here without that lock is exactly the window #6208 closes.
    let res = handle.compact_vector_orphans().await?;
    Ok(json!({
        "palace": palace,
        "total_checked": res.total_checked,
        "orphans_removed": res.orphans_removed,
        "index_size_before": res.index_size_before,
        "index_size_after": res.index_size_after,
    }))
}
