//! Every declared binding of the active agent that does not resolve (#7903).
//!
//! Why: #7882 made a missing `[[stores]]` index an explicit error in
//! `tagent system status`, but a `[tools].search_indexes` entry, a listener
//! binding, or an MCP override naming something that does not exist still
//! failed soft: the binding was skipped at use and reported nowhere.
//! What: [`unresolved_bindings`] is the pure check over a loaded config and the
//! targets observed on this machine; [`collect`] gathers those targets and the
//! store statuses for `gather`. A target that could not be observed at all
//! (trusty-search down, `config.toml` unreadable) is not reported as a missing
//! binding — the daemon section already reports the outage, and the read
//! failure logs a WARN — matching #7882's soft `DaemonUnreachable`.
//! Test: `super::bindings_tests`.

use serde::Serialize;
use serde_json::Value;

use crate::agents::AgentConfig;
use crate::channels::ChannelScope;
use crate::mcp::{McpTier, ServerStatus};
use crate::stores::StoreStatus;

/// What kind of declaration failed to resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingKind {
    /// A `[[stores]]` binding or the default `vector_search` slot.
    Store,
    /// A `[tools].search_indexes` entry.
    SearchIndex,
    /// A listener binding naming a harness listener.
    Channel,
    /// An MCP server this assistant's `[mcp]` table declares.
    McpServer,
}

impl BindingKind {
    /// The label `render_text` prints.
    pub fn label(self) -> &'static str {
        match self {
            BindingKind::Store => "store",
            BindingKind::SearchIndex => "search_index",
            BindingKind::Channel => "channel",
            BindingKind::McpServer => "mcp_server",
        }
    }
}

/// One declared binding that does not resolve, with an operator-facing error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnresolvedBinding {
    pub kind: BindingKind,
    pub name: String,
    pub error: String,
}

/// What exists on this machine for the declarations to resolve against.
///
/// What: `None` for a catalogue that could not be read, which skips that
/// check (see the module doc).
#[derive(Debug, Default)]
pub struct BindingTargets<'a> {
    pub stores: &'a [StoreStatus],
    pub search_binding_error: Option<&'a str>,
    pub search_indexes: Option<&'a [String]>,
    pub global_listeners: Option<&'a [String]>,
    pub mcp_global: &'a [String],
    pub mcp_disabled: &'a [String],
    pub mcp_statuses: &'a [ServerStatus],
}

/// Every declared binding of `cfg` that `targets` does not satisfy (#7903).
///
/// What: in order — stores carrying a #7882 `error`; a default search slot
/// that failed to resolve (#7902); `search_indexes` ids missing from the
/// daemon catalogue; enabled listener bindings (assistant channels with no
/// provider of their own) naming no harness listener; `[mcp] disabled` names
/// matching no global server; assistant-tier MCP servers that are enabled but
/// unusable.
/// Test: `reports_every_unresolved_binding_of_every_kind`,
/// `a_fully_resolved_config_reports_nothing`,
/// `global_tier_mcp_status_is_excluded_with_reason`.
pub fn unresolved_bindings(
    agent: &str,
    cfg: &AgentConfig,
    targets: &BindingTargets<'_>,
) -> Vec<UnresolvedBinding> {
    let mut out = Vec::new();
    let mut push = |kind, name: &str, error: String| {
        out.push(UnresolvedBinding {
            kind,
            name: name.to_string(),
            error,
        })
    };
    for store in targets.stores {
        if let Some(error) = &store.error {
            push(BindingKind::Store, &store.name, error.clone());
        }
    }
    if let Some(error) = targets.search_binding_error {
        push(
            BindingKind::Store,
            "vector_search",
            format!("assistant `{agent}`'s default search slot does not resolve: {error}"),
        );
    }
    if let (Some(declared), Some(catalogue)) = (&cfg.tools.search_indexes, targets.search_indexes) {
        for id in declared.iter().filter(|id| !catalogue.contains(id)) {
            push(
                BindingKind::SearchIndex,
                id,
                format!(
                    "assistant `{agent}` attaches search index `{id}`, which is not registered \
                     on the trusty-search daemon"
                ),
            );
        }
    }
    if let Some(listeners) = targets.global_listeners {
        let dangling = cfg.channels.iter().filter(|c| {
            c.scope == ChannelScope::Assistant
                && c.provider.is_empty()
                && c.enabled
                && !listeners.contains(&c.id)
        });
        for channel in dangling {
            push(
                BindingKind::Channel,
                &channel.id,
                format!(
                    "assistant `{agent}` binds listener `{}`, which the harness config does not \
                     define",
                    channel.id
                ),
            );
        }
    }
    for name in targets
        .mcp_disabled
        .iter()
        .filter(|n| !targets.mcp_global.contains(n))
    {
        push(
            BindingKind::McpServer,
            name,
            format!(
                "assistant `{agent}`'s `[mcp] disabled` list names `{name}`, but no global MCP \
                 server named `{name}` exists"
            ),
        );
    }
    // #7903 review: `McpTier::Global` is filtered out deliberately, not an
    // oversight. `grade()` (`crate::mcp::shared::resolve`) tiers a server
    // `Assistant` only when THIS agent's own `[mcp]` table names it (an
    // override or a `disabled` entry); a `Global`-tier status is inherited
    // unchanged from the shared `servers.toml` this agent never declared.
    // Reporting it here would misattribute a shared config problem as this
    // agent's own dangling binding, and every assistant sharing that server
    // would report the identical entry. Its health is already surfaced,
    // scoped to the whole harness rather than one agent, on the same report's
    // `SystemStatusReport::mcp_servers` (`system_status::mcp_server_status`).
    // Test: `global_tier_mcp_status_is_excluded_with_reason`.
    for status in targets
        .mcp_statuses
        .iter()
        .filter(|s| s.tier == McpTier::Assistant && s.enabled && !s.usable)
    {
        let reason = status.reason.as_deref().unwrap_or("not usable");
        push(
            BindingKind::McpServer,
            &status.name,
            format!(
                "assistant `{agent}` declares MCP server `{}`: {reason}",
                status.name
            ),
        );
    }
    out
}

