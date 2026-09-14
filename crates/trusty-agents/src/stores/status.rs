//! Live resolution of OKG store bindings against the running daemons
//! (#3816/#3864, DOC-54 SPEC-AGENTS-04 §5.1).
//!
//! Why: A binding in `agent.toml` is a claim, not a fact. The GUI's OKG
//! Stores pane previously rendered a hardcoded "not connected" placeholder
//! precisely because nothing checked whether the named index existed; this
//! module turns the claim into an observed status by asking trusty-search
//! (`GET /indexes/{id}/status`) and trusty-memory (`GET
//! /api/v1/palaces/{id}/drawers?limit=1`) directly. Every failure mode —
//! daemon undiscoverable, connection refused, 404, malformed body, timeout —
//! collapses to `connected: false` plus a human-readable `reason`, mirroring
//! `crate::system_status::daemons`' established "down is a normal, reportable
//! state" contract. A bound store that cannot be resolved must never stop an
//! agent booting.
//!
//! **Issue #4115: HTTP reachability is not corpus health.** A 2xx response
//! with a parseable body used to be treated as `connected: true`
//! unconditionally — but trusty-search can warm-boot an index whose corpus
//! failed to open (`chunk_count: 0`, every staged-pipeline lane `Failed`)
//! while still answering the status probe successfully, which reported a
//! healthy-looking green card for a store that returns zero results for
//! every query. `connected` is now derived from the response's `stages`
//! object (see [`failed_stages`]) — the exact ground truth trusty-search's
//! own `/health` handler already uses (`IndexStages::any_failed`,
//! `core::registry.rs`) — not from HTTP status alone.
//! **The two daemons no longer share a transport (#6286).** trusty-search is
//! still loopback HTTP; ADR-0032 moved trusty-memory onto a Unix socket.
//! `resolve_daemon_base_url("trusty-memory")` reads an `http_addr` file nothing
//! writes any more, so `memory_base` was permanently `None` and EVERY
//! memory-backed store reported `palace_connected: false` with "daemon not
//! discoverable" whether or not the daemon was up. The palace half dials the
//! socket through `trusty_common::memory_rpc`.
//!
//! **Issue #7882: a missing index is not a soft state.** Every failure mode
//! above collapsing to `connected: false` also collapsed the one failure a
//! HUMAN must fix — a binding naming an index that was never created — into
//! the same shape as a stopped daemon, with nothing periodic or startup-level
//! reporting it. cto-assistant's `[[stores]]` binding sat on a nonexistent
//! index for weeks that way (#7876). [`StoreFault`] classifies the
//! not-connected paths and [`StoreStatus::error`] carries the actionable
//! message for the standing-misconfiguration class only, so a down daemon
//! stays distinguishable from a bad binding on every surface.
//!
//! What: [`StoreStatus`] is the per-store report (serialized straight to the
//! sidecar API and the GUI card). [`resolve_store_statuses`] resolves a whole
//! [`StoresConfig`]; the search base URL and the memory socket are injected so
//! tests can point at a mock rather than the developer's live daemons.
//! Test: `super::status::tests` — a mock trusty-search router and a mock memory
//! socket cover connected, missing, and unreachable; config-validation
//! short-circuiting is covered without any network at all.

use std::time::Duration;

use serde::Serialize;

use super::config::{AgentStoreBinding, StoresConfig};

/// Per-probe timeout.
///
/// Why: Identical rationale (and value) to
/// [`crate::system_status::daemons::PROBE_TIMEOUT`] — a daemon that accepts
/// the TCP connection but never answers must degrade to "not connected"
/// within a couple of seconds rather than stalling agent boot or an API
/// request behind it.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Why a store is not connected, classified (#7882).
///
/// Why: `connected: false` plus a prose `reason` collapsed two categories a
/// client must treat differently — a TRANSIENT operational state (the daemon
/// is down, the corpus failed to open) that clears when the daemon comes back,
/// and a STANDING MISCONFIGURATION (the binding names an index that was never
/// created) that clears only when a human acts. cto-assistant's binding named
/// a nonexistent index for weeks precisely because the two looked identical
/// (#7876). This enum is the machine-readable distinction; [`StoreStatus::error`]
/// is the operator-facing half.
/// What: one variant per not-connected path in [`resolve_one`], serialized
/// `snake_case`. [`StoreFault::MissingIndex`] — and only it — is an error:
/// the daemon ANSWERED and said the index id is not registered, which no
/// amount of waiting fixes. `DaemonUnreachable` deliberately stays a soft
/// state so a laptop with trusty-search stopped does not report every store
/// as broken.
/// Test: `reports_a_missing_index_as_an_error_naming_index_and_assistant`,
/// `daemon_unreachable_stays_distinguishable_from_a_missing_index`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StoreFault {
    /// The binding itself is unusable; nothing was probed.
    InvalidBinding,
    /// trusty-search could not be found or could not be reached.
    DaemonUnreachable,
    /// trusty-search answered `404` — the bound index id does not exist.
    MissingIndex,
    /// The index exists but its corpus or a staged lane is broken (#4115),
    /// or the daemon answered unusably.
    IndexUnhealthy,
}

