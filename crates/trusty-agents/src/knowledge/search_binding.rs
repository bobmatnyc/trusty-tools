//! Query the protected extraction index without moving a legacy memory palace (#4283).
use crate::{
    assistants::{AssistantHome, AssistantInstanceId},
    stores::StoresConfig,
};

/// Why: a legacy palace may coexist with newly provisioned document knowledge.
/// What: select the persisted protected index only while its original configuration still matches.
/// Test: `protected_binding_cannot_be_removed_or_retargeted`.
pub fn bound_index(name: &str, stores: &StoresConfig) -> anyhow::Result<Option<String>> {
    let home = AssistantHome::under(
        crate::assistants::assistants_root()?,
        AssistantInstanceId::new(name)?,
    );
    let Some(state) = super::KnowledgeStore::new(home.clone()).status()? else {
        return Ok(stores.default_search_index().map(String::from));
    };
    anyhow::ensure!(
        state.binding_confirmed,
        "Protected knowledge setup is incomplete"
    );
    if let Some(legacy) = state.legacy_binding {
        let mut selected = stores
            .bindings
            .first()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Legacy knowledge binding was removed"))?;
        let mut legacy = legacy;
        selected.palace = None;
        legacy.palace = None;
        anyhow::ensure!(selected == legacy, "Legacy knowledge binding changed");
    } else {
        let binding = stores
            .bindings
            .first()
            .ok_or_else(|| anyhow::anyhow!("Protected knowledge binding was removed"))?;
        anyhow::ensure!(
            home.store_root(binding)? == state.store.root
                && binding.resolved_index() == state.store.index_id,
            "Protected knowledge binding changed"
        );
    }
    Ok(Some(state.store.index_id))
}

/// Build the same protected query tool for chat and subprocess assistants.
pub fn tool(
    name: &str,
    cfg: &crate::agents::AgentConfig,
    attached: Vec<String>,
) -> crate::tools::memory::VectorSearchTool {
    let (index, error) = match bound_index(name, &cfg.stores) {
        Ok(index) => (index, None),
        Err(error) => (None, Some(error.to_string())),
    };
    crate::tools::memory::VectorSearchTool::new()
        .with_default_index(index)
        .with_binding_error(error)
        .with_attached_indexes(attached)
        .with_index_enforcement(cfg.tools.search_indexes_enforced())
}
