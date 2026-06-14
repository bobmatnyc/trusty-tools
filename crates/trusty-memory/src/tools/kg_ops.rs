//! KG / alias / prompt-fact / upgrade tool handlers for the trusty-memory
//! MCP surface.
//!
//! Why: the knowledge-graph (`kg_*`), alias (`add_alias`/`discover_aliases`),
//! prompt-fact (`list_prompt_facts`/`remove_prompt_fact`/`get_prompt_context`)
//! and `upgrade` tool handlers form one cohesive group split out of the former
//! monolithic `tools.rs` (issue #607).
//! What: per-tool `pub(crate) async fn handle_*` functions moved verbatim;
//! visibility widened to `pub(crate)` so the dispatcher (in `tools::mod`) and
//! the test module can reach them.
//! Test: `dispatch_kg_assert_then_query`, `dispatch_discover_aliases_*`,
//! `dispatch_kg_gaps_returns_cached` in `tools::tests`.

use crate::AppState;
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use trusty_common::memory_core::palace::PalaceId;
use trusty_common::memory_core::store::kg::Triple;

use super::helpers::{open_palace_handle, resolve_palace};

pub(crate) async fn handle_kg_assert(state: &AppState, args: Value) -> Result<Value> {
    let palace = resolve_palace(state, &args, "kg_assert")?;
    let palace = palace.as_str();
    let subject = args
        .get("subject")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("kg_assert: missing 'subject'"))?
        .to_string();
    let predicate = args
        .get("predicate")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("kg_assert: missing 'predicate'"))?
        .to_string();
    let object = args
        .get("object")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("kg_assert: missing 'object'"))?
        .to_string();
    let confidence = args
        .get("confidence")
        .and_then(|v| v.as_f64())
        .map(|c| (c as f32).clamp(0.0, 1.0))
        .unwrap_or(1.0);
    let provenance = args
        .get("provenance")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let handle = open_palace_handle(state, palace)?;
    let triple = Triple {
        subject,
        predicate,
        object,
        valid_from: chrono::Utc::now(),
        valid_to: None,
        confidence,
        provenance,
    };
    let is_hot = crate::prompt_facts::is_hot_predicate(&triple.predicate);
    handle.kg.assert(triple).await.context("kg.assert")?;
    // Rebuild the prompt cache if this assertion touched a hot
    // predicate; otherwise the cache stays valid and we skip the
    // gather/format pass. Failures are logged but non-fatal — the
    // write succeeded, the cache is only a denormalisation.
    if is_hot {
        if let Err(e) = crate::prompt_facts::rebuild_prompt_cache(state).await {
            tracing::warn!("rebuild_prompt_cache after kg_assert failed: {e:#}");
        }
    }
    Ok(json!({ "status": "asserted" }))
}

pub(crate) async fn handle_add_alias(state: &AppState, args: Value) -> Result<Value> {
    let short = args
        .get("short")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("add_alias: missing 'short'"))?
        .to_string();
    let full = args
        .get("full")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("add_alias: missing 'full'"))?
        .to_string();
    let extra = args
        .get("extra")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // `add_alias` is bound to the default palace when configured;
    // otherwise it lands in whatever palace the caller names. This
    // mirrors `resolve_palace`'s rule but without the helpful error
    // — aliases are typically project-scoped via `--palace`.
    let palace = resolve_palace(state, &args, "add_alias")?;
    let handle = open_palace_handle(state, &palace)?;
    // Compose the object: "<full>" or "<full> (<extra>)".
    let object = match extra.as_deref() {
        Some(e) if !e.is_empty() => format!("{full} ({e})"),
        _ => full.clone(),
    };
    let triple = Triple {
        subject: short.clone(),
        predicate: "is_alias_for".to_string(),
        object,
        valid_from: chrono::Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: Some("add_alias".to_string()),
    };
    handle
        .kg
        .assert(triple)
        .await
        .context("kg.assert (alias)")?;
    if let Err(e) = crate::prompt_facts::rebuild_prompt_cache(state).await {
        tracing::warn!("rebuild_prompt_cache after add_alias failed: {e:#}");
    }
    Ok(json!({ "asserted": true, "short": short, "full": full }))
}

pub(crate) async fn handle_list_prompt_facts(state: &AppState, _args: Value) -> Result<Value> {
    let triples = crate::prompt_facts::gather_hot_triples(state).await?;
    let payload: Vec<Value> = triples
        .into_iter()
        .map(|(subject, predicate, object)| {
            json!({ "subject": subject, "predicate": predicate, "object": object })
        })
        .collect();
    Ok(json!({ "facts": payload }))
}

