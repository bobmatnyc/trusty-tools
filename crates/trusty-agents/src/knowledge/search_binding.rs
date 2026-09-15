//! Query the protected extraction index without moving a legacy memory palace (#4283).
use crate::{
    assistants::{AssistantHome, AssistantInstanceId},
    stores::StoresConfig,
};

/// The indexes `vector_search` answers from for one assistant (#7902).
///
/// Why: once knowledge provisioning ran, the default slot returned the
/// protected extraction index even when the agent declared its own
/// `[[stores]].index`, so the declared corpus became unreachable while the
/// stores API still reported it as the bound index.
/// What: `default_index` is the slot an unqualified `vector_search` uses;
/// `protected_index` is the provisioned extraction index, `None` before
/// provisioning.
/// Test: `declared_store_index_holds_the_default_slot_after_provisioning`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchSlots {
    pub default_index: Option<String>,
    pub protected_index: Option<String>,
}

/// Why: a legacy palace may coexist with newly provisioned document knowledge.
/// What: the default search slot from [`search_slots`].
/// Test: `protected_binding_cannot_be_removed_or_retargeted`,
/// `a_declared_store_cannot_name_the_protected_index`.
pub fn bound_index(name: &str, stores: &StoresConfig) -> anyhow::Result<Option<String>> {
    Ok(search_slots(name, stores)?.default_index)
}

/// Resolve both search slots for `name`, refusing a binding that changed.
///
/// Why: #7902 — a declared store index must answer the default slot, and the
/// protected index must stay protected rather than be rebound by a declaration.
/// What: without knowledge state the declared index is the default. With a
/// confirmed legacy binding (a declared store outside the protected root) the
/// declared index is the default and the protected index stays reachable by
/// id; a declared store naming the protected index id is an error. With no
/// legacy binding the first binding must still name the protected root and
/// index, or the call errors.
/// Test: `declared_store_index_holds_the_default_slot_after_provisioning`,
/// `a_declared_store_cannot_name_the_protected_index`,
/// `retargeting_the_protected_binding_is_refused`.
pub fn search_slots(name: &str, stores: &StoresConfig) -> anyhow::Result<SearchSlots> {
    let home = AssistantHome::under(
        crate::assistants::assistants_root()?,
        AssistantInstanceId::new(name)?,
    );
    let Some(state) = super::KnowledgeStore::new(home.clone()).status()? else {
        return Ok(SearchSlots {
            default_index: stores.default_search_index().map(String::from),
            protected_index: None,
        });
    };
    anyhow::ensure!(
        state.binding_confirmed,
        "Protected knowledge setup is incomplete"
    );
    let protected = state.store.index_id;
    if let Some(mut legacy) = state.legacy_binding {
        let mut selected = stores
            .bindings
            .first()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Legacy knowledge binding was removed"))?;
        selected.palace = None;
        legacy.palace = None;
        anyhow::ensure!(selected == legacy, "Legacy knowledge binding changed");
        let declared = selected.resolved_index().to_string();
        // #7902: a declaration may not take over the protected index id.
        anyhow::ensure!(
            declared != protected,
            "Store `{}` names the protected knowledge index `{protected}`; a declared store \
             must use its own index",
            selected.name
        );
        // #7902: the declared index answers the default slot, not the protected one.
        return Ok(SearchSlots {
            default_index: Some(declared),
            protected_index: Some(protected),
        });
    }
    let binding = stores
        .bindings
        .first()
        .ok_or_else(|| anyhow::anyhow!("Protected knowledge binding was removed"))?;
    anyhow::ensure!(
        home.store_root(binding)? == state.store.root && binding.resolved_index() == protected,
        "Protected knowledge binding changed"
    );
    Ok(SearchSlots {
        default_index: Some(protected.clone()),
        protected_index: Some(protected),
    })
}

/// Build the same protected query tool for chat and subprocess assistants.
///
/// What: the default slot from [`search_slots`]; when the protected index is
/// not the default, it joins the attached indexes so it stays queryable by id
/// (#7902). A resolution error becomes the tool's binding error.
/// Test: `declared_store_index_holds_the_default_slot_after_provisioning`.
pub fn tool(
    name: &str,
    cfg: &crate::agents::AgentConfig,
    mut attached: Vec<String>,
) -> crate::tools::memory::VectorSearchTool {
    let (slots, error) = match search_slots(name, &cfg.stores) {
        Ok(slots) => (slots, None),
        Err(error) => (SearchSlots::default(), Some(error.to_string())),
    };
    if let Some(protected) = slots.protected_index
        && slots.default_index.as_ref() != Some(&protected)
    {
        attached.push(protected);
    }
    crate::tools::memory::VectorSearchTool::new()
        .with_default_index(slots.default_index)
        .with_binding_error(error)
        .with_attached_indexes(attached)
        .with_index_enforcement(cfg.tools.search_indexes_enforced())
}

#[cfg(test)]
#[path = "search_binding_tests.rs"]
mod tests;