impl StoreFault {
    /// The operator-facing message for a fault that will never self-heal.
    ///
    /// Why (#7882): only a fault a HUMAN must fix earns an `error` field —
    /// promoting a down daemon to an error would train readers to ignore the
    /// field entirely, which is the failure mode this issue is about.
    /// What: `Some(message)` for [`StoreFault::MissingIndex`], naming the
    /// assistant, the store and the index id so the line is actionable
    /// without cross-referencing `agent.toml`; `None` for every other fault,
    /// whose prose already lives in `reason`.
    /// Test: `reports_a_missing_index_as_an_error_naming_index_and_assistant`.
    fn error_for(self, agent_name: &str, store: &str, index: &str) -> Option<String> {
        match self {
            StoreFault::MissingIndex => Some(format!(
                "assistant `{agent_name}` binds store `{store}` to trusty-search index \
                 `{index}`, which does not exist on the daemon — create the index or \
                 correct the binding"
            )),
            _ => None,
        }
    }
}

/// One bound store's resolved, live status.
///
/// Why: The GUI card and the sidecar API need exactly one shape covering
/// both "CONNECTED, here is the index and how big it is" and "NOT CONNECTED,
/// here is why" — a client should never have to distinguish an absent field
/// from a failed probe. `reason` is `Some` only when `connected` is false.
/// What: `name`/`tree`/`index`/`palace` echo the resolved binding (defaults
/// already applied); `connected` reflects the SEARCH INDEX only, since that
/// is the corpus `vector_search` routes to. Palace health is reported
/// separately in `palace_connected`/`palace_reason` so a missing palace
/// downgrades one line of the card rather than the whole store.
/// Test: `resolves_connected_store_with_stats`,
/// `reports_missing_index_as_not_connected`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StoreStatus {
    pub name: String,
    pub tree: String,
    pub index: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub palace: Option<String>,
    pub connected: bool,
    /// Why the store is not connected. `None` when `connected` is true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Chunk count reported by the index, when the probe succeeded and the
    /// daemon supplied one. Cheap — it comes from the same status call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk_count: Option<u64>,
    /// The indexed root directory, when reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_path: Option<String>,
    /// The index's own readiness string (`"ready"`, `"indexing"`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_status: Option<String>,
    /// `None` when the binding declares no palace; otherwise whether the
    /// palace was observed to exist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub palace_connected: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub palace_reason: Option<String>,
    /// The `okg://` tree resolved to a real directory (#3892). `None` when the
    /// URI does not resolve — the state that made the two facets independent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree_path: Option<String>,
    /// Entities this store has INGESTED but not yet made SEARCHABLE (#3892).
    ///
    /// Why: `connected` says the index exists; it says nothing about whether the
    /// tree's content is in it. A store can be connected, non-empty, and still
    /// be missing everything the last ingest wrote — the failure this field
    /// exists to make visible. `None` when the tree has no OKG source registry
    /// (a hand-built tree), which is not the same as zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_index: Option<usize>,
    /// Entities recorded as current in the index. Same nullability as
    /// `pending_index`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synced_index: Option<usize>,
    /// Names of the `/indexes/{id}/status` `stages` (`lexical`/`semantic`/
    /// `graph`) reporting `status: "failed"` (issue #4115).
    ///
    /// Why: trusty-search can warm-boot an index whose durable corpus (or a
    /// single lane) failed to open — `chunk_count: 0`, every stage `Failed` —
    /// while the HTTP probe itself still returns 2xx with a parseable body.
    /// HTTP reachability is not corpus health: a bare boolean `connected`
    /// collapsed those two into the same "green" outcome, which is exactly
    /// how `cto-duetto` reported `connected: true` while answering zero
    /// queries. This field is the distinct degraded/failed signal the
    /// boolean cannot carry — empty when every present stage is healthy,
    /// non-empty (and `connected` forced to `false`) otherwise. Mirrors
    /// trusty-search's own `IndexStages::any_failed()` ground truth
    /// (`core/registry.rs`), which `/health` already uses for the identical
    /// reason.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub failed_stages: Vec<String>,
    /// Which category of failure this is (#7882). `None` when connected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fault: Option<StoreFault>,
    /// An operator-actionable error naming the assistant and the index id.
    ///
    /// Why (#7882): `Some` ONLY for a standing misconfiguration — today, a
    /// bound index the daemon says does not exist. A client that renders this
    /// field as an error, and `reason` as a soft state, gets the fail-open
    /// case surfaced without also crying wolf every time a daemon is stopped.
    /// Test: `reports_a_missing_index_as_an_error_naming_index_and_assistant`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl StoreStatus {
    /// A status carrying only the binding's identity and a disconnect reason.
    ///
    /// Why: Every not-connected path (invalid config, undiscoverable daemon,
    /// 404, timeout) produces the same shape; building it in one place keeps
    /// the `Option` fields from drifting out of sync.
    /// What: #7882 makes `fault` mandatory at every call site — a new
    /// not-connected path cannot be added without classifying it, which is
    /// how the missing-index case stayed indistinguishable from a down daemon.
    fn disconnected(
        binding: &AgentStoreBinding,
        agent_name: &str,
        reason: String,
        fault: StoreFault,
    ) -> Self {
        let error = fault.error_for(agent_name, &binding.name, binding.resolved_index());
        Self {
            name: binding.name.clone(),
            tree: binding.resolved_tree(agent_name),
            index: binding.resolved_index().to_string(),
            palace: binding.palace.clone(),
            connected: false,
            reason: Some(reason),
            chunk_count: None,
            root_path: None,
            index_status: None,
            palace_connected: None,
            palace_reason: None,
            tree_path: None,
            pending_index: None,
            synced_index: None,
            failed_stages: Vec::new(),
            fault: Some(fault),
            error,
        }
    }
}