pub(crate) async fn handle_remove_prompt_fact(state: &AppState, args: Value) -> Result<Value> {
    let subject = args
        .get("subject")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("remove_prompt_fact: missing 'subject'"))?
        .to_string();
    let predicate = args
        .get("predicate")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("remove_prompt_fact: missing 'predicate'"))?
        .to_string();

    // The prompt-fact surface spans every palace, so try retracting
    // across all of them and report `true` if any palace closed an
    // active interval. This matches `list_prompt_facts`' scope so
    // round-tripping list→remove never silently no-ops because the
    // caller didn't name the right palace.
    let mut closed_total: usize = 0;
    for palace_id in state.registry.list() {
        if let Some(handle) = state.registry.get(&palace_id) {
            match handle.kg.retract(&subject, &predicate).await {
                Ok(n) => closed_total += n,
                Err(e) => tracing::warn!(
                    palace = %palace_id.as_str(),
                    "retract failed: {e:#}",
                ),
            }
        }
    }
    if closed_total > 0 {
        if let Err(e) = crate::prompt_facts::rebuild_prompt_cache(state).await {
            tracing::warn!("rebuild_prompt_cache after remove_prompt_fact failed: {e:#}");
        }
        Ok(json!({ "removed": true, "closed": closed_total }))
    } else {
        Ok(json!({ "removed": false, "reason": "not found" }))
    }
}

pub(crate) async fn handle_kg_query(state: &AppState, args: Value) -> Result<Value> {
    let palace = resolve_palace(state, &args, "kg_query")?;
    let subject = args
        .get("subject")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("kg_query: missing 'subject'"))?;
    let handle = open_palace_handle(state, &palace)?;
    let triples = handle
        .kg
        .query_active(subject)
        .await
        .context("kg.query_active")?;
    let payload: Vec<Value> = triples
        .iter()
        .map(|t| {
            json!({
                "subject": t.subject,
                "predicate": t.predicate,
                "object": t.object,
                "valid_from": t.valid_from.to_rfc3339(),
                "valid_to": t.valid_to.as_ref().map(|d| d.to_rfc3339()),
                "confidence": t.confidence,
                "provenance": t.provenance,
            })
        })
        .collect();
    // Issue #60: surface a hint when the requested subject has no
    // active triples so the model knows `kg_bootstrap` and
    // `kg_assert` exist. Empty payload is the only signal we have
    // at the per-subject query layer; that's the user-visible
    // "nothing here" case the hint is for.
    let mut response = json!({ "subject": subject, "triples": payload });
    if crate::bootstrap::is_kg_empty_for_subject(&triples) {
        response["hint"] = Value::String(crate::bootstrap::KG_EMPTY_HINT.to_string());
    }
    Ok(response)
}

pub(crate) async fn handle_kg_gaps(state: &AppState, args: Value) -> Result<Value> {
    // Why (issue #53): Surface the cached community-detection output
    // so the model can plan exploration without re-running Louvain.
    // We deliberately do NOT recompute on the read path; the cache is
    // refreshed by the dream cycle.
    // What: Resolves the palace (explicit arg or daemon default),
    // validates it exists by opening the handle, and returns the
    // cached vec (an empty array when the dream cycle has not yet
    // populated it).
    // Test: `dispatch_kg_gaps_returns_cached`.
    let palace = resolve_palace(state, &args, "kg_gaps")?;
    // Ensure the palace exists; this also surfaces a useful error for
    // typos in the palace argument.
    let _handle = open_palace_handle(state, &palace)?;
    let pid = PalaceId::new(&palace);
    let cached = state.registry.get_gaps(&pid).unwrap_or_default();
    let payload: Vec<Value> = cached
        .into_iter()
        .map(|g| {
            json!({
                "entities": g.entities,
                "internal_density": g.internal_density,
                "external_bridges": g.external_bridges,
                "suggested_exploration": g.suggested_exploration,
            })
        })
        .collect();
    Ok(json!({ "palace": palace, "gaps": payload }))
}

