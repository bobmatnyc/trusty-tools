//! Index registration + status helpers shared by the `init`, `index`, and
//! `discover` flows.
//!
//! Why: both `Init` and `Index` need the same "register, parse `created`"
//! dance, optionally forwarding per-index repo-config filters; the `--force`
//! pre-snapshot path also needs the current chunk count before reindex begins.
//! #9214: every call goes over the daemon socket (`search.index.create`,
//! `search.index.status`); there is no HTTP path.
//! What: `RegisterFilters` (filter payload), `register_index_with_daemon{,_filtered}`
//! (idempotent register), `register_index_reporting_collision` (the same call
//! reporting a root-path 409 as data, #7758), and `fetch_chunk_count` (status
//! probe).
//! Test: `a_root_owned_by_another_index_is_reported_not_bailed` and the rest of
//! `super::tests`; the happy path is covered indirectly by `handle_index`.

use crate::commands::daemon_rpc::{index_status, rpc_error};
use crate::commands::explicit_target::lookup_index_by_path;
use anyhow::{Context as _, Result};
use trusty_search::service::daemon_client::{DaemonCallError, DaemonClient};
use trusty_search::service::rpc::writes::METHOD_INDEX_CREATE;

/// Register an index with the daemon (idempotent).
///
/// Why: factored out of `Init` and `Index` because both flows need the same
/// "register, parse `created`" dance.
/// What: returns `Ok((created, daemon_reachable))`. `daemon_reachable=false`
/// means nothing serves the daemon socket; every other failure is an error.
/// Test: covered indirectly by `handle_index` tests.
pub async fn register_index_with_daemon(
    index_name: &str,
    project_path: &std::path::Path,
) -> Result<(bool, bool)> {
    register_index_with_daemon_filtered(index_name, project_path, &RegisterFilters::default()).await
}

/// Optional repo-config filters carried in `search.index.create` params.
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
    /// What: when `true`, the `search.index.create` params include
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

/// What `search.index.create` answered, for a caller that must act on the answer.
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
    /// Nothing is serving the daemon socket. `reason` is the client's own
    /// text, which names the socket (#9214).
    Unreachable { reason: String },
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
    legacy_pair(register_index_reporting_collision(index_name, project_path, filters).await?)
}

/// Flatten a [`RegisterOutcome`] into the historical `(created, reachable)`.
///
/// What: a root collision is the daemon's refusal, raised (#7758); nothing
/// serving the socket is `(false, false)`.
/// Test: `the_legacy_register_wrapper_still_bails_on_a_root_collision`.
pub(super) fn legacy_pair(outcome: RegisterOutcome) -> Result<(bool, bool)> {
    match outcome {
        RegisterOutcome::Registered { created } => Ok((created, true)),
        // #7758: the refusal, for every caller that has no `--force` to honour.
        RegisterOutcome::RootOwnedBy { refusal, .. } => anyhow::bail!(refusal),
        RegisterOutcome::Unreachable { .. } => Ok((false, false)),
    }
}

/// Register over the daemon socket, reporting a root-path collision instead
/// of bailing on it.
///
/// Why: #7758 — see [`RegisterOutcome`]. `trusty-search index <root> --force`
/// on a root already registered under another id died here with a bare
/// conflict, contradicting the flag's own `--help` ("force a full reindex even
/// if the index already has chunks"). The daemon is right to refuse a second
/// registration over one corpus — two indexes cannot share one `.redb` — so
/// the fix belongs on this side: report the owning id and let
/// `index_one_with_filters` reindex it.
/// What: resolves the daemon socket and calls [`register_on`].
/// Test: `a_root_owned_by_another_index_is_reported_not_bailed`.
pub async fn register_index_reporting_collision(
    index_name: &str,
    project_path: &std::path::Path,
    filters: &RegisterFilters,
) -> Result<RegisterOutcome> {
    register_on(&DaemonClient::resolve()?, index_name, project_path, filters).await
}