/// The knowledge directory holding one KB tree per assistant.
///
/// Mirrors `trusty-kb`'s own default (and `tools::okg::knowledge_dir`) so every
/// surface addresses the same trees: `$KB_KNOWLEDGE_DIR`, else
/// `<home>/.trusty-agents/knowledge`.
fn knowledge_dir() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("KB_KNOWLEDGE_DIR") {
        return std::path::PathBuf::from(dir);
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".trusty-agents")
        .join("knowledge")
}

/// Fill in the tree-side half of a store's status (#3892).
///
/// Why: the probe above answers "does the index exist?"; this answers "and does
/// it hold what the tree holds?". Both halves are needed before a card can
/// honestly say a store is working.
/// What: resolves the `okg://` tree to a directory and, when that directory has
/// an OKG source registry, folds its index coverage. Local filesystem work only
/// — one `stat` per settled entity — and every failure leaves the fields `None`
/// rather than degrading the connection verdict.
/// Test: `reports_the_unsearchable_backlog_for_a_bound_tree`.
fn attach_tree_coverage(status: &mut StoreStatus) {
    let Some(tree_path) = super::binding::okg_tree_path(&knowledge_dir(), &status.tree) else {
        return;
    };
    status.tree_path = Some(tree_path.display().to_string());
    if !tree_path.is_dir() {
        return;
    }
    let store =
        trusty_kb::store::KbStore::new(tree_path, trusty_kb::schema::Profile::default_profile());
    if let Ok(coverage) = store.okg_index_coverage() {
        status.pending_index = Some(coverage.pending);
        status.synced_index = Some(coverage.synced);
    }
}

/// Resolve every binding in `stores` against the live daemons.
///
/// Why: One entry point for both the sidecar API (`GET
/// /api/agents/:name/stores`) and any boot-time reporting, so the GUI and the
/// logs can never disagree about whether a store is connected.
/// What: `search_base` is a full base URL (e.g. `http://127.0.0.1:7878`) and
/// `memory_socket` is the path trusty-memory binds; `None` for either means
/// that daemon was not discoverable and the halves it backs resolve to
/// not-connected with that as the reason — both injected rather than resolved
/// internally so tests can drive a mock. Returns one [`StoreStatus`] per
/// binding, in declaration order. Never errors.
/// Test: `resolves_connected_store_with_stats`,
/// `reports_missing_index_as_not_connected`,
/// `reports_undiscoverable_search_daemon`,
/// `invalid_binding_short_circuits_without_network`.
pub async fn resolve_store_statuses(
    agent_name: &str,
    stores: &StoresConfig,
    search_base: Option<&str>,
    memory_socket: Option<&std::path::Path>,
) -> Vec<StoreStatus> {
    let client = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            // Building a client cannot normally fail; if it does, report it
            // rather than panicking an agent's boot path.
            return stores
                .bindings
                .iter()
                .map(|b| {
                    StoreStatus::disconnected(
                        b,
                        agent_name,
                        format!("HTTP client unavailable: {e}"),
                        StoreFault::DaemonUnreachable,
                    )
                })
                .collect();
        }
    };

    let mut out = Vec::with_capacity(stores.bindings.len());
    for binding in &stores.bindings {
        out.push(resolve_one(&client, agent_name, binding, search_base, memory_socket).await);
    }
    out
}