pub(crate) async fn handle_get_prompt_context(state: &AppState, args: Value) -> Result<Value> {
    // Why (issue #42): the model calls this at the start of each
    // turn to pull aliases/conventions/facts into its working
    // context. A `query` filter lets it scope the result to just
    // the facts that matter for the current task — cheap on the
    // wire and keeps the prompt focused.
    // What: read-locks the cache once, clones the snapshot, then
    // releases the lock so the formatter runs without blocking
    // concurrent readers. When `query` is set we re-format a
    // filtered subset of the raw triples; otherwise we serve the
    // pre-formatted string directly.
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    // Issue #229: tokio::sync::RwLock is async-aware — `.read()` returns a
    // future that resolves to the guard, so no poison handling is needed
    // (tokio locks are not poisoned by panics).
    let cache_snapshot = {
        let guard = state.prompt_context_cache.read().await;
        guard.clone()
    };

    let body = if let Some(q) = query.as_deref() {
        let needle = q.to_lowercase();
        let filtered: Vec<(String, String, String)> = cache_snapshot
            .triples
            .into_iter()
            .filter(|(subject, _predicate, object)| {
                subject.to_lowercase().contains(&needle) || object.to_lowercase().contains(&needle)
            })
            .collect();
        let formatted = crate::prompt_facts::build_prompt_context(&filtered);
        if formatted.is_empty() {
            "No project context found matching your query.".to_string()
        } else {
            formatted
        }
    } else if cache_snapshot.formatted.is_empty() {
        "No prompt facts stored yet.".to_string()
    } else {
        cache_snapshot.formatted
    };

    // Return the body as a bare JSON string so the MCP envelope's
    // `content[0].text` carries the formatted Markdown verbatim
    // (ready to paste into the model's working context) without an
    // extra `{"context": "..."}` wrapper that callers would have
    // to strip.
    Ok(Value::String(body))
}

pub(crate) async fn handle_discover_aliases(state: &AppState, args: Value) -> Result<Value> {
    // Why (issue #42): Surface project shorthand automatically so the
    // model never has to be told `tga == trusty-git-analytics`. The
    // tool resolves a palace (default or argument), runs the
    // pure-discovery scanner against the requested root (or cwd),
    // checks each candidate against the palace's active KG, and
    // asserts only the new ones. The prompt cache is rebuilt once
    // at the end iff anything was actually asserted.
    // What: returns `{ discovered: [...], already_known: N, new: M }`
    // so callers can audit the delta.
    // Test: `dispatch_discover_aliases_inserts_new_and_dedupes`.
    let palace = resolve_palace(state, &args, "discover_aliases")?;
    let project_root = args
        .get("project_root")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .ok_or_else(|| anyhow!("discover_aliases: no project_root and cwd unavailable"))?;

    let discoveries = crate::discovery::discover_project_aliases(&project_root).await?;

    let handle = open_palace_handle(state, &palace)?;

    let mut already_known = 0usize;
    let mut newly_asserted = 0usize;
    let mut reported: Vec<Value> = Vec::with_capacity(discoveries.len());

    for d in &discoveries {
        // Check active triples for the subject; if any matches the
        // same predicate + object, skip the assertion.
        let active = handle
            .kg
            .query_active(&d.short)
            .await
            .context("kg.query_active")?;
        let exists = active
            .iter()
            .any(|t| t.predicate == "is_alias_for" && t.object == d.full);
        if exists {
            already_known += 1;
            continue;
        }

        let triple = Triple {
            subject: d.short.clone(),
            predicate: "is_alias_for".to_string(),
            object: d.full.clone(),
            valid_from: chrono::Utc::now(),
            valid_to: None,
            confidence: 1.0,
            provenance: Some(format!("discover_aliases:{}", d.source.as_str())),
        };
        handle
            .kg
            .assert(triple)
            .await
            .context("kg.assert (discover)")?;
        newly_asserted += 1;
        reported.push(json!({
            "short": d.short,
            "full": d.full,
            "source": d.source.as_str(),
        }));
    }

    if newly_asserted > 0 {
        if let Err(e) = crate::prompt_facts::rebuild_prompt_cache(state).await {
            tracing::warn!("rebuild_prompt_cache after discover_aliases failed: {e:#}");
        }
    }

    Ok(json!({
        "discovered": reported,
        "already_known": already_known,
        "new": newly_asserted,
        "palace": palace,
    }))
}

