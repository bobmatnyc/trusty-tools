//! Index registration + status helpers shared by the `init`, `index`, and
//! `discover` flows.
//!
//! Why: both `Init` and `Index` need the same "POST /indexes, parse `created`"
//! dance, optionally forwarding per-index repo-config filters; the `--force`
//! pre-snapshot path also needs the current chunk count before reindex begins.
//! What: `RegisterFilters` (filter payload), `register_index_with_daemon{,_filtered}`
//! (idempotent register), and `fetch_chunk_count` (status probe).
//! Test: covered indirectly by `handle_index` integration tests.

use crate::commands::daemon_utils::daemon_base_url;
use anyhow::Result;

/// Register an index with the daemon (idempotent).
///
/// Why: factored out of `Init` and `Index` because both flows need the same
/// "POST /indexes, parse `created`" dance.
/// What: returns `Ok((created, daemon_reachable))`. `daemon_reachable=false`
/// surfaces network failures distinctly from "registered but already existed".
/// Test: covered indirectly by `handle_index` tests.
pub async fn register_index_with_daemon(
    index_name: &str,
    project_path: &std::path::Path,
) -> Result<(bool, bool)> {
    register_index_with_daemon_filtered(index_name, project_path, &RegisterFilters::default()).await
}

/// Optional repo-config filters carried in `POST /indexes` request bodies.
///
/// Why: `trusty-search.yaml` declares per-index filter sets (`paths`,
/// `exclude`, `languages`, `domain_terms`). The CLI loads the YAML and
/// forwards the resolved values to the daemon when registering each
/// index so the daemon stores them on the `IndexHandle` and applies them
/// to subsequent reindex + search calls.
/// What: thin struct carrying the four fields. `Default` has empty collections,
/// `lexical_only=false`, `skip_kg=false`, `defer_embed=true` (issue #923 default).
/// Test: `commands::index::handle_index` populates this from `IndexConfig`.
#[derive(Debug)]
pub struct RegisterFilters {
    pub include_paths: Vec<String>,
    pub exclude_globs: Vec<String>,
    pub extensions: Vec<String>,
    pub domain_terms: Vec<String>,
    /// Issue #109, Phase 1: when `true`, the CLI tells the daemon to register
    /// this index as `lexical_only` — the reindex pipeline skips Stages 2/3
    /// permanently. Persisted on the daemon side via `indexes.toml`.
    pub lexical_only: bool,
    /// Issue #313: when `true`, the CLI tells the daemon to register this
    /// index with `skip_kg = true` — Phase 3 KG rebuild is suppressed
    /// permanently. Persisted on the daemon side via `indexes.toml`.
    ///
    /// Why: exposes the KG-skip flag at the CLI-to-daemon boundary so
    /// `trusty-search index --no-kg` and the YAML `skip_kg: true` field can
    /// both reach the daemon's create-index handler without extra scaffolding.
    /// What: when `true`, the request body sent to `POST /indexes` includes
    /// `"skip_kg": true`. The daemon stores it in `indexes.toml`.
    /// Test: covered by `skip_kg_index_never_runs_phase3` (end-to-end).
    pub skip_kg: bool,
    /// Issue #923: deferred-embedding (default `true`). Fast pass completes
    /// synchronously (lexical + KG `Ready`); semantic embedding is deferred to
    /// a background job. Set `false` for synchronous full-index behaviour.
    /// Why/What/Test: see `register_index_with_daemon_filtered`.
    pub defer_embed: bool,
}

impl Default for RegisterFilters {
    /// Why: `bool::default()=false` would mis-set `defer_embed`; manual impl
    /// sets `defer_embed=true` (issue #923 default-on).
    /// What/Test: all collections empty; `lexical_only=skip_kg=false`, `defer_embed=true`.
    fn default() -> Self {
        Self {
            include_paths: vec![],
            exclude_globs: vec![],
            extensions: vec![],
            domain_terms: vec![],
            lexical_only: false,
            skip_kg: false,
            defer_embed: true,
        }
    }
}

/// Variant of [`register_index_with_daemon`] that forwards filter/domain
/// fields in the request body so the daemon can store them on the handle.
///
/// Why: the filtered variant is needed when any of the optional fields are
/// non-empty or when `lexical_only` / `skip_kg` is set.
/// What: builds a JSON body with the non-empty filter fields and POSTs to
/// `/indexes`. Returns `(created, daemon_reachable)`.
/// Test: covered indirectly by `handle_index` integration tests.
pub async fn register_index_with_daemon_filtered(
    index_name: &str,
    project_path: &std::path::Path,
    filters: &RegisterFilters,
) -> Result<(bool, bool)> {
    let base = daemon_base_url();
    let client = trusty_common::server::daemon_http_client()?;
    let create_url = format!("{}/indexes", base);
    let mut create_body = serde_json::json!({
        "id": index_name,
        "root_path": project_path,
    });
    if !filters.include_paths.is_empty() {
        create_body["include_paths"] = serde_json::json!(filters.include_paths);
    }
    if !filters.exclude_globs.is_empty() {
        create_body["exclude_globs"] = serde_json::json!(filters.exclude_globs);
    }
    if !filters.extensions.is_empty() {
        create_body["extensions"] = serde_json::json!(filters.extensions);
    }
    if !filters.domain_terms.is_empty() {
        create_body["domain_terms"] = serde_json::json!(filters.domain_terms);
    }
    if filters.lexical_only {
        create_body["lexical_only"] = serde_json::json!(true);
    }
    if filters.skip_kg {
        create_body["skip_kg"] = serde_json::json!(true);
    }
    // Issue #923: omit `defer_embed` when true (server default); only send when opting out.
    if !filters.defer_embed {
        create_body["defer_embed"] = serde_json::json!(false);
    }
    match client.post(&create_url).json(&create_body).send().await {
        Ok(resp) if resp.status().is_success() => {
            let body: serde_json::Value =
                resp.json().await.unwrap_or_else(|_| serde_json::json!({}));
            let created = body
                .get("created")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            Ok((created, true))
        }
        Ok(resp) => {
            anyhow::bail!("daemon returned {} for POST /indexes", resp.status());
        }
        Err(_) => Ok((false, false)),
    }
}

/// Fetch chunk count for an index via /status. Returns `None` if the daemon
/// is unreachable or the index isn't registered.
///
/// Why: the `--force` pre-snapshot path needs the current chunk count before
/// the reindex begins, so the final verify message can show "(was N)".
/// What: GETs `/indexes/:id/status` and parses `chunk_count`.
/// Test: covered indirectly by `run_reindex_force_opts`.
pub async fn fetch_chunk_count(index_id: &str) -> Option<u64> {
    let base = daemon_base_url();
    let url = format!("{}/indexes/{}/status", base, index_id);
    let client = trusty_common::server::daemon_http_client().ok()?;
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: serde_json::Value = resp.json().await.ok()?;
    body.get("chunk_count").and_then(|v| v.as_u64())
}