/// Store statuses and unresolved bindings for `agent`, observed live.
///
/// Why (#7882, #7903): the stores and every other binding are resolved from one
/// config load, through the same `resolve_store_statuses` the sidecar uses.
/// What: an agent that will not load yields two empty vecs, matching every
/// other subsystem's "degrade, never fail" contract.
/// Test: `super::tests::gather_report_serializes_to_json_with_expected_keys`.
pub(super) async fn collect(agent: &str) -> (Vec<StoreStatus>, Vec<UnresolvedBinding>) {
    let Ok(cfg) = AgentConfig::by_name_async(agent).await else {
        return (Vec::new(), Vec::new());
    };
    let stores = if cfg.stores.bindings.is_empty() {
        Vec::new()
    } else {
        crate::stores::resolve_store_statuses(
            agent,
            &cfg.stores,
            trusty_common::resolve_daemon_base_url("trusty-search").as_deref(),
            trusty_common::memory_rpc::resolve_memory_socket()
                .ok()
                .as_deref(),
        )
        .await
    };
    let search_binding_error = crate::knowledge::search_binding::bound_index(agent, &cfg.stores)
        .err()
        .map(|e| e.to_string());
    let search_indexes = match &cfg.tools.search_indexes {
        Some(ids) if !ids.is_empty() => search_catalogue().await,
        _ => None,
    };
    let global_listeners = global_listener_names().await;
    let mcp = crate::mcp::resolve_here(Some(agent)).await;
    let mcp_global: Vec<String> = mcp.global.iter().map(|s| s.name.clone()).collect();
    let unresolved = unresolved_bindings(
        agent,
        &cfg,
        &BindingTargets {
            stores: &stores,
            search_binding_error: search_binding_error.as_deref(),
            search_indexes: search_indexes.as_deref(),
            global_listeners: global_listeners.as_deref(),
            mcp_global: &mcp_global,
            mcp_disabled: &mcp.overrides.disabled,
            mcp_statuses: &mcp.statuses,
        },
    );
    (stores, unresolved)
}

/// Index ids registered on trusty-search, or `None` when it cannot be asked.
async fn search_catalogue() -> Option<Vec<String>> {
    use trusty_common::search_rpc;
    let socket = search_rpc::search_socket().ok()?;
    let reply = search_rpc::call_at(
        &socket,
        search_rpc::METHOD_INDEXES_LIST,
        serde_json::json!({}),
        super::daemons::PROBE_TIMEOUT,
    )
    .await
    .inspect_err(|e| {
        tracing::warn!(error = %e, "system status: search index catalogue unavailable; search_indexes not checked");
    })
    .ok()?;
    let list = reply
        .get("indexes")
        .and_then(Value::as_array)
        .or_else(|| reply.as_array())?;
    Some(
        list.iter()
            .filter_map(|v| v.get("id").or_else(|| v.get("index_id")))
            .filter_map(Value::as_str)
            .map(String::from)
            .collect(),
    )
}

/// Listener names the harness `config.toml` defines, read without creating it.
async fn global_listener_names() -> Option<Vec<String>> {
    let path = crate::mcp::GlobalConfig::config_path().ok()?;
    let raw = match tokio::fs::read_to_string(&path).await {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(Vec::new()),
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "system status: harness config unreadable; listener bindings not checked");
            return None;
        }
    };
    match crate::mcp::GlobalConfig::from_toml_str(&raw) {
        Ok(cfg) => Some(cfg.listeners().into_iter().map(|l| l.name).collect()),
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "system status: harness config invalid; listener bindings not checked");
            None
        }
    }
}
