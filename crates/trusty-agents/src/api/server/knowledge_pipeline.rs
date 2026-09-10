//! Assistant-only knowledge orchestration API (#4531, DOC-57 §4.7).
//! Why: clients need durable monthly requests and truthful dependency state.
//! What: validates Assistant membership and source scope before touching the private pipeline.
//! Test: `assistant_pipeline_rejects_specialists_without_creating_state`, `pipeline_projects_are_revisioned_and_registered`.
mod catalog;
mod indexing;
pub(crate) mod intake;
#[cfg(test)]
mod tests;

use crate::{
    agents::AgentConfig,
    assistants::{AssistantHome, AssistantInstanceId},
    knowledge::{KnowledgeError, KnowledgeState, KnowledgeStore, ProtectedStore, SourceDescriptor},
};
use axum::{Json, extract::Path as AxumPath, http::StatusCode};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf};

type Error = (StatusCode, Json<Value>);
fn failure(status: StatusCode, message: impl std::fmt::Display) -> Error {
    (status, Json(json!({"error":message.to_string()})))
}
fn bad(message: impl std::fmt::Display) -> Error {
    failure(StatusCode::BAD_REQUEST, message)
}
fn core_error(error: KnowledgeError) -> Error {
    let status = match error {
        KnowledgeError::BadRequest(_) => StatusCode::BAD_REQUEST,
        KnowledgeError::Conflict => StatusCode::CONFLICT,
        KnowledgeError::InvalidState(_) => StatusCode::UNPROCESSABLE_ENTITY,
        KnowledgeError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
    };
    failure(status, error)
}
async fn disk<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, KnowledgeError> + Send + 'static,
) -> Result<T, Error> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| {
            failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Knowledge operation interrupted",
            )
        })?
        .map_err(core_error)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Reconcile {
    revision: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Policy {
    revision: String,
    paused: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Projects {
    revision: String,
    chat_id: String,
    projects: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Backfill {
    revision: String,
    months: u32,
}

struct Context {
    name: String,
    home: AssistantHome,
    config: AgentConfig,
    manifest: PathBuf,
    raw: String,
    projects: BTreeMap<String, String>,
    listeners: Vec<crate::listeners::config::ListenerConfig>,
    channels: Vec<super::agent_channels::Binding>,
    search_socket: Option<PathBuf>,
    dirs: Vec<PathBuf>,
    registry_entries: Vec<crate::registry::ProjectEntry>,
    live: bool,
}

impl Context {
    async fn load(name: &str) -> Result<Self, Error> {
        let dirs = crate::agents::agents_dir_candidates();
        let root = crate::assistants::assistants_root()
            .map_err(|e| failure(StatusCode::SERVICE_UNAVAILABLE, e))?;
        let registry = crate::registry::ProjectRegistry::new()
            .map_err(|e| failure(StatusCode::SERVICE_UNAVAILABLE, e))?;
        let entries = registry
            .load()
            .await
            .map_err(|e| failure(StatusCode::SERVICE_UNAVAILABLE, e))?;
        let global = crate::mcp::config::GlobalConfig::load().await;
        let mut context = Self::at(
            &dirs,
            root,
            name,
            entries.into_values().collect(),
            global.listeners,
        )
        .await?;
        context.search_socket = trusty_common::search_rpc::search_socket().ok();
        context.live = true;
        Ok(context)
    }

    async fn at(
        dirs: &[PathBuf],
        root: PathBuf,
        name: &str,
        projects: Vec<crate::registry::ProjectEntry>,
        listeners: Vec<crate::listeners::config::ListenerConfig>,
    ) -> Result<Self, Error> {
        let id = AssistantInstanceId::new(name).map_err(bad)?;
        if id.as_str() != name {
            return Err(bad("Assistant ID must use its exact configured spelling"));
        }
        let (manifest, _) = super::agent_patch::resolve_agent_paths(dirs, name)
            .ok_or_else(|| failure(StatusCode::NOT_FOUND, "Assistant not found"))?;
        // A syntactically valid ID is not proof of Assistant membership (#4531).
        if !crate::assistants::discover_instances(dirs).contains(&id) {
            return Err(failure(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Knowledge stores belong to user-facing Assistants, not specialist or delegated agents",
            ));
        }
        let config = AgentConfig::by_name_in(dirs, name)
            .map_err(|e| failure(StatusCode::UNPROCESSABLE_ENTITY, e))?;
        if !crate::assistants::is_assistant_role(&config.agent.role) || name == "ctrl" {
            return Err(failure(
                StatusCode::UNPROCESSABLE_ENTITY,
                "This agent is not a user-facing Assistant",
            ));
        }
        let raw = tokio::fs::read_to_string(&manifest)
            .await
            .map_err(|e| failure(StatusCode::SERVICE_UNAVAILABLE, e))?;
        let (_, _, channels) = super::agent_channels::load_at(dirs, name).await?;
        Ok(Self {
            name: name.into(),
            home: AssistantHome::under(root, id),
            config,
            manifest,
            raw,
            projects: catalog::registered(projects.clone()),
            listeners,
            channels,
            search_socket: None,
            dirs: dirs.to_vec(),
            registry_entries: projects,
            live: false,
        })
    }

    /// Reload effective bindings for every admission attempt, including CAS retries.
    async fn reload(&self) -> Result<Self, Error> {
        if self.live {
            return Self::load(&self.name).await;
        }
        Self::at(
            &self.dirs,
            self.home
                .path()
                .parent()
                .ok_or_else(|| bad("Invalid Assistant home"))?
                .to_path_buf(),
            &self.name,
            self.registry_entries.clone(),
            self.listeners.clone(),
        )
        .await
    }

    fn store(&self) -> KnowledgeStore {
        KnowledgeStore::new(self.home.clone())
    }
    async fn state(&self) -> Result<Option<KnowledgeState>, Error> {
        let store = self.store();
        disk(move || store.status()).await
    }
    fn sources(
        &self,
        projects: &BTreeMap<String, Vec<String>>,
    ) -> Result<Vec<SourceDescriptor>, Error> {
        catalog::sources(
            projects,
            &self.projects,
            &self.config,
            &self.listeners,
            &self.channels,
        )
    }
    fn selected_store(&self) -> Result<Option<ProtectedStore>, Error> {
        let bindings = &self.config.stores.bindings;
        if bindings.is_empty() {
            return Ok(None);
        }
        if bindings.len() != 1 {
            return Err(failure(
                StatusCode::CONFLICT,
                "An Assistant must have exactly one protected OKG; reconcile the existing bindings first",
            ));
        }
        let binding = &bindings[0];
        if let Some(reason) = binding.validate() {
            return Err(failure(StatusCode::CONFLICT, reason));
        }
        if binding.root.is_none() {
            return Err(failure(
                StatusCode::CONFLICT,
                "The existing OKG uses a legacy shared-root binding. Migrate it to this Assistant's private home before enabling extraction; existing data has not been moved",
            ));
        }
        let root = self
            .home
            .store_root(binding)
            .map_err(|e| failure(StatusCode::CONFLICT, e))?;
        Ok(Some(ProtectedStore {
            root,
            index_id: binding.resolved_index().into(),
            protected: true,
        }))
    }

    async fn bind_new_store(&self, state: &KnowledgeState) -> Result<(), Error> {
        if !self.config.stores.bindings.is_empty() {
            return Ok(());
        }
        let _guard = super::AGENT_CONFIG_WRITE_LOCK.lock().await;
        if tokio::fs::read_to_string(&self.manifest)
            .await
            .map_err(|e| failure(StatusCode::SERVICE_UNAVAILABLE, e))?
            != self.raw
        {
            return Err(core_error(KnowledgeError::Conflict));
        }
        let mut doc = self.raw.parse::<toml_edit::DocumentMut>().map_err(bad)?;
        let mut binding = toml_edit::Table::new();
        binding["name"] = toml_edit::value(&state.store.index_id);
        binding["index"] = toml_edit::value(&state.store.index_id);
        binding["root"] = toml_edit::value("okg");
        binding["tree"] = toml_edit::value(format!("okg://{}", self.name));
        let mut bindings = toml_edit::ArrayOfTables::new();
        bindings.push(binding);
        doc["stores"] = toml_edit::Item::ArrayOfTables(bindings);
        super::agent_listeners::atomic_write(&self.manifest, doc.to_string().as_bytes())
            .await
            .map_err(|e| failure(StatusCode::SERVICE_UNAVAILABLE, e))
    }

    async fn envelope(
        &self,
        state: Option<KnowledgeState>,
        start_index: bool,
    ) -> Result<Json<Value>, Error> {
        let projects = state
            .as_ref()
            .map(|s| s.projects_by_chat.clone())
            .unwrap_or_default();
        let sources = self.sources(&projects)?;
        let store_issue = self
            .selected_store()
            .err()
            .map(|(_, Json(v))| v["error"].clone());
        let index = match state.as_ref() {
            Some(state) => {
                indexing::status(self.search_socket.as_deref(), &state.store, start_index).await
            }
            None => {
                json!({"connected":false,"reason":"Initialize this Assistant's knowledge pipeline to provision its protected store"})
            }
        };
        Ok(Json(
            json!({"assistant":self.name,"pipeline":state,"sources":sources,"index":index,"store_issue":store_issue}),
        ))
    }

    async fn current(&self, revision: &str) -> Result<KnowledgeState, Error> {
        let state = self.state().await?.ok_or_else(|| {
            failure(
                StatusCode::CONFLICT,
                "Initialize the knowledge pipeline first",
            )
        })?;
        if state.revision != revision {
            return Err(core_error(KnowledgeError::Conflict));
        }
        let selected = self.selected_store()?;
        if selected.as_ref() != Some(&state.store) {
            return Err(failure(
                StatusCode::CONFLICT,
                "Protected OKG binding changed; restore its original binding",
            ));
        }
        Ok(state)
    }
    async fn reconcile(&self, state: KnowledgeState) -> Result<KnowledgeState, Error> {
        let sources = self.sources(&state.projects_by_chat)?;
        let store = self.store();
        disk(move || store.reconcile(&state.revision, &sources, Utc::now())).await
    }
}

/// Read-only status; never starts or simulates extraction.
pub(super) async fn get(AxumPath(name): AxumPath<String>) -> Result<Json<Value>, Error> {
    let context = Context::load(&name).await?;
    let state = context.state().await?;
    context.envelope(state, false).await
}
/// Initialize and reconcile eligible sources, preserving immutable store identity.
pub(super) async fn post(
    AxumPath(name): AxumPath<String>,
    Json(request): Json<Reconcile>,
) -> Result<Json<Value>, Error> {
    let context = Context::load(&name).await?;
    initialize(context, request).await
}
async fn initialize(mut context: Context, request: Reconcile) -> Result<Json<Value>, Error> {
    let state = match context.state().await? {
        Some(state) => {
            if request.revision.as_deref() != Some(state.revision.as_str()) {
                return Err(core_error(KnowledgeError::Conflict));
            }
            if state.binding_confirmed {
                context.current(&state.revision).await?
            } else {
                match context.selected_store()? {
                    Some(selected) if selected != state.store => {
                        return Err(failure(
                            StatusCode::CONFLICT,
                            "Protected store setup conflicts with the current binding",
                        ));
                    }
                    None if state.store.root != context.home.okg_dir() => {
                        return Err(failure(
                            StatusCode::CONFLICT,
                            "Restore the original private OKG binding to complete setup",
                        ));
                    }
                    _ => state,
                }
            }
        }
        None => {
            if request.revision.is_some() {
                return Err(core_error(KnowledgeError::Conflict));
            }
            let selected = context.selected_store()?;
            let store = context.store();
            disk(move || store.initialize(Utc::now(), selected)).await?
        }
    };
    context.bind_new_store(&state).await?;
    if context.config.stores.bindings.is_empty() {
        context
            .config
            .stores
            .bindings
            .push(crate::stores::AgentStoreBinding {
                name: state.store.index_id.clone(),
                index: Some(state.store.index_id.clone()),
                root: Some("okg".into()),
                tree: Some(format!("okg://{}", context.name)),
                ..Default::default()
            });
    }
    let store = context.store();
    let state = disk(move || store.confirm_binding(&state.revision)).await?;
    let state = context.reconcile(state).await?;
    intake::replay(&context).await?;
    let state = context.state().await?.unwrap_or(state);
    context.envelope(Some(state), true).await
}
pub(super) async fn patch(
    AxumPath(name): AxumPath<String>,
    Json(request): Json<Policy>,
) -> Result<Json<Value>, Error> {
    let context = Context::load(&name).await?;
    let state = context.current(&request.revision).await?;
    let state = context.reconcile(state).await?;
    let store = context.store();
    let state = disk(move || store.set_paused(&state.revision, request.paused, Utc::now())).await?;
    intake::replay(&context).await?;
    let state = context.state().await?.unwrap_or(state);
    context.envelope(Some(state), false).await
}
pub(super) async fn projects(
    AxumPath(name): AxumPath<String>,
    Json(request): Json<Projects>,
) -> Result<Json<Value>, Error> {
    let context = Context::load(&name).await?;
    update_projects(context, request).await
}
async fn update_projects(context: Context, request: Projects) -> Result<Json<Value>, Error> {
    let state = context.current(&request.revision).await?;
    let projects = catalog::validate_projects(&request.projects, &context.projects)?;
    let mut selected = state.projects_by_chat.clone();
    selected.insert(request.chat_id.clone(), projects.clone());
    let sources = context.sources(&selected)?;
    let store = context.store();
    let state = disk(move || {
        store.update_projects(
            &request.revision,
            &request.chat_id,
            &projects,
            &sources,
            Utc::now(),
        )
    })
    .await?;
    context.envelope(Some(state), false).await
}
pub(super) async fn backfill(
    AxumPath(name): AxumPath<String>,
    Json(request): Json<Backfill>,
) -> Result<Json<Value>, Error> {
    if request.months == 0 || request.months > 12 {
        return Err(bad("Request between 1 and 12 additional months at a time"));
    }
    let context = Context::load(&name).await?;
    let state = context.current(&request.revision).await?;
    let state = context.reconcile(state).await?;
    let store = context.store();
    let state =
        disk(move || store.extend_history(&state.revision, request.months, Utc::now())).await?;
    context.envelope(Some(state), false).await
}

/// Self-only native tools share the operator service; the caller fixes Assistant identity.
pub(crate) async fn assistant_history(
    name: &str,
    action: &str,
    revision: Option<String>,
    months: Option<u32>,
) -> Result<Value, Error> {
    let Json(value) = match action {
        "get" if revision.is_none() && months.is_none() => get(AxumPath(name.into())).await?,
        "initialize" if months.is_none() => {
            post(AxumPath(name.into()), Json(Reconcile { revision })).await?
        }
        "backfill" => {
            backfill(
                AxumPath(name.into()),
                Json(Backfill {
                    revision: revision.ok_or_else(|| {
                        bad("Read the pipeline revision before requesting history")
                    })?,
                    months: months.ok_or_else(|| bad("Choose the number of additional months"))?,
                }),
            )
            .await?
        }
        _ => {
            return Err(bad(
                "Use get, initialize, or backfill with the documented arguments",
            ));
        }
    };
    Ok(value)
}

/// Provision eligible Assistants and replay their pending identities without delaying API readiness.
/// A failed legacy binding remains an explicit per-Assistant issue; no extraction runs here.
pub(super) async fn startup() {
    let dirs = crate::agents::agents_dir_candidates();
    for id in crate::assistants::discover_instances(&dirs) {
        let result = async {
            let context = Context::load(id.as_str()).await?;
            let revision = context.state().await?.map(|s| s.revision);
            initialize(context, Reconcile { revision }).await
        }
        .await;
        if let Err((status, Json(error))) = result {
            tracing::warn!(assistant=%id, %status, reason=%error["error"], "Assistant knowledge setup needs attention");
        }
    }
}
