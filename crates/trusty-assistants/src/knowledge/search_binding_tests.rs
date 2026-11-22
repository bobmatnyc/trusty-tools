//! Search-slot resolution after knowledge provisioning (#7902).
//!
//! Why: the default `vector_search` slot returned the protected extraction
//! index over an agent's declared `[[stores]].index`. These tests provision a
//! real knowledge state in a temporary assistants root and read the slot
//! through the same `tool` / `bound_index` entry points the runtime uses.

use chrono::Utc;

use super::{bound_index, tool};
use crate::agents::AgentConfig;
use crate::assistants::{ASSISTANTS_DIR_ENV, AssistantHome, AssistantInstanceId};
use crate::knowledge::KnowledgeStore;
use crate::stores::AgentStoreBinding;

/// Point the assistants root at `dir` under `ENV_LOCK`, restoring on drop.
struct AssistantsRoot {
    prev: Option<std::ffi::OsString>,
    // Dropped last, so the lock outlives the restore.
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl AssistantsRoot {
    fn set(dir: &std::path::Path) -> Self {
        let lock = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os(ASSISTANTS_DIR_ENV);
        // SAFETY: ENV_LOCK is held for this guard's lifetime.
        unsafe { std::env::set_var(ASSISTANTS_DIR_ENV, dir) };
        Self { prev, _lock: lock }
    }
}

impl Drop for AssistantsRoot {
    fn drop(&mut self) {
        // SAFETY: the lock is still held — it drops after this body.
        unsafe {
            match self.prev.take() {
                Some(v) => std::env::set_var(ASSISTANTS_DIR_ENV, v),
                None => std::env::remove_var(ASSISTANTS_DIR_ENV),
            }
        }
    }
}

const NAME: &str = "fixture-7902";

/// Initialize knowledge for [`NAME`] and return its store plus protected index id.
fn initialize(root: &std::path::Path) -> (KnowledgeStore, String, String) {
    let home = AssistantHome::under(root.to_path_buf(), AssistantInstanceId::new(NAME).unwrap());
    let store = KnowledgeStore::new(home);
    let state = store.initialize(Utc::now(), None).unwrap();
    (store, state.revision, state.store.index_id)
}

fn agent_with(binding: &AgentStoreBinding) -> AgentConfig {
    let mut cfg: AgentConfig = toml::from_str(&format!(
        "[agent]\nname='{NAME}'\nrole='assistant'\nmodel='m'\ndescription='d'\n\
         [llm]\ntemperature=0.0\nmax_tokens=64\n[system_prompt]\ncontent='x'\n"
    ))
    .unwrap();
    cfg.stores.bindings = vec![binding.clone()];
    cfg
}

/// #7902 regression: a declared store keeps the default slot after provisioning,
/// and the protected index stays queryable by id instead of vanishing.
#[test]
fn declared_store_index_holds_the_default_slot_after_provisioning() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let _root = AssistantsRoot::set(&root);
    let (store, revision, protected) = initialize(&root);
    let declared = AgentStoreBinding {
        name: "projects".into(),
        index: Some("cto-projects".into()),
        ..Default::default()
    };
    store
        .confirm_binding_with_legacy(&revision, Some(declared.clone()))
        .unwrap();
    let cfg = agent_with(&declared);

    assert_eq!(
        bound_index(NAME, &cfg.stores).unwrap().as_deref(),
        Some("cto-projects")
    );
    assert_eq!(
        tool(NAME, &cfg, vec![]).allowed_index_ids(),
        vec!["cto-projects".to_string(), protected],
        "declared index first, protected index still reachable"
    );
}

/// #7902 error arm: a declaration naming the protected index id is refused.
#[test]
fn a_declared_store_cannot_name_the_protected_index() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let _root = AssistantsRoot::set(&root);
    let (store, revision, protected) = initialize(&root);
    let hijack = AgentStoreBinding {
        name: "mine".into(),
        index: Some(protected.clone()),
        ..Default::default()
    };
    store
        .confirm_binding_with_legacy(&revision, Some(hijack.clone()))
        .unwrap();
    let cfg = agent_with(&hijack);

    let err = bound_index(NAME, &cfg.stores).unwrap_err().to_string();
    assert!(err.contains("protected knowledge index"), "{err}");
    assert!(err.contains(&protected), "{err}");
}

/// The protected binding itself cannot be pointed at another index.
#[test]
fn retargeting_the_protected_binding_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let _root = AssistantsRoot::set(&root);
    let (store, revision, _protected) = initialize(&root);
    store.confirm_binding_with_legacy(&revision, None).unwrap();
    let retargeted = AgentStoreBinding {
        name: "protected".into(),
        index: Some("foreign".into()),
        root: Some("okg".into()),
        ..Default::default()
    };

    let err = bound_index(NAME, &agent_with(&retargeted).stores).unwrap_err();
    assert!(err.to_string().contains("binding changed"), "{err}");
}
