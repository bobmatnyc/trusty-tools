//! Server-bound memory for both assistant execution paths (#7360).
use super::{ToolExecutor, ToolRegistry, ToolResult};
use crate::assistants::memory_policy::{self, MemoryPolicy};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

fn operation(name: &str) -> &str {
    name.rsplit("__").next().unwrap_or(name)
}
pub(crate) fn scope_for(name: &str) -> &'static str {
    if matches!(
        operation(name),
        "memory_recall"
            | "memory_recall_deep"
            | "memory_recall_all"
            | "memory_list"
            | "memory_search"
            | "search_memory"
    ) {
        "memory.read"
    } else {
        "memory.write"
    }
}
fn memory_tool(tool: &dyn ToolExecutor) -> bool {
    tool.scope().is_some_and(|s| s.starts_with("memory."))
        || operation(tool.name()).starts_with("memory_")
        || tool.name().contains("trusty-memory")
        || operation(tool.name()).starts_with("chat_")
        || operation(tool.name()).starts_with("kg_")
        || operation(tool.name()).starts_with("palace_")
        || operation(tool.name()).starts_with("room_")
        || operation(tool.name()).starts_with("drawer_")
        || matches!(
            operation(tool.name()),
            "get_prompt_context" | "list_prompt_facts"
        )
        || matches!(
            operation(tool.name()),
            "search_memory" | "store_memory" | "retrieve_memory" | "list_memory_keys"
        )
}

pub(crate) fn scope_granted(config: &crate::agents::AgentConfig, scope: &str) -> bool {
    use crate::tools::registry::scope::{Scope, ScopePattern};
    crate::agents::permissions::effective_scopes(&config.tools, &config.permissions)
        .unwrap_or_default()
        .iter()
        .any(|pattern| ScopePattern::new(pattern).matches(&Scope::new(scope)))
}

pub(crate) fn operation_granted(config: &crate::agents::AgentConfig, name: &str) -> bool {
    let root = match std::env::current_dir() {
        Ok(path) => path,
        Err(_) => return false,
    };
    let catalog = crate::skills::manifest::SkillCatalog::builtin().with_authored(
        crate::skills::manifest::authored::load_from_paths(
            &crate::skills::sources::SkillSourceRegistry::load(&root).resolved_paths(),
        ),
    );
    let (patterns, _) = crate::skills::manifest::effective_tool_patterns(
        config.tools.allow.as_ref(),
        config.skills.allow.as_ref(),
        &catalog,
    );
    scope_granted(config, scope_for(name))
        && patterns
            .is_some_and(|p| crate::ctrl::pm_task::tool_authz::allow_patterns_grant_tool(name, &p))
}

/// Why: discovery must never provide an unbound memory bypass.
/// What: replace every memory executor after discovery and reserve durable aliases.
/// Test: `namespace_arguments_reject_foreign_writes_and_default_broad_reads`.
pub fn bind(registry: &mut ToolRegistry, assistant: &str) {
    registry.tools.retain(|_, tool| {
        !memory_tool(tool.as_ref())
            || matches!(
                operation(tool.name()),
                "memory_remember"
                    | "memory_write"
                    | "memory_store"
                    | "memory_recall"
                    | "memory_recall_all"
                    | "memory_recall_deep"
                    | "memory_list"
                    | "memory_search"
                    | "search_memory"
            )
    });
    for tool in registry.tools.values_mut() {
        if memory_tool(tool.as_ref()) {
            *tool = Arc::new(BoundMemory {
                name: tool.name().into(),
                assistant: assistant.into(),
                original: Some(tool.clone()),
            });
        }
    }
    for name in ["memory_remember", "memory_write", "memory_recall"] {
        if !registry.contains(name) {
            registry.register(Arc::new(BoundMemory {
                name: name.into(),
                assistant: assistant.into(),
                original: None,
            }));
        }
    }
}

pub struct BoundMemory {
    name: String,
    assistant: String,
    original: Option<Arc<dyn ToolExecutor>>,
}

/// Closed host entry point; callers cannot supply a daemon tool name.
pub(crate) async fn execute_operation(assistant: &str, operation: &str, args: Value) -> ToolResult {
    if !matches!(
        operation,
        "memory_remember" | "memory_write" | "memory_recall"
    ) {
        return ToolResult::err("Unsupported memory operation");
    }
    BoundMemory {
        name: operation.into(),
        assistant: assistant.into(),
        original: None,
    }
    .execute(args)
    .await
}

