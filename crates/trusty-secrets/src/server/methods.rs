//! The `secrets.*` method bodies, one synchronous function each.
//!
//! Why: every method touches blocking resources — `git`, the index lock, the
//! keychain — so the bodies are plain functions the router runs on the
//! blocking pool. Params arrive as raw JSON and are decoded here, so a decode
//! failure becomes [`ErrorKind::InvalidParams`] and the serde message, which
//! can quote a `set` value, is dropped unread.
//! What: params are one flat object: `project` (an absolute directory path;
//! optional for `doctor`) beside the S1 request fields, e.g.
//! `{"project": "/repo", "vault": "trusty/o/r", "key": "K", "value": "…"}`.
//! No function returns a value: `set` returns S1's masked confirmation,
//! `list` names and metadata, `copy` names, `doctor` ids and paths.
//! Test: `server_tests.rs` beside this module.

use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::errors::ErrorKind;
use super::project::ProjectContext;
use super::router::State;
use crate::api::methods::{
    CopyRequest, CopyResponse, DeleteRequest, ListRequest, ListResponse, SetRequest,
};
use crate::api::{BackendId, SecretKey, SecretsError};
use crate::store::{Capabilities, SecretStore};

/// `secrets.doctor` — not among S1's method names.
pub const DOCTOR: &str = "secrets.doctor";

/// The params field naming the project directory.
pub const PROJECT_FIELD: &str = "project";

/// A method body: state and raw params in, a JSON result or a fixed kind out.
// #9073: crate-private; S8 passes the caller pid into the body.
pub(crate) type MethodFn = fn(&State, Value) -> Result<Value, ErrorKind>;

/// Split `params` into the project directory and the remaining fields.
///
/// What: `params` must be an object whose `project` is a string; anything
/// else is [`ErrorKind::InvalidParams`]. The rest is returned untouched.
fn split_project(params: Value) -> Result<(PathBuf, Map<String, Value>), ErrorKind> {
    let Value::Object(mut fields) = params else {
        return Err(ErrorKind::InvalidParams);
    };
    match fields.remove(PROJECT_FIELD) {
        Some(Value::String(dir)) => Ok((PathBuf::from(dir), fields)),
        _ => Err(ErrorKind::InvalidParams),
    }
}

/// Decode the non-project fields as `T`, discarding the serde error.
///
/// Why: a serde message can quote the rejected input — for `set`, the value.
fn decode<T: DeserializeOwned>(fields: Map<String, Value>) -> Result<T, ErrorKind> {
    serde_json::from_value(Value::Object(fields)).map_err(|_| ErrorKind::InvalidParams)
}

fn to_json<T: Serialize>(response: &T) -> Result<Value, ErrorKind> {
    serde_json::to_value(response).map_err(|_| ErrorKind::Internal)
}

/// `secrets.scopes`: the project scope, then the owner scope.
///
/// Test: `server_scopes_round_trip_over_a_real_socket`.
pub(crate) fn scopes(_state: &State, params: Value) -> Result<Value, ErrorKind> {
    let (dir, rest) = split_project(params)?;
    if !rest.is_empty() {
        return Err(ErrorKind::InvalidParams);
    }
    let project = ProjectContext::resolve(&dir)?;
    to_json(&project.scopes().to_response())
}

/// `secrets.list`: names, lengths, `updated_at`, and the agents flag.
///
/// What: reads the names-only index only; never opens a backend.
/// Test: `server_set_list_delete_round_trip_over_a_real_socket`,
/// `server_corrupt_index_is_a_fixed_error`.
pub(crate) fn list(state: &State, params: Value) -> Result<Value, ErrorKind> {
    let (dir, rest) = split_project(params)?;
    let request: ListRequest = decode(rest)?;
    let project = ProjectContext::resolve(&dir)?;
    project.require_in_scope(&request.vault)?;
    let keys = state.index.list(&request.vault)?;
    to_json(&ListResponse {
        vault: request.vault,
        keys,
    })
}

