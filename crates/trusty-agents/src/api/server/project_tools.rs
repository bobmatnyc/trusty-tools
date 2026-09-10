//! Registered-project indexing and additive import into an assistant's bound OKG.
//! Uses the search socket and existing docstore tool; clients cannot override
//! destination trees/indexes. Tests inject registries, agent dirs and sockets.
use super::{agent_patch::resolve_agent_paths, agent_stores::is_valid_agent_name};
use crate::{registry::ProjectRegistry, stores::StoresConfig};
use axum::{Json, extract::Query, http::StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use trusty_common::search_rpc;
mod skills;
pub(super) use skills::{
    get_project_skills, get_user_skills, patch_project_skills, patch_user_skills,
};

type ApiError = (StatusCode, Json<Value>);
fn failure(status: StatusCode, message: impl std::fmt::Display) -> ApiError {
    (status, Json(json!({"error":message.to_string()})))
}
fn internal(e: impl std::fmt::Display) -> ApiError {
    failure(StatusCode::INTERNAL_SERVER_ERROR, e)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProjectQuery {
    path: String,
    agent: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProjectPath {
    path: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ImportRequest {
    path: String,
    agent: String,
}

async fn registered_path(registry: &ProjectRegistry, raw: &str) -> Result<PathBuf, ApiError> {
    let original = Path::new(raw);
    if !original.is_absolute() {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "Project path must be absolute",
        ));
    }
    let root = tokio::fs::canonicalize(original)
        .await
        .map_err(|e| failure(StatusCode::BAD_REQUEST, e))?;
    if !root.is_dir() {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "Project must be an existing directory",
        ));
    }
    let entries = registry.load().await.map_err(internal)?;
    if !entries.values().any(|entry| {
        entry
            .path
            .canonicalize()
            .is_ok_and(|registered| registered == root)
    }) {
        return Err(failure(
            StatusCode::NOT_FOUND,
            "Register this project folder before configuring it",
        ));
    }
    Ok(root)
}
#[derive(Debug)]
struct Destination {
    tree: String,
    root: PathBuf,
    index: String,
}
fn destination(dirs: &[PathBuf], knowledge: &Path, name: &str) -> Result<Destination, ApiError> {
    if !is_valid_agent_name(name) {
        return Err(failure(StatusCode::BAD_REQUEST, "Invalid assistant name"));
    }
    let (path, _) = resolve_agent_paths(dirs, name)
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "Unknown assistant"))?;
    #[derive(Deserialize)]
    struct Partial {
        #[serde(default)]
        stores: StoresConfig,
    }
    let config: Partial =
        toml::from_str(&std::fs::read_to_string(path).map_err(internal)?).map_err(internal)?;
    let binding = config.stores.primary().ok_or_else(|| {
        failure(
            StatusCode::CONFLICT,
            "This assistant has no bound knowledge store",
        )
    })?;
    if let Some(reason) = binding.validate() {
        return Err(failure(StatusCode::CONFLICT, reason));
    }
    let tree = binding.resolved_tree(name);
    let root = if binding.root.is_some() {
        let home = crate::assistants::AssistantHome::for_instance(name)
            .map_err(|e| failure(StatusCode::CONFLICT, e))?;
        let root = home
            .store_root(binding)
            .map_err(|e| failure(StatusCode::CONFLICT, e))?;
        trusty_kb::roots::assert_within(home.path(), &root)
            .map_err(|e| failure(StatusCode::CONFLICT, e))?;
        root
    } else {
        crate::stores::okg_tree_path(knowledge, &tree).ok_or_else(|| {
            failure(
                StatusCode::CONFLICT,
                "The bound store is not a local OKG tree",
            )
        })?
    };
    // The URI path is confined by Roots; an explicit AssistantHome root is
    // confined by store_root above. No client-supplied destination is accepted.
    if binding.root.is_none() {
        trusty_kb::roots::assert_within(knowledge, &root)
            .map_err(|e| failure(StatusCode::CONFLICT, e))?;
    }
    Ok(Destination {
        tree,
        root,
        index: binding.resolved_index().to_owned(),
    })
}
fn matching_index(list: &Value, root: &Path) -> Option<String> {
    list.get("indexes")
        .and_then(Value::as_array)
        .or_else(|| list.as_array())?
        .iter()
        .find_map(|item| {
            let registered = item.get("root_path")?.as_str()?;
            let canonical = std::fs::canonicalize(registered).ok()?;
            (canonical == root)
                .then(|| {
                    item.get("id")
                        .or_else(|| item.get("index_id"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .flatten()
        })
}
async fn search(socket: &Path, method: &str, params: Value) -> Result<Value, ApiError> {
    search_rpc::call_at(socket, method, params, Duration::from_secs(15))
        .await
        .map_err(|e| failure(StatusCode::SERVICE_UNAVAILABLE, e))
}
async fn index_status(socket: &Path, root: &Path) -> Value {
    let list = match search(
        socket,
        search_rpc::METHOD_INDEXES_LIST,
        json!({"details":true}),
    )
    .await
    {
        Ok(v) => v,
        Err((_, Json(v))) => {
            return json!({"connected":false,"id":null,"status":null,"reason":v["error"]});
        }
    };
    let Some(id) = matching_index(&list, root) else {
        return json!({"connected":true,"id":null,"status":null,"reason":"This project has no registered search index"});
    };
    match search(
        socket,
        search_rpc::METHOD_INDEX_STATUS,
        json!({"index_id":id}),
    )
    .await
    {
        Ok(status) => json!({"connected":true,"id":id,"status":status}),
        Err((_, Json(e))) => json!({"connected":false,"id":id,"status":null,"reason":e["error"]}),
    }
}
pub(super) async fn get_project_tools(
    Query(query): Query<ProjectQuery>,
) -> Result<Json<Value>, ApiError> {
    let root = registered_path(&ProjectRegistry::new().map_err(internal)?, &query.path).await?;
    let knowledge = match query.agent.as_deref() {
        Some(agent) => match destination(
            &crate::agents::agents_dir_candidates(),
            &crate::tools::okg::knowledge_dir(),
            agent,
        ) {
            Ok(value) => {
                json!({"available":true,"agent":agent,"tree":value.tree,"index":value.index})
            }
            Err((_, Json(e))) => {
                json!({"available":false,"agent":agent,"tree":null,"index":null,"reason":e["error"]})
            }
        },
        None => {
            json!({"available":false,"agent":null,"tree":null,"index":null,"reason":"Select an assistant to import project contents"})
        }
    };
    let index = match search_rpc::search_socket() {
        Ok(socket) => index_status(&socket, &root).await,
        Err(e) => json!({"connected":false,"id":null,"status":null,"reason":e.to_string()}),
    };
    Ok(Json(
        json!({"path":root,"index":index,"knowledge":knowledge}),
    ))
}
async fn start_index(socket: &Path, root: &Path) -> Result<Value, ApiError> {
    let list = search(
        socket,
        search_rpc::METHOD_INDEXES_LIST,
        json!({"details":true}),
    )
    .await?;
    let id = match matching_index(&list, root) {
        Some(id) => id,
        None => {
            let id = trusty_common::derive_checkout_index_id(root).ok_or_else(|| {
                failure(
                    StatusCode::BAD_REQUEST,
                    "Choose a project directory below the filesystem root",
                )
            })?;
            search(
                socket,
                search_rpc::METHOD_INDEX_CREATE,
                json!({"id":id,"root_path":root,"follow_links":false}),
            )
            .await?;
            // Creation may find an existing id. Re-read the authoritative map
            // and refuse to trigger an unrelated index on a mismatched reply.
            let list = search(
                socket,
                search_rpc::METHOD_INDEXES_LIST,
                json!({"details":true}),
            )
            .await?;
            matching_index(&list, root).ok_or_else(|| {
                failure(
                    StatusCode::CONFLICT,
                    "Search did not register this project root",
                )
            })?
        }
    };
    let result = search(
        socket,
        search_rpc::METHOD_INDEX_REINDEX,
        json!({"index_id":id}),
    )
    .await?;
    Ok(
        json!({"path":root,"index_id":id,"status":result.get("status").and_then(Value::as_str).unwrap_or("started"),"result":result}),
    )
}
pub(super) async fn index_project(Json(req): Json<ProjectPath>) -> Result<Json<Value>, ApiError> {
    let root = registered_path(&ProjectRegistry::new().map_err(internal)?, &req.path).await?;
    let socket =
        search_rpc::search_socket().map_err(|e| failure(StatusCode::SERVICE_UNAVAILABLE, e))?;
    Ok(Json(start_index(&socket, &root).await?))
}
fn import_args(root: &Path, agent: &str, destination: &Destination) -> Result<Value, ApiError> {
    let id = trusty_common::derive_checkout_index_id(root)
        .ok_or_else(|| failure(StatusCode::BAD_REQUEST, "Invalid project root"))?;
    let mut extensions = trusty_kb::okg::docstore::DEFAULT_EXTENSIONS
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    extensions.extend(
        [
            "rs", "py", "js", "jsx", "ts", "tsx", "go", "java", "kt", "swift", "c", "h", "cpp",
            "cs", "rb", "php", "ex", "exs", "sql", "sh", "css", "scss", "svelte", "vue",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    Ok(
        json!({"agent":agent,"root":destination.root,"source_id":format!("project-{id}"),"path":root,"extensions":extensions,"recursive":true,"tombstone_deleted":false}),
    )
}
static IMPORT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
pub(super) async fn import_project(
    Json(req): Json<ImportRequest>,
) -> Result<Json<Value>, ApiError> {
    let root = registered_path(&ProjectRegistry::new().map_err(internal)?, &req.path).await?;
    let destination = destination(
        &crate::agents::agents_dir_candidates(),
        &crate::tools::okg::knowledge_dir(),
        &req.agent,
    )?;
    // Keep the existing operator read allow-list; registration alone never
    // bypasses hidden-directory/source-policy restrictions.
    crate::tools::okg::docstore_policy()
        .permit(&root)
        .map_err(|e| failure(StatusCode::FORBIDDEN, e))?;
    let _guard = IMPORT_LOCK
        .try_lock()
        .map_err(|_| failure(StatusCode::CONFLICT, "Another project import is running"))?;
    let args = import_args(&root, &req.agent, &destination)?;
    let store = trusty_kb::store::KbStore::new(
        destination.root.clone(),
        trusty_kb::schema::Profile::default_profile(),
    );
    let result =
        crate::tools::okg::ingest_into_store(&args, store, &crate::tools::okg::docstore_policy())
            .await
            .map_err(|e| failure(StatusCode::UNPROCESSABLE_ENTITY, e))?;
    if result.is_error() {
        return Err(failure(StatusCode::UNPROCESSABLE_ENTITY, result.content()));
    }
    let result: Value = serde_json::from_str(result.content()).map_err(internal)?;
    Ok(Json(
        json!({"path":root,"agent":req.agent,"tree":destination.tree,"result":result}),
    ))
}
#[cfg(test)]
mod tests;