fn arguments(
    policy: &MemoryPolicy,
    name: &str,
    mut args: Value,
) -> Result<(String, Value), String> {
    let op = match operation(name) {
        "memory_store" => "memory_remember",
        "memory_search" | "search_memory" => "memory_recall",
        op => op,
    };
    let write = matches!(op, "memory_remember" | "memory_write" | "memory_store");
    let supported = write
        || matches!(
            op,
            "memory_recall"
                | "memory_recall_deep"
                | "memory_recall_all"
                | "memory_list"
                | "memory_search"
                | "search_memory"
        );
    if !supported {
        return Err("This memory operation has no assistant namespace contract; use memory_remember or memory_recall".into());
    }
    let map = args
        .as_object_mut()
        .ok_or("Memory arguments must be an object")?;
    let explicit_broad = map
        .remove("across_palaces")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let wildcard = map.get("palace").and_then(Value::as_str) == Some("*");
    let broad = explicit_broad || (wildcard && policy.cross_palace_query);
    if broad && write {
        return Err("Durable writes cannot cross assistant namespaces".into());
    }
    if broad && !policy.cross_palace_query {
        return Err(
            "Enable cross-palace querying in Settings before requesting other namespaces".into(),
        );
    }
    // #7360: reject alternative scope selectors, never let them override identity.
    for key in [
        "palace",
        "palace_id",
        "namespace",
        "index",
        "index_id",
        "to_palace",
        "from_palace",
        "palaces",
    ] {
        if let Some(value) = map.get(key) {
            let own = value.as_str() == Some(policy.namespace.as_str());
            let broad_read = !write && (value.is_null() || value.as_str() == Some("*"));
            if !own && !broad_read && (write || !policy.cross_palace_query || key != "palace") {
                return Err(format!(
                    "Memory scope denied: {key} must be this assistant's namespace {}",
                    policy.namespace
                ));
            }
        }
    }
    let foreign = policy.cross_palace_query
        && !write
        && map
            .get("palace")
            .and_then(Value::as_str)
            .is_some_and(|p| p != "*" && p != policy.namespace);
    for key in [
        "palace_id",
        "namespace",
        "index",
        "index_id",
        "to_palace",
        "from_palace",
        "palaces",
    ] {
        map.remove(key);
    }
    let method = if write {
        "memory_remember"
    } else if broad {
        "memory_recall_all"
    } else if op == "memory_recall_all" && !policy.cross_palace_query {
        "memory_recall"
    } else {
        op
    };
    if !foreign {
        map.insert("palace".into(), json!(policy.namespace));
    }
    if method == "memory_recall_all" {
        if let Some(query) = map.remove("query") {
            map.entry("q").or_insert(query);
        }
    } else if let Some(query) = map.remove("q") {
        map.entry("query").or_insert(query);
    }
    if let Some(text) = map.remove("content") {
        map.entry("text").or_insert(text);
    }
    let permitted: &[&str] = if write {
        &["palace", "text", "context", "tags", "room"]
    } else if method == "memory_list" {
        &["palace", "tag", "limit", "room", "wing"]
    } else {
        &["palace", "query", "q", "top_k", "room", "wing"]
    };
    map.retain(|key, _| permitted.contains(&key.as_str()));
    if write {
        map.remove("allow_secret_like");
        map.insert("force".into(), json!(true));
    }
    Ok((method.into(), args))
}