/// `secrets.set`: upsert one key; answer S1's masked confirmation once.
///
/// What: the value goes to the project's backend through [`SecretStore`]
/// and is dropped with the request. Neither the request nor the response is
/// logged or formatted here.
/// Test: `server_set_list_delete_round_trip_over_a_real_socket`,
/// `server_malformed_set_never_echoes_its_value`.
pub(crate) fn set(state: &State, params: Value) -> Result<Value, ErrorKind> {
    let (dir, rest) = split_project(params)?;
    let request: SetRequest = decode(rest)?;
    let project = ProjectContext::resolve(&dir)?;
    project.require_in_scope(&request.vault)?;
    let store = SecretStore::new(project.backend(state)?, state.index.clone());
    let response = store.set(&request.vault, &request.key, &request.value)?;
    to_json(&response)
}

/// `secrets.delete`: remove one key from the backend and the index.
///
/// Test: `server_set_list_delete_round_trip_over_a_real_socket`.
pub(crate) fn delete(state: &State, params: Value) -> Result<Value, ErrorKind> {
    let (dir, rest) = split_project(params)?;
    let request: DeleteRequest = decode(rest)?;
    let project = ProjectContext::resolve(&dir)?;
    project.require_in_scope(&request.vault)?;
    let store = SecretStore::new(project.backend(state)?, state.index.clone());
    to_json(&store.delete(&request.vault, &request.key)?)
}

/// Which keys a `copy` moves: every indexed key, or the ones named.
///
/// Why: keeps the request's "empty means all" rule in one place, so a change
/// to `CopyRequest`'s shape is a change to [`CopySelection::of`] only.
enum CopySelection {
    All,
    Keys(Vec<SecretKey>),
}

impl CopySelection {
    fn of(request: &CopyRequest) -> Self {
        if request.keys.is_empty() {
            Self::All
        } else {
            Self::Keys(request.keys.clone())
        }
    }
}

/// `secrets.copy`: copy keys between two backends inside one project.
///
/// Why: DOC-74 §13 Q6 — copy never crosses projects. The request names no
/// vault; the vault is always this project's own project vault. #9065: the
/// destination write and its index row must not drift apart, so each key
/// goes through [`SecretStore::set`] rather than a bare backend write.
/// What: refuses `from == to` ([`ErrorKind::SameBackend`]), a source without
/// `READ` or a destination without `WRITE` ([`ErrorKind::Unsupported`]),
/// before any key moves. Each key is read from the source and written with
/// [`SecretStore::set`] on the destination, which takes the index lock,
/// upserts the row and, when the publish fails, deletes a new entry again.
/// A key the source lacks, or any other per-key failure, lands in `failed`
/// and the copy continues. An entry that compensation could not delete
/// aborts the copy with [`ErrorKind::OrphanedBackendEntry`] and no copied
/// list; keys copied before it stay visible through `secrets.list`. Values
/// are never returned.
/// Test: `server_copy_moves_keys_between_backends_in_one_project`,
/// `server_copy_refuses_the_same_backend_twice`,
/// `server_copy_compensates_a_key_whose_index_publish_fails`,
/// `server_copy_aborts_with_orphaned_backend_entry_when_compensation_fails`.
pub(crate) fn copy(state: &State, params: Value) -> Result<Value, ErrorKind> {
    let (dir, rest) = split_project(params)?;
    let request: CopyRequest = decode(rest)?;
    if request.from_backend == request.to_backend {
        return Err(ErrorKind::SameBackend);
    }
    let project = ProjectContext::resolve(&dir)?;
    let vault = project.scopes().project().clone();
    let source = (state.backends)(&request.from_backend)?;
    let destination = (state.backends)(&request.to_backend)?;
    if !source.capabilities().contains(Capabilities::READ)
        || !destination.capabilities().contains(Capabilities::WRITE)
    {
        return Err(ErrorKind::Unsupported);
    }
    let keys = match CopySelection::of(&request) {
        CopySelection::Keys(keys) => keys,
        CopySelection::All => state
            .index
            .list(&vault)?
            .into_iter()
            .map(|meta| meta.name)
            .collect(),
    };
    // #9065: write through `set`'s index lock and compensation, never around it.
    let store = SecretStore::new(destination, state.index.clone());
    let mut response = CopyResponse {
        copied: Vec::new(),
        failed: Vec::new(),
    };
    for key in keys {
        let Ok(Some(value)) = source.get(&vault, &key) else {
            response.failed.push(key);
            continue;
        };
        match store.set(&vault, &key, &value) {
            Ok(_) => response.copied.push(key),
            // #9065: an orphan is never folded into `failed`; it aborts the copy.
            Err(orphan @ SecretsError::OrphanedBackendEntry { .. }) => return Err(orphan.into()),
            Err(_) => response.failed.push(key),
        }
    }
    to_json(&response)
}

