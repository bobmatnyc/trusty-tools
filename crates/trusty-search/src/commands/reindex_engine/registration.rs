//! Index registration + status helpers shared by the `init`, `index`, and
//! `discover` flows.
//!
//! Why: both `Init` and `Index` need the same "POST /indexes, parse `created`"
//! dance, optionally forwarding per-index repo-config filters; the `--force`
//! pre-snapshot path also needs the current chunk count before reindex begins.
//! What: `RegisterFilters` (filter payload), `register_index_with_daemon{,_filtered}`
//! (idempotent register), `register_index_reporting_collision` (the same call
//! reporting a root-path 409 as data, #7758), and `fetch_chunk_count` (status
//! probe).
//! Test: `a_root_owned_by_another_index_is_reported_not_bailed` and the rest of
//! `super::tests`; the happy path is covered indirectly by `handle_index`.

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

/// What `POST /indexes` answered, for a caller that must act on the answer.
///
/// Why: #7758 — `register_index_with_daemon_filtered` collapsed every non-2xx
/// into one `daemon returned {status}` bail, so `index --force` could not tell
/// "this root is already indexed under another id" from any other failure and
/// aborted instead of doing the reindex its own `--help` promises. The status
/// line alone is not enough: the decision needs the id that owns the root, and
/// that only exists in the 409 body.
/// What: the three outcomes a caller distinguishes. Every other failure stays a
/// bail inside the resolver, so a caller matching these three has already
/// handled all of them.
/// Test: `a_root_owned_by_another_index_is_reported_not_bailed`,
/// `a_conflict_without_an_existing_id_still_bails`,
/// `a_successful_registration_reports_created`.
#[derive(Debug, PartialEq, Eq)]
pub enum RegisterOutcome {
    /// The index is registered. `created` is `false` for an idempotent
    /// re-registration of the same id over the same tree.
    Registered { created: bool },
    /// `409`: this `root_path` already belongs to a DIFFERENT index id
    /// (#2336, #3993). Carries that id and the daemon's own refusal text.
    RootOwnedBy {
        existing_id: String,
        refusal: String,
    },
    /// The daemon did not answer at all.
    Unreachable,
}

/// Variant of [`register_index_with_daemon`] that forwards filter/domain
/// fields in the request body so the daemon can store them on the handle.
///
/// Why: the filtered variant is needed when any of the optional fields are
/// non-empty or when `lexical_only` / `skip_kg` is set.
/// What: [`register_index_reporting_collision`] with the root-collision arm
/// flattened back into the historical bail, so callers that cannot act on a
/// collision are unchanged. Returns `(created, daemon_reachable)`.
/// Test: covered indirectly by `handle_index` integration tests.
pub async fn register_index_with_daemon_filtered(
    index_name: &str,
    project_path: &std::path::Path,
    filters: &RegisterFilters,
) -> Result<(bool, bool)> {
    match register_index_reporting_collision(index_name, project_path, filters).await? {
        RegisterOutcome::Registered { created } => Ok((created, true)),
        // #7758: the pre-existing message, unchanged, for every caller that has
        // no `--force` to honour.
        RegisterOutcome::RootOwnedBy { refusal, .. } => anyhow::bail!(refusal),
        RegisterOutcome::Unreachable => Ok((false, false)),
    }
}

/// `POST /indexes`, reporting a root-path collision instead of bailing on it.
///
/// Why: #7758 — see [`RegisterOutcome`]. `trusty-search index <root> --force`
/// on a root already registered under another id died here with
/// `daemon returned 409 Conflict for POST /indexes`, contradicting the flag's
/// own `--help` ("force a full reindex even if the index already has chunks").
/// The daemon is right to refuse a second registration over one corpus — two
/// indexes cannot share one `.redb` — so the fix belongs on this side: report
/// the owning id and let `index_one_with_filters` reindex it.
/// What: builds the JSON body from the non-empty filter fields, POSTs, and maps
/// the response onto [`RegisterOutcome`]. A `409` carrying an `existing_id`
/// becomes `RootOwnedBy`; every other non-2xx still bails with the historical
/// `daemon returned {status} for POST /indexes` text, which is what the
/// same-id-different-root refusal (a 409 with no `existing_id`) keeps getting.
/// Test: `a_root_owned_by_another_index_is_reported_not_bailed`.
pub async fn register_index_reporting_collision(
    index_name: &str,
    project_path: &std::path::Path,
    filters: &RegisterFilters,
) -> Result<RegisterOutcome> {
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
            Ok(RegisterOutcome::Registered { created })
        }
        Ok(resp) => {
            let refusal = format!("daemon returned {} for POST /indexes", resp.status());
            // #7758: only the root-collision 409 carries `existing_id`; it is
            // the sole refusal a reindex can satisfy instead.
            let existing_id = if resp.status() == reqwest::StatusCode::CONFLICT {
                resp.json::<serde_json::Value>()
                    .await
                    .ok()
                    .and_then(|body| {
                        body.get("existing_id")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    })
                    .filter(|id| !id.is_empty())
            } else {
                None
            };
            match existing_id {
                Some(existing_id) => Ok(RegisterOutcome::RootOwnedBy {
                    existing_id,
                    refusal,
                }),
                None => anyhow::bail!(refusal),
            }
        }
        Err(_) => Ok(RegisterOutcome::Unreachable),
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