#[async_trait]
impl ToolExecutor for BoundMemory {
    fn name(&self) -> &str {
        &self.name
    }
    fn scope(&self) -> Option<&str> {
        Some(scope_for(&self.name))
    }
    fn restricted_tiers(&self) -> &[crate::rbac::ServiceTier] {
        if matches!(
            operation(&self.name),
            "memory_remember" | "memory_write" | "memory_store"
        ) {
            return &[
                crate::rbac::ServiceTier::ReadOnly,
                crate::rbac::ServiceTier::Analytics,
            ];
        }
        self.original
            .as_ref()
            .map(|t| t.restricted_tiers())
            .unwrap_or(&[])
    }
    fn schema(&self) -> Value {
        let write = matches!(
            operation(&self.name),
            "memory_remember" | "memory_write" | "memory_store"
        );
        let list = operation(&self.name) == "memory_list";
        let mut properties = if write {
            json!({"text":{"type":"string","minLength":1},"tags":{"type":"array","items":{"type":"string"}},"context":{"type":"string"},"room":{"type":"string"}})
        } else if list {
            json!({"tag":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100},"room":{"type":"string"},"wing":{"type":"string"}})
        } else {
            json!({"query":{"type":"string"},"top_k":{"type":"integer","minimum":1,"maximum":50},"room":{"type":"string"},"wing":{"type":"string"},"across_palaces":{"type":"boolean","description":"Query all palaces when Settings enables cross-palace reads."}})
        };
        if !write {
            properties["palace"] = json!({"type":"string","description":"Defaults to this assistant; other palaces require cross-palace reads enabled."});
        }
        json!({"type":"function","function":{"name":self.name,"description":if write {"Persist a fact in this assistant's durable memory. A skipped or failed write is not saved."}else{"Recall durable memory in the assistant namespace; cross-palace reads require Settings opt-in."},"parameters":{"type":"object","additionalProperties":false,"properties":properties,"required":if write {vec!["text"]}else if list {vec![]}else{vec!["query"]}}}})
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let _write_guard = if scope_for(&self.name) == "memory.write" {
            let dirs = crate::agents::agents_dir_candidates();
            let Some((manifest, _)) =
                crate::api::server::agent_patch::resolve_agent_paths(&dirs, &self.assistant)
            else {
                return ToolResult::err("Assistant manifest unavailable");
            };
            match crate::knowledge::execution::mutation_guard(&manifest).await {
                Ok(guard) => Some(guard),
                Err(e) => return ToolResult::err(e.to_string()),
            }
        } else {
            None
        };
        // #7396: `operation_granted` reads the manifest and walks the skill
        // sources synchronously, so it runs off the runtime worker exactly as
        // the policy resolve below does.
        let assistant = self.assistant.clone();
        let tool_name = self.name.clone();
        let granted = tokio::task::spawn_blocking(move || {
            crate::agents::AgentConfig::by_name(&assistant)
                .map(|config| operation_granted(&config, &tool_name))
        })
        .await;
        match granted {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => {
                return ToolResult::err(
                    "This assistant does not have the required memory permission scope",
                );
            }
            Ok(Err(e)) => {
                return ToolResult::err(format!("Cannot resolve memory permissions: {e}"));
            }
            Err(e) => return ToolResult::err(e.to_string()),
        }
        let assistant = self.assistant.clone();
        let policy =
            match tokio::task::spawn_blocking(move || memory_policy::resolve(&assistant)).await {
                Ok(Ok(policy)) => policy,
                Ok(Err(e)) => return ToolResult::err(e.to_string()),
                Err(e) => return ToolResult::err(e.to_string()),
            };
        let (method, args) = match arguments(&policy, &self.name, args) {
            Ok(value) => value,
            Err(e) => return ToolResult::err(e),
        };
        let socket = crate::memory::trusty_client::default_trusty_socket();
        match call(&socket, &policy, &method, args).await {
            Ok(result) => ToolResult::ok(result.to_string()),
            Err(e) => ToolResult::err(format!("Durable memory operation failed: {e}")),
        }
    }
}

async fn call(
    socket: &std::path::Path,
    policy: &MemoryPolicy,
    method: &str,
    args: Value,
) -> anyhow::Result<Value> {
    use trusty_common::memory_rpc::call_memory_tool_at_with_timeout as rpc;
    let timeout = std::time::Duration::from_secs(30);
    if method == "memory_remember" {
        ensure_palace(socket, policy).await?;
    }
    let namespace = if method == "memory_recall_all" {
        json!("multiple; each result identifies palace_id")
    } else {
        args.get("palace")
            .cloned()
            .unwrap_or(json!(policy.namespace))
    };
    let result = rpc(socket, method, args, timeout).await?;
    if method == "memory_remember" {
        anyhow::ensure!(
            result.get("status").and_then(Value::as_str) == Some("stored"),
            "Memory was not stored: {result}"
        );
    }
    Ok(json!({"namespace":namespace,"assistant":policy.assistant_id,"result":result}))
}