/// `search.index.create` on `client`, mapped onto [`RegisterOutcome`].
///
/// Why (#9214 H2): only "nothing is serving the socket" may read as
/// [`RegisterOutcome::Unreachable`] — a daemon that answered and broke the
/// exchange, or refused, is a different fault. Every caller treats
/// `Unreachable` as "start the daemon", and `init`/`discover` treat it as a
/// soft skip, so folding every failure into it hid real refusals.
/// What: success reads `created`. A conflict whose owner is a different id —
/// named by the refusal's `data.existing_id` when the daemon sends one, else
/// found by a fail-closed lookup of `project_path` — becomes
/// [`RegisterOutcome::RootOwnedBy`]. Every other conflict, an invalid-params
/// refusal (#8922: its message names the glob), a broken exchange, and every
/// other refusal is an error carrying the daemon's text.
/// Test: `a_root_owned_by_another_index_is_reported_not_bailed`,
/// `a_conflict_without_an_existing_id_still_bails`,
/// `a_broken_exchange_is_an_error_not_unreachable`.
pub(crate) async fn register_on(
    client: &DaemonClient,
    index_name: &str,
    project_path: &std::path::Path,
    filters: &RegisterFilters,
) -> Result<RegisterOutcome> {
    let params = create_params(index_name, project_path, filters);
    match client.call(METHOD_INDEX_CREATE, params).await {
        Ok(body) => {
            let created = body
                .get("created")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            Ok(RegisterOutcome::Registered { created })
        }
        // #9214 H2: unreachable only; it used to be `Err(_) => Unreachable`.
        Err(e) if e.is_unreachable() => Ok(RegisterOutcome::Unreachable {
            reason: e.to_string(),
        }),
        Err(e) if e.is_conflict() => root_owner(client, index_name, project_path, e).await,
        Err(e) => Err(rpc_error(e)),
    }
}

/// The [`RegisterOutcome`] for a conflict refusal (#7758).
///
/// What: `existing_id` from the refusal's `data` when present; otherwise the
/// registration that owns `project_path`, looked up fail-closed (an unreadable
/// status refuses). An owner that is `index_name` itself, or no owner at all,
/// leaves the refusal an error — the same-id-different-root and overlap
/// conflicts name their cause in the daemon's message.
async fn root_owner(
    client: &DaemonClient,
    index_name: &str,
    project_path: &std::path::Path,
    e: DaemonCallError,
) -> Result<RegisterOutcome> {
    let named = e
        .data()
        .and_then(|d| d.get("existing_id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let refusal = rpc_error(e).to_string();
    let existing_id = match named {
        Some(id) => Some(id),
        None => lookup_index_by_path(client, project_path)
            .await
            .with_context(|| format!("{refusal}; and the owning index could not be identified"))?
            .map(|r| r.id)
            .filter(|id| id != index_name),
    };
    match existing_id {
        Some(existing_id) => Ok(RegisterOutcome::RootOwnedBy {
            existing_id,
            refusal,
        }),
        None => anyhow::bail!(refusal),
    }
}

/// The `search.index.create` params: id, root, and the non-default filters.
fn create_params(
    index_name: &str,
    project_path: &std::path::Path,
    filters: &RegisterFilters,
) -> serde_json::Value {
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
    create_body
}

/// Fetch an index's chunk count from its status. `None` when the status
/// cannot be read.
///
/// Why: the `--force` pre-snapshot path needs the current chunk count before
/// the reindex begins, so the final verify message can show "(was N)". The
/// number is cosmetic, so a failed read only drops that suffix; the kickoff
/// that follows reports any real fault.
/// What: `search.index.status` and its `chunk_count`.
/// Test: covered indirectly by `run_reindex_force_opts`.
pub async fn fetch_chunk_count(client: &DaemonClient, index_id: &str) -> Option<u64> {
    match index_status(client, index_id).await {
        Ok(body) => body.get("chunk_count").and_then(|v| v.as_u64()),
        Err(e) => {
            tracing::debug!(index_id, error = %e, "no prior chunk count");
            None
        }
    }
}