/// Resolve a single binding. See [`resolve_store_statuses`].
async fn resolve_one(
    client: &reqwest::Client,
    agent_name: &str,
    binding: &AgentStoreBinding,
    search_base: Option<&str>,
    memory_socket: Option<&std::path::Path>,
) -> StoreStatus {
    // Config problems short-circuit before any network call — an unusable
    // binding has nothing meaningful to probe.
    if let Some(issue) = binding.validate() {
        return StoreStatus::disconnected(binding, agent_name, issue, StoreFault::InvalidBinding);
    }

    let index = binding.resolved_index().to_string();
    let Some(search_base) = search_base else {
        return StoreStatus::disconnected(
            binding,
            agent_name,
            "trusty-search daemon not discoverable (no address file; is it running?)".to_string(),
            StoreFault::DaemonUnreachable,
        );
    };

    let url = format!(
        "{}/indexes/{}/status",
        search_base.trim_end_matches('/'),
        index
    );
    let mut status = match client.get(&url).send().await {
        Err(e) => {
            return StoreStatus::disconnected(
                binding,
                agent_name,
                format!("trusty-search unreachable at {search_base}: {e}"),
                StoreFault::DaemonUnreachable,
            );
        }
        Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => {
            // #7882: the daemon ANSWERED and said the id is unknown — a
            // standing misconfiguration, not a transient outage. Log it on
            // every probe (stderr, per the daemon-stdout rule) so a binding
            // pointing at a never-created index cannot sit unnoticed the way
            // cto-assistant's did (#7876).
            tracing::warn!(
                agent = agent_name,
                store = %binding.name,
                index = %index,
                "bound store names a trusty-search index that does not exist"
            );
            return StoreStatus::disconnected(
                binding,
                agent_name,
                format!("search index `{index}` is not registered on the trusty-search daemon"),
                StoreFault::MissingIndex,
            );
        }
        Ok(resp) if !resp.status().is_success() => {
            let code = resp.status();
            return StoreStatus::disconnected(
                binding,
                agent_name,
                format!("trusty-search returned HTTP {code} for index `{index}`"),
                StoreFault::IndexUnhealthy,
            );
        }
        Ok(resp) => match resp.json::<serde_json::Value>().await {
            Err(e) => {
                return StoreStatus::disconnected(
                    binding,
                    agent_name,
                    format!("trusty-search returned an unreadable status body: {e}"),
                    StoreFault::IndexUnhealthy,
                );
            }
            Ok(body) => {
                // Issue #4115: HTTP 2xx + parseable JSON only proves the
                // daemon answered — it does NOT prove the corpus opened. A
                // warm-boot `corpus_open_failed` marks every stage `Failed`
                // (trusty-search's `derive_warm_boot_stages`) while `status`
                // and `chunk_count: 0` still look plausible. `failed_stages`
                // is the ground truth `/health` already uses
                // (`IndexStages::any_failed`, `core/registry.rs`); a
                // non-empty result forces `connected: false` regardless of
                // what the top-level `status` string claims.
                let failed_stages = failed_stages(&body);
                let index_status = body
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                let (connected, reason, fault) = if failed_stages.is_empty() {
                    (true, None, None)
                } else {
                    (
                        false,
                        Some(format!(
                            "index `{index}` is reachable but its corpus failed to open — \
                             {} stage(s) report `failed` ({}); the index answers no results \
                             regardless of what `status` claims",
                            failed_stages.len(),
                            failed_stages.join(", "),
                        )),
                        Some(StoreFault::IndexUnhealthy),
                    )
                };
                StoreStatus {
                    name: binding.name.clone(),
                    tree: binding.resolved_tree(agent_name),
                    index: index.clone(),
                    palace: binding.palace.clone(),
                    connected,
                    reason,
                    chunk_count: body.get("chunk_count").and_then(serde_json::Value::as_u64),
                    root_path: body
                        .get("root_path")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    index_status,
                    palace_connected: None,
                    palace_reason: None,
                    tree_path: None,
                    pending_index: None,
                    synced_index: None,
                    failed_stages,
                    fault,
                    // #7882: a reachable index is never a standing
                    // misconfiguration — a broken corpus is operational.
                    error: None,
                }
            }
        },
    };

    if let Some(palace) = &binding.palace {
        let (ok, reason) = probe_palace(memory_socket, palace).await;
        status.palace_connected = Some(ok);
        status.palace_reason = reason;
    }
    attach_tree_coverage(&mut status);

    status
}