pub(crate) async fn ensure_palace(
    socket: &std::path::Path,
    policy: &MemoryPolicy,
) -> anyhow::Result<()> {
    use trusty_common::memory_rpc::call_memory_tool_at_with_timeout as rpc;
    let timeout = std::time::Duration::from_secs(30);
    // Serialize first-write provision within this assistant, without resetting existing metadata.
    let list = rpc(socket, "palace_list", json!({}), timeout).await?;
    let palaces = list
        .get("palaces")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("Invalid memory palace catalogue"))?;
    if !palaces
        .iter()
        .any(|p| p.as_str() == Some(&policy.namespace))
    {
        rpc(socket,"palace_create",json!({"name":policy.namespace,"description":format!("Durable memory for assistant {}",policy.assistant_id),"force":true}),timeout).await?;
    }
    Ok(())
}

/// Automatic recall shares the explicit tool namespace; unavailable ownership disables only memory.
pub async fn bind_automatic(config: &mut crate::agents::AgentConfig, name: &str) {
    if !crate::assistants::is_assistant_role(&config.agent.role) {
        return;
    }
    let owned = name.to_owned();
    let policy = tokio::task::spawn_blocking(move || memory_policy::resolve(&owned)).await;
    match policy {
        Ok(Ok(policy)) => {
            if let Some(binding) = config.stores.bindings.first_mut() {
                binding.palace = Some(policy.namespace);
            } else {
                config
                    .stores
                    .bindings
                    .push(crate::stores::AgentStoreBinding {
                        name: name.into(),
                        palace: Some(policy.namespace),
                        ..Default::default()
                    });
            }
        }
        error => {
            tracing::warn!(
                assistant = name,
                ?error,
                "Assistant memory unavailable; continuing without automatic memory"
            );
            for binding in &mut config.stores.bindings {
                binding.palace = None;
            }
        }
    }
}