pub(crate) async fn handle_kg_bootstrap(state: &AppState, args: Value) -> Result<Value> {
    // Issue #60: scan well-known project files and seed the KG with
    // structured triples + temporal metadata. The handler resolves
    // the palace (explicit arg or daemon default) and forwards the
    // optional `project_path` to the bootstrap helper.
    let palace = resolve_palace(state, &args, "kg_bootstrap")?;
    let project_path = args
        .get("project_path")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from);
    let result = crate::bootstrap::bootstrap_palace(state, &palace, project_path.as_deref())
        .await
        .context("bootstrap_palace")?;
    // Rebuild the prompt cache: bootstrap can land hot predicates
    // (descriptions, language tags) that affect the prompt-facts
    // surface. Cache failures are non-fatal.
    if let Err(e) = crate::prompt_facts::rebuild_prompt_cache(state).await {
        tracing::warn!("rebuild_prompt_cache after kg_bootstrap failed: {e:#}");
    }
    crate::bootstrap::result_to_json(&result)
}

/// MCP `upgrade` tool handler — check for or install a new trusty-memory version.
///
/// Why: Exposes the upgrade workflow to MCP clients (e.g. Claude Code) so
/// operators can trigger a version check or install from within an AI session
/// without leaving the assistant. Never auto-installs silently — the `confirm`
/// parameter must be explicitly set to `true` by the operator.
///
/// What:
/// - `check=true` or `confirm` absent/false: call `check_crates_io` (fresh,
///   bypassing the 24h cache) and return current vs. available. No install.
/// - `confirm=true`: call `upgrade_and_restart`. The MCP response is returned
///   BEFORE the process exits so the client receives the result, then the
///   daemon restarts (under launchd) or prints a hint (unsupervised). To
///   guarantee the response is flushed before exit, the actual exit is
///   dispatched on a short-delayed `tokio::spawn(sleep(500ms))` task.
///
/// Test: `cargo test -p trusty-memory` — the schema is included in the
/// `tool_definitions_lists_all_tools` test; the confirm=false path can be
/// validated via `cargo run -p trusty-memory -- serve` + an MCP client.
pub(crate) async fn handle_upgrade_tool(state: &AppState, args: Value) -> Result<Value> {
    let check = args.get("check").and_then(Value::as_bool).unwrap_or(true);
    let confirm = args
        .get("confirm")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let crate_name = env!("CARGO_PKG_NAME");
    let current = env!("CARGO_PKG_VERSION");

    // Check-only path: report versions, no install.
    let info = trusty_common::update::check_crates_io(crate_name, current).await;

    let (latest, is_update) = match &info {
        Some(u) => (u.latest.as_str(), true),
        None => (current, false),
    };

    if check || !confirm {
        let msg = if is_update {
            format!(
                "Update available: {crate_name} {latest} (you have {current}). \
                 Call with confirm=true to install."
            )
        } else {
            format!("{crate_name} {current} is already up to date.")
        };
        return Ok(
            serde_json::json!({ "status": "checked", "current": current, "latest": latest, "update_available": is_update, "message": msg }),
        );
    }

    // confirm=true path: install, health-gate, restart/hint.
    // Return the response first, then trigger the restart on a short delay so
    // the MCP transport has time to flush the JSON-RPC response to the client
    // before the process exits. 500 ms is conservative but safe; the bridge
    // reconnect (issue #535) resumes the session after the daemon comes back up.
    if !is_update {
        return Ok(serde_json::json!({
            "status": "up_to_date",
            "current": current,
            "message": format!("{crate_name} {current} is already up to date — nothing to install.")
        }));
    }

    let upgrade_state = state.update_available.clone();
    let latest_owned = latest.to_string();
    let crate_name_owned = crate_name.to_string();
    let response = serde_json::json!({
        "status": "installing",
        "current": current,
        "latest": latest_owned,
        "message": format!(
            "Installing {crate_name} {latest_owned} — daemon will restart automatically \
             under launchd, or you will be prompted to restart manually."
        )
    });

    // Spawn the actual install + restart on a delayed task so this handler
    // returns the response to the client before the process exits.
    tokio::spawn(async move {
        // 500 ms gives the MCP transport time to flush the response.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        match trusty_common::update::upgrade_and_restart(&crate_name_owned, &crate_name_owned).await
        {
            Ok(Some(hint)) => {
                tracing::info!("{hint}");
                eprintln!("{hint}");
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!("upgrade_and_restart failed: {e:#}");
                eprintln!("[trusty-memory] upgrade failed: {e:#}");
                // Update the state to clear any stale update_available so
                // the next /health call does not report a broken state.
                if let Ok(mut g) = upgrade_state.lock() {
                    *g = None;
                }
            }
        }
    });

    Ok(response)
}
