//! Unresolved-binding reporting for `tagent system status` (#7903).

use super::bindings::{BindingKind, BindingTargets, unresolved_bindings};
use crate::agents::AgentConfig;
use crate::mcp::{McpTier, ServerStatus};
use crate::stores::{StoreFault, StoreStatus};

/// An agent declaring one binding of every kind that can dangle.
fn agent() -> AgentConfig {
    let mut cfg: AgentConfig = toml::from_str(
        r#"
[agent]
name = "fixture"
role = "assistant"
model = "m"
description = "d"
[llm]
temperature = 0.0
max_tokens = 64
[system_prompt]
content = "x"
[tools]
search_indexes = ["present-index", "ghost-index"]
[[listeners]]
name = "gmail-personal"
[[listeners]]
name = "ghost-listener"
[[stores]]
name = "dead-kb"
"#,
    )
    .expect("fixture parses");
    cfg.absorb_legacy_listeners();
    cfg
}

fn store(error: Option<&str>) -> StoreStatus {
    StoreStatus {
        name: "dead-kb".into(),
        tree: "okg://fixture".into(),
        index: "dead-kb".into(),
        palace: None,
        connected: error.is_none(),
        reason: error.map(|_| "missing".to_string()),
        chunk_count: None,
        root_path: None,
        index_status: None,
        palace_connected: None,
        palace_reason: None,
        tree_path: None,
        pending_index: None,
        synced_index: None,
        failed_stages: Vec::new(),
        fault: error.map(|_| StoreFault::MissingIndex),
        error: error.map(str::to_string),
    }
}

fn server(name: &str, tier: McpTier, usable: bool) -> ServerStatus {
    ServerStatus {
        name: name.into(),
        tier,
        enabled: true,
        usable,
        reason: (!usable).then(|| "credential GH_TOKEN does not resolve".to_string()),
    }
}

fn identities(found: &[super::bindings::UnresolvedBinding]) -> Vec<(BindingKind, String)> {
    let mut ids: Vec<_> = found.iter().map(|b| (b.kind, b.name.clone())).collect();
    ids.sort();
    ids
}

/// #7903 regression: six dangling bindings of four kinds all reach the report.
#[test]
fn reports_every_unresolved_binding_of_every_kind() {
    let cfg = agent();
    // The #7882 message shape: `StoreFault::error_for` names the assistant.
    let stores = [store(Some(
        "assistant `fixture` binds store `dead-kb` to trusty-search index `dead-kb`, which does not exist on the daemon",
    ))];
    let catalogue = ["present-index".to_string()];
    let listeners = ["gmail-personal".to_string()];
    let global = ["real-mcp".to_string()];
    let disabled = ["real-mcp".to_string(), "ghost-mcp".to_string()];
    let statuses = [
        server("assistant-mcp", McpTier::Assistant, false),
        server("global-broken", McpTier::Global, false),
        server("ok-mcp", McpTier::Assistant, true),
    ];
    let targets = BindingTargets {
        stores: &stores,
        search_binding_error: Some("Legacy knowledge binding changed"),
        search_indexes: Some(&catalogue),
        global_listeners: Some(&listeners),
        mcp_global: &global,
        mcp_disabled: &disabled,
        mcp_statuses: &statuses,
    };

    let found = unresolved_bindings("fixture", &cfg, &targets);

    assert_eq!(found.len(), 6, "{found:#?}");
    assert_eq!(
        identities(&found),
        vec![
            (BindingKind::Store, "dead-kb".to_string()),
            (BindingKind::Store, "vector_search".to_string()),
            (BindingKind::SearchIndex, "ghost-index".to_string()),
            (BindingKind::Channel, "ghost-listener".to_string()),
            (BindingKind::McpServer, "assistant-mcp".to_string()),
            (BindingKind::McpServer, "ghost-mcp".to_string()),
        ]
    );
    assert!(
        found.iter().all(|b| b.error.contains("fixture")),
        "{found:#?}"
    );
    assert!(
        !identities(&found).contains(&(BindingKind::McpServer, "global-broken".to_string())),
        "a Global-tier server's own brokenness is not this agent's dangling binding \
         (see bindings.rs's citation on the mcp_statuses filter); it surfaces on \
         SystemStatusReport::mcp_servers instead: {found:#?}"
    );
}

/// #7903 review: an unusable `McpTier::Global` server must be excluded from
/// `unresolved_bindings` for a stated reason, not merely absent because
/// nothing asserts it either way. This isolates that exclusion from the
/// six-binding fixture above so the two never drift.
#[test]
fn global_tier_mcp_status_is_excluded_with_reason() {
    let cfg = agent();
    let stores = [store(None)];
    let catalogue = ["present-index".to_string(), "ghost-index".to_string()];
    let listeners = ["gmail-personal".to_string(), "ghost-listener".to_string()];
    let global = ["real-mcp".to_string(), "global-broken".to_string()];
    let disabled = ["real-mcp".to_string()];
    let statuses = [server("global-broken", McpTier::Global, false)];
    let targets = BindingTargets {
        stores: &stores,
        search_binding_error: None,
        search_indexes: Some(&catalogue),
        global_listeners: Some(&listeners),
        mcp_global: &global,
        mcp_disabled: &disabled,
        mcp_statuses: &statuses,
    };

    assert_eq!(
        unresolved_bindings("fixture", &cfg, &targets),
        Vec::new(),
        "a Global-tier server this agent never declared is not the agent's own \
         dangling binding; its health belongs to SystemStatusReport::mcp_servers"
    );
}

/// The same declarations, all satisfied, report nothing.
#[test]
fn a_fully_resolved_config_reports_nothing() {
    let cfg = agent();
    let stores = [store(None)];
    let catalogue = ["present-index".to_string(), "ghost-index".to_string()];
    let listeners = ["gmail-personal".to_string(), "ghost-listener".to_string()];
    let global = ["real-mcp".to_string()];
    let disabled = ["real-mcp".to_string()];
    let statuses = [server("ok-mcp", McpTier::Assistant, true)];
    let targets = BindingTargets {
        stores: &stores,
        search_binding_error: None,
        search_indexes: Some(&catalogue),
        global_listeners: Some(&listeners),
        mcp_global: &global,
        mcp_disabled: &disabled,
        mcp_statuses: &statuses,
    };

    assert_eq!(unresolved_bindings("fixture", &cfg, &targets), Vec::new());
}