/// `secrets.doctor` params: an optional project.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DoctorRequest {
    #[serde(default)]
    project: Option<PathBuf>,
}

/// One backend's row in the doctor table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct BackendStatus {
    /// The backend id.
    pub id: BackendId,
    /// Whether this build can open it.
    pub available: bool,
    /// Its capability flags, by name.
    pub capabilities: Vec<String>,
}

/// `secrets.doctor` response: backend availability and paths only.
// #9073: §7 `detect_backends` grows the doctor table, so callers read it only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DoctorResponse {
    /// The socket this server answers on.
    pub socket: PathBuf,
    /// The names-only index directory.
    pub index_root: PathBuf,
    /// The machine config file.
    pub machine_config: PathBuf,
    /// The project's checkout root, when a project was named.
    pub project_root: Option<PathBuf>,
    /// The project config file, when a project was named.
    pub project_config: Option<PathBuf>,
    /// The backend the §6.1 precedence selects.
    pub selected_backend: BackendId,
    /// Every backend this build knows, plus the selected one.
    pub backends: Vec<BackendStatus>,
}

/// `secrets.doctor`: which backends this build can open, and where it looks.
///
/// What: S2 has no `detect_backends` yet (§7 lands with the CLI-backed
/// integrations), so "available" means "this build opens it". Opening a
/// backend reads no secret. Reports paths and ids only.
/// Test: `server_doctor_reports_backends_and_paths_only`.
pub(crate) fn doctor(state: &State, params: Value) -> Result<Value, ErrorKind> {
    let request: DoctorRequest = match params {
        Value::Null => DoctorRequest::default(),
        other => serde_json::from_value(other).map_err(|_| ErrorKind::InvalidParams)?,
    };
    let project = request
        .project
        .as_deref()
        .map(ProjectContext::resolve)
        .transpose()?;
    let selected = match &project {
        Some(project) => project.resolved_config(state)?.backend,
        None => {
            let machine = crate::store::config::load_machine_at(&state.settings.machine_config)?;
            crate::store::config::resolve(None, machine.as_ref()).backend
        }
    };
    let mut ids = vec![BackendId::keychain()];
    if !ids.contains(&selected) {
        ids.push(selected.clone());
    }
    let backends = ids
        .into_iter()
        .map(|id| {
            let opened = (state.backends)(&id).ok();
            BackendStatus {
                available: opened.is_some(),
                capabilities: opened
                    .map(|b| capability_names(b.capabilities()))
                    .unwrap_or_default(),
                id,
            }
        })
        .collect();
    to_json(&DoctorResponse {
        socket: state.settings.socket.clone(),
        index_root: state.settings.index_root.clone(),
        machine_config: state.settings.machine_config.clone(),
        project_root: project.as_ref().map(|p| p.root().to_path_buf()),
        project_config: project.as_ref().map(ProjectContext::config_path),
        selected_backend: selected,
        backends,
    })
}

fn capability_names(caps: Capabilities) -> Vec<String> {
    [
        (Capabilities::READ, "READ"),
        (Capabilities::WRITE, "WRITE"),
        (Capabilities::LIST_NAMES, "LIST_NAMES"),
        (Capabilities::SYNC_TARGET, "SYNC_TARGET"),
    ]
    .into_iter()
    .filter(|(flag, _)| caps.contains(*flag))
    .map(|(_, name)| name.to_string())
    .collect()
}