/// The three staged-pipeline lane names trusty-search's `/indexes/{id}/status`
/// nests under `stages` (issue #109 Phase 1: `lexical` → `semantic` →
/// `graph`, `core::registry::IndexStages`).
const STAGE_NAMES: &[&str] = &["lexical", "semantic", "graph"];

/// Which of `body["stages"]`'s three lanes report `status: "failed"`.
///
/// Why (issue #4115): trusty-search serialises `IndexStages` (three
/// `StageState`s) under `stages`; a stage's `status` is one of `pending` /
/// `in_progress` / `ready` / `skipped` / `failed` (snake_case,
/// `core::registry::StageStatus`). `core::registry::IndexStages::any_failed()`
/// is the exact ground truth trusty-search's OWN `/health` handler uses to
/// detect a warm-booted-but-broken index (issue #1870) — this function reads
/// the same three fields off the wire JSON rather than depending on
/// trusty-search's internal types directly (`IndexStages`/`StageState` do not
/// derive `Deserialize`; only the wire shape is a stable contract here).
/// What: names of every stage present in `body["stages"]` whose `status` is
/// exactly `"failed"`, in `lexical, semantic, graph` order. An absent
/// `stages` object (a trusty-search version predating issue #109) yields an
/// empty vec — this is additive detection, never a new false-negative on an
/// older daemon.
/// Test: `resolves_connected_store_with_stats` (empty — healthy stages),
/// `reports_corpus_open_failed_as_not_connected`,
/// `failed_stages_ignores_a_missing_stages_object`.
fn failed_stages(body: &serde_json::Value) -> Vec<String> {
    let Some(stages) = body.get("stages") else {
        return Vec::new();
    };
    STAGE_NAMES
        .iter()
        .filter(|name| {
            stages
                .get(**name)
                .and_then(|s| s.get("status"))
                .and_then(serde_json::Value::as_str)
                == Some("failed")
        })
        .map(|name| (*name).to_string())
        .collect()
}

/// Probe whether `palace` exists on the trusty-memory daemon.
///
/// Why: `memory.drawers_list` with `limit: 1` is the cheapest existence check
/// that does NOT create anything — `palace_create`, which the rest of this
/// crate uses (`memory::trusty_client`), is an ensure/create and would silently
/// conjure a palace a status probe has no business creating. This is the
/// method its REST predecessor (`GET …/drawers?limit=1`) folded onto.
/// What: `(true, None)` when the daemon answers. A not-found refusal is the
/// palace being absent — read off `MemoryRpcError::is_not_found`, the same
/// distinction the 404 status carried — and everything else is the daemon being
/// unreachable or in trouble, carrying its own message.
/// Test: `reports_missing_palace_without_downgrading_index`,
/// `reports_unopenable_palace_as_a_server_error_not_an_absence`.
async fn probe_palace(
    memory_socket: Option<&std::path::Path>,
    palace: &str,
) -> (bool, Option<String>) {
    let Some(socket) = memory_socket else {
        return (
            false,
            Some("trusty-memory daemon not discoverable (is it running?)".to_string()),
        );
    };
    match trusty_common::memory_rpc::call_memory_tool_at_with_timeout(
        socket,
        "memory.drawers_list",
        serde_json::json!({ "palace_id": palace, "limit": 1 }),
        PROBE_TIMEOUT,
    )
    .await
    {
        Ok(_) => (true, None),
        Err(e) => (false, Some(describe_palace_failure(&e, palace))),
    }
}

/// Turn a palace-probe failure into the reason a card renders.
///
/// Kept beside [`probe_palace`] rather than inlined so
/// `api::server::agent_kg`'s equivalent can be read against it: the two must
/// describe the same down daemon the same way, or one pane contradicts another.
fn describe_palace_failure(e: &anyhow::Error, palace: &str) -> String {
    match e.downcast_ref::<trusty_common::memory_rpc::MemoryRpcError>() {
        Some(rpc) if rpc.is_not_found() => format!("memory palace `{palace}` does not exist"),
        Some(rpc) => format!("trusty-memory refused the read for palace `{palace}`: {rpc}"),
        None => format!("trusty-memory unreachable: {e:#}"),
    }
}

// Split into a sibling file (issue #610's 500-SLOC production cap) —
// mirrors `index_feed.rs` / `index_feed_tests.rs` in this same directory.
#[cfg(test)]
#[path = "status_tests.rs"]
mod tests;