/// Automatic fact recall respects the same permission grant as explicit recall.
pub fn recall_stores(config: &crate::agents::AgentConfig) -> crate::stores::StoresConfig {
    let mut stores = config.stores.clone();
    if !scope_granted(config, "memory.read") {
        for binding in &mut stores.bindings {
            binding.palace = None;
        }
    }
    stores
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn memory_write_wait_observes_permission_revocation() {
        if std::env::var_os("TRUSTY_MEMORY_REVOCATION_CHILD").is_none() {
            let root = tempfile::tempdir().unwrap();
            let status = tokio::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "tools::assistant_memory::tests::memory_write_wait_observes_permission_revocation"])
                .env("TRUSTY_MEMORY_REVOCATION_CHILD", "1")
                .env("HOME", root.path())
                .env("TAGENT_CONFIG_DIR", root.path().join("config"))
                .env("TAGENT_PROJECT_DIR", root.path())
                .env("TAGENT_ASSISTANTS_DIR", root.path().join("assistants"))
                .env("TRUSTY_DATA_DIR_OVERRIDE", root.path().join("data"))
                .env("TRUSTY_MEMORY_SOCKET", root.path().join("missing.sock"))
                .env_remove("OPEN_MPM_CONFIG_DIR")
                .env_remove("OPEN_MPM_PROJECT_DIR")
                .status().await.unwrap();
            assert!(status.success());
            return;
        }
        use std::future::Future;
        let config = std::path::PathBuf::from(std::env::var_os("TAGENT_CONFIG_DIR").unwrap());
        let agents = config;
        std::fs::create_dir_all(&agents).unwrap();
        let manifest = agents.join("fixture.toml");
        let raw = "[agent]\nname='fixture'\nrole='assistant'\nmodel='fixture'\ndescription='Synthetic'\n[llm]\nmax_tokens=128\ntemperature=0.0\n[system_prompt]\ncontent='Synthetic'\n[tools]\nallow=['memory_write']\nscopes=['memory.write']\n";
        std::fs::write(
            agents.join("assistant.toml"),
            raw.replace("name='fixture'", "name='assistant'"),
        )
        .unwrap();
        let raw = raw.replace("name='fixture'", "name='fixture'\nextends='assistant'");
        std::fs::write(&manifest, &raw).unwrap();
        assert!(crate::agents::AgentConfig::by_name("fixture").is_ok());
        let gate = crate::knowledge::execution::mutation_guard(&manifest)
            .await
            .unwrap();
        let tool = BoundMemory {
            name: "memory_write".into(),
            assistant: "fixture".into(),
            original: None,
        };
        let mut operation = Box::pin(tool.execute(json!({"text":"Blue"})));
        std::future::poll_fn(|cx| {
            assert!(operation.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        std::fs::write(
            &manifest,
            format!("{}\n[permissions]\nreplace_scopes=true\nscopes=[]\n", raw),
        )
        .unwrap();
        drop(gate);
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), operation)
            .await
            .unwrap();
        assert!(result.is_error());
        assert!(
            result
                .content()
                .contains("required memory permission scope"),
            "{result:?}"
        );
    }
    #[test]
    fn reserved_write_overrides_never_cross_the_daemon_boundary() {
        let policy = MemoryPolicy {
            assistant_id: "own".into(),
            namespace: "own".into(),
            revision: "r".into(),
            cross_palace_query: false,
        };
        let (_,args)=arguments(&policy,"memory_write",json!({"text":"Blue","allow_secret_like":true,"force":false,"admin":true,"context":"preference"})).unwrap();
        assert_eq!(args["force"], true);
        assert!(args.get("allow_secret_like").is_none());
        assert!(args.get("admin").is_none());
        assert_eq!(args["palace"], "own");
    }
    #[tokio::test]
    async fn fresh_palace_is_created_once_and_skipped_write_is_not_saved() {
        let created = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = created.clone();
        let daemon=crate::uds_mock::spawn(move |method,params|{
            let created=created.clone();let method=method.to_owned();
            Box::pin(async move {match method.as_str(){
                "palace_list"=>Ok(json!({"palaces":if created.load(std::sync::atomic::Ordering::SeqCst){vec!["own"]}else{vec![]}})),
                "palace_create"=>{assert!(!created.swap(true,std::sync::atomic::Ordering::SeqCst));assert_eq!(params["name"],"own");Ok(json!({"status":"created"}))},
                "memory_remember"=>Ok(json!({"status":"skipped","reason":"secret gate"})),
                _=>Err(crate::uds_mock::RpcError::internal("unexpected method"))
            }})
        }).await;
        let policy = MemoryPolicy {
            assistant_id: "own".into(),
            namespace: "own".into(),
            revision: "r".into(),
            cross_palace_query: false,
        };
        for _ in 0..2 {
            let error = call(
                daemon.socket(),
                &policy,
                "memory_remember",
                json!({"palace":"own","text":"synthetic"}),
            )
            .await
            .unwrap_err();
            assert!(error.to_string().contains("secret gate"));
        }
        assert!(observed.load(std::sync::atomic::Ordering::SeqCst));
    }
    #[tokio::test]
    async fn bound_memory_wire_contract_keeps_default_reads_local_and_cross_reads_explicit() {
        use crate::uds_mock::{self, RpcError};
        let facts = Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::<
            String,
            String,
        >::new()));
        facts
            .lock()
            .unwrap()
            .insert("other".into(), "Foreign fact".into());
        let daemon = uds_mock::spawn(move |method: &str, params: Value| {
            let facts = facts.clone();
            let method = method.to_string();
            Box::pin(async move {
                match method.as_str() {
                    "palace_list" => Ok(json!({"palaces":["own","other"]})),
                    "memory_remember" => {
                        let palace = params["palace"].as_str().ok_or_else(|| RpcError::internal("missing palace"))?;
                        let text = params["text"].as_str().ok_or_else(|| RpcError::internal("missing text"))?;
                        facts.lock().unwrap().insert(palace.into(), text.into());
                        Ok(json!({"id":"saved-fact","palace":palace,"persisted":true,"status":"stored"}))
                    }
                    "memory_recall" => {
                        if params["query"].as_str().is_none() { return Err(RpcError::internal("memory_recall requires query")); }
                        let palace = params["palace"].as_str().ok_or_else(|| RpcError::internal("missing palace"))?;
                        Ok(json!({"results":[{"content":facts.lock().unwrap().get(palace),"palace_id":palace}]}))
                    }
                    "memory_recall_all" => {
                        if params["q"].as_str().is_none() { return Err(RpcError::internal("memory_recall_all requires q")); }
                        let results: Vec<_> = facts.lock().unwrap().iter().map(|(palace,text)| json!({"palace_id":palace,"content":text})).collect();
                        Ok(json!({"results":results}))
                    }
                    _ => Err(RpcError::method_not_found(&method, &[])),
                }
            })
        }).await;
        let mut policy = MemoryPolicy {
            assistant_id: "one".into(),
            namespace: "own".into(),
            revision: "r".into(),
            cross_palace_query: false,
        };
        let (method, args) =
            arguments(&policy, "memory_write", json!({"content":"A durable fact"})).unwrap();
        let saved = call(daemon.socket(), &policy, &method, args).await.unwrap();
        assert_eq!(saved["result"]["id"], "saved-fact");
        for name in [
            "memory_recall",
            "memory_recall_all",
            "mcp__trusty-memory__memory_recall",
        ] {
            let (method, args) = arguments(&policy, name, json!({"q":"fact"})).unwrap();
            let result = call(daemon.socket(), &policy, &method, args).await.unwrap();
            assert_eq!(result["result"]["results"].as_array().unwrap().len(), 1);
            assert_eq!(result["result"]["results"][0]["content"], "A durable fact");
        }
        let (method, args) = arguments(
            &policy,
            "memory_recall",
            json!({"query":"fact","palace":"*"}),
        )
        .unwrap();
        assert_eq!(args["palace"], "own");
        assert_eq!(method, "memory_recall");
        assert!(
            arguments(
                &policy,
                "memory_recall",
                json!({"query":"fact","across_palaces":true})
            )
            .is_err()
        );
        policy.cross_palace_query = true;
        let (method, args) = arguments(
            &policy,
            "memory_recall",
            json!({"query":"fact","across_palaces":true}),
        )
        .unwrap();
        let result = call(daemon.socket(), &policy, &method, args).await.unwrap();
        assert_eq!(result["result"]["results"].as_array().unwrap().len(), 2);
        assert!(
            arguments(
                &policy,
                "memory_remember",
                json!({"text":"bad","palace":"other"})
            )
            .is_err()
        );
    }
    #[test]
    fn bound_writers_deny_read_only_tiers_and_reserved_memory_names_are_wrapped() {
        let mut registry = ToolRegistry::new();
        bind(&mut registry, "one");
        for name in ["memory_remember", "memory_write"] {
            assert!(
                registry.tools[name]
                    .restricted_tiers()
                    .contains(&crate::rbac::ServiceTier::ReadOnly)
            );
            assert!(
                registry.tools[name]
                    .restricted_tiers()
                    .contains(&crate::rbac::ServiceTier::Analytics)
            );
        }
        struct Raw {
            name: &'static str,
        }
        #[async_trait]
        impl ToolExecutor for Raw {
            fn name(&self) -> &str {
                self.name
            }
            fn schema(&self) -> Value {
                json!({})
            }
            async fn execute(&self, _: Value) -> ToolResult {
                panic!("Raw executor must never run")
            }
        }
        registry.register(Arc::new(Raw { name: "kg_assert" }));
        bind(&mut registry, "one");
        assert!(!registry.contains("kg_assert"));
        assert!(
            arguments(
                &MemoryPolicy {
                    assistant_id: "one".into(),
                    namespace: "own".into(),
                    revision: "r".into(),
                    cross_palace_query: true
                },
                "kg_assert",
                json!({})
            )
            .is_err()
        );
    }
    /// #7443's invariant, restated against the daemon-bound path (#7396).
    ///
    /// Why: the local palace store this guarantee used to be proved against is
    /// gone, and its proof (`native_memory::tests::two_assistants_never_cross_read`)
    /// went with it. What replaced the store is a policy whose namespace is
    /// derived per assistant — but `namespace_arguments_reject_foreign_writes_\
    /// and_default_broad_reads` exercises ONE policy's argument rewriting, so
    /// nothing asserted that two assistants with two distinct namespaces cannot
    /// read each other THROUGH the daemon. That is the statement the guarantee
    /// actually makes, so it gets asserted end to end over the wire.
    /// What: two policies, a fact written under each through `BoundMemory`'s own
    /// argument rewriting, then every read shape each one can issue with
    /// `cross_palace_query=false` — including the ones that name the other
    /// palace outright. Each assistant sees only its own fact, and the flag that
    /// would change that is not reachable from a turn
    /// (`tools::concierge::tests::attended_turns_cannot_patch_permissions_or_\
    /// cross_palace_reads`).
    /// Test: this function IS the test.
    #[tokio::test]
    async fn two_assistants_never_cross_read() {
        use crate::uds_mock::{self, RpcError};
        let facts = Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::<
            String,
            String,
        >::new()));
        let store = facts.clone();
        let daemon = uds_mock::spawn(move |method: &str, params: Value| {
            let facts = store.clone();
            let method = method.to_string();
            Box::pin(async move {
                match method.as_str() {
                    "palace_list" => Ok(json!({"palaces":["alpha","beta"]})),
                    "memory_remember" => {
                        let palace = params["palace"].as_str().unwrap().to_owned();
                        let text = params["text"].as_str().unwrap().to_owned();
                        facts.lock().unwrap().insert(palace, text);
                        Ok(json!({"status":"stored"}))
                    }
                    // The daemon answers exactly what it was asked for. Any
                    // cross-read therefore has to come from the arguments this
                    // crate built, which is the layer under test.
                    "memory_recall" => {
                        let palace = params["palace"].as_str().unwrap();
                        Ok(json!({"results":facts.lock().unwrap().get(palace)
                            .map(|t| vec![json!({"palace_id":palace,"content":t})])
                            .unwrap_or_default()}))
                    }
                    _ => Err(RpcError::method_not_found(&method, &[])),
                }
            })
        })
        .await;

        let policy = |assistant: &str, namespace: &str| MemoryPolicy {
            assistant_id: assistant.into(),
            namespace: namespace.into(),
            revision: "r".into(),
            cross_palace_query: false,
        };
        let one = policy("one", "alpha");
        let two = policy("two", "beta");
        for (policy, secret) in [(&one, "Alpha's secret"), (&two, "Beta's secret")] {
            let (method, args) =
                arguments(policy, "memory_write", json!({ "text": secret })).unwrap();
            call(daemon.socket(), policy, &method, args).await.unwrap();
        }

        for (mine, theirs, own_secret) in [
            (&one, "beta", "Alpha's secret"),
            (&two, "alpha", "Beta's secret"),
        ] {
            for request in [
                json!({"query":"secret"}),
                json!({"query":"secret","palace":theirs}),
                json!({"query":"secret","palace":"*"}),
            ] {
                let Ok((method, args)) = arguments(mine, "memory_recall", request.clone()) else {
                    // Refusing the request outright is the stronger answer.
                    continue;
                };
                let result = call(daemon.socket(), mine, &method, args).await.unwrap();
                let hits = result["result"]["results"].as_array().unwrap();
                assert_eq!(
                    hits.len(),
                    1,
                    "{} saw {hits:?} for {request}",
                    mine.namespace
                );
                assert_eq!(
                    hits[0]["content"], own_secret,
                    "{} read another assistant's namespace: {hits:?}",
                    mine.namespace
                );
            }
        }
    }

    #[test]
    fn namespace_arguments_reject_foreign_writes_and_default_broad_reads() {
        for cross in [false, true] {
            let p = MemoryPolicy {
                assistant_id: "cto-assistant".into(),
                namespace: "cto".into(),
                revision: "r".into(),
                cross_palace_query: cross,
            };
            for tool in [
                "memory_remember",
                "memory_write",
                "mcp__trusty-memory__memory_remember",
            ] {
                assert!(arguments(&p, tool, json!({"palace":"other","text":"fact"})).is_err());
                assert!(arguments(&p, tool, json!({"palace":"*","text":"fact"})).is_err());
                assert_eq!(
                    arguments(&p, tool, json!({"text":"fact"})).unwrap().1["palace"],
                    "cto"
                );
            }
            assert_eq!(
                arguments(&p, "memory_recall", json!({"q":"fact"}))
                    .unwrap()
                    .1["palace"],
                "cto"
            );
            assert_eq!(
                arguments(&p, "memory_recall", json!({"q":"fact","palace":"other"})).is_ok(),
                cross
            );
            assert_eq!(
                arguments(&p, "memory_recall_all", json!({"q":"fact"}))
                    .unwrap()
                    .0,
                if cross {
                    "memory_recall_all"
                } else {
                    "memory_recall"
                }
            );
            assert!(arguments(&p, "memory_send_message", json!({})).is_err());
        }
    }
}
