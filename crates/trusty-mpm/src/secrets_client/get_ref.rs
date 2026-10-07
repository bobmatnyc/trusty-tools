//! `secrets_get_ref`: resolve a key to a reference, on demand (#7522).
//!
//! Why: DOC-74 §10.2 and ruling 34 — the model may learn which `secret://`
//! reference names a key, and whether a value is stored behind it, but never
//! the value. Ruling 30 (§10.1, §15.5) drops the session-start preload, so
//! the lookup runs only when the tool is called.
//! What: [`get_ref`] parses the argument, then calls `secrets.scopes`,
//! `secrets.list` per candidate vault (names-only index; no backend read) and
//! `secrets.doctor` (the selected backend). It answers a [`RefAnswer`], whose
//! fields are names, a flag and a timestamp only. The list row's `length` and
//! "agents may use" flag are dropped.
//! Test: `secrets_get_ref_reports_a_project_key_without_its_value`,
//! `secrets_get_ref_falls_back_to_the_owner_scope`,
//! `secrets_get_ref_reports_an_unknown_key_as_absent`,
//! `secrets_get_ref_fails_without_a_value_when_the_socket_is_unreachable`,
//! `secrets_get_ref_fails_without_a_value_when_the_project_is_unresolved`.

use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use trusty_secrets::api::methods::{ListResponse, ScopeKind, ScopesResponse, method};
use trusty_secrets::server::{DOCTOR, DoctorResponse, OnDemandSecrets};
use trusty_secrets::{BackendId, SecretKey, SecretRef, VaultName};

use super::{describe, with_project};

/// The prefix every error text carries: the tool's own name.
const TOOL: &str = "secrets_get_ref";

/// A `secrets_get_ref` failure. Every variant's text is fixed or built from
/// names; none carries a value.
///
/// Test: the `secrets_get_ref_fails_*` tests.
#[derive(Debug, thiserror::Error)]
pub enum GetRefError {
    /// `project` is not an absolute UTF-8 path.
    #[error("secrets_get_ref: `project` must be an absolute path to the project directory")]
    Project,
    /// `key` is neither a valid key name nor a valid `secret://` reference.
    #[error("secrets_get_ref: {0}")]
    Key(trusty_secrets::SecretsError),
    /// The socket call failed; the text comes from [`describe`].
    #[error("{0}")]
    Client(String),
    /// The server answered with a shape this client does not read.
    #[error("secrets_get_ref: {method}: the answer did not decode")]
    Decode {
        /// The method whose answer did not decode.
        method: &'static str,
    },
}

/// The tool's answer. No field can hold a value, a masked head or a length.
///
/// Why: DOC-74 §4 T-2 and §10.2 — names, references and metadata
/// (`backend`, `imported_at`, `present`) only.
/// What: `reference` is the canonical form of the request (`secret://KEY`,
/// `secret://<owner>/KEY` or `secret://<owner>/<repo>/KEY`). `scope`, `vault`
/// and `imported_at` are `null` when `present` is false. `backend` is the
/// backend the project's config selects; the index records no per-key
/// backend. `imported_at` is the index row's last write, in Unix seconds.
/// Test: `secrets_get_ref_answer_has_exactly_the_documented_fields`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefAnswer {
    /// The requested reference, canonical form.
    pub reference: String,
    /// The key name.
    pub key: SecretKey,
    /// Whether the names-only index lists the key in a candidate vault.
    pub present: bool,
    /// `project` or `owner`: the scope that holds the key.
    pub scope: Option<ScopeKind>,
    /// The vault that holds the key.
    pub vault: Option<VaultName>,
    /// The backend the project's config selects.
    pub backend: BackendId,
    /// The key's last write, Unix seconds.
    pub imported_at: Option<u64>,
}

/// Parse `raw` as a `secret://` reference, or as a bare key (`secret://KEY`).
fn parse_reference(raw: &str) -> Result<SecretRef, GetRefError> {
    if raw.starts_with(trusty_secrets::api::SCHEME) {
        return SecretRef::parse(raw).map_err(GetRefError::Key);
    }
    let key = SecretKey::new(raw).map_err(GetRefError::Key)?;
    Ok(SecretRef::Unscoped { key })
}

/// One `secrets.*` call in `project`, decoded as `T`.
async fn call<T: DeserializeOwned>(
    client: &OnDemandSecrets,
    project: &Path,
    method: &'static str,
    params: Value,
) -> Result<T, GetRefError> {
    let params = with_project(project, params).ok_or(GetRefError::Project)?;
    let answer = client
        .call(method, params)
        .await
        .map_err(|e| GetRefError::Client(describe(TOOL, &e, client.socket())))?;
    serde_json::from_value(answer).map_err(|_| GetRefError::Decode { method })
}

/// Resolve `raw` — a key name or a `secret://` reference — in `project`.
///
/// Why: see the module docs.
/// What: an unscoped reference is looked up in the project vault, then the
/// owner vault (DOC-74 §15.3); a pinned one in its own vault, which the
/// server refuses unless it is one of the project's scopes. A miss is
/// `present: false`, not an error. Logs the key name and the outcome only.
///
/// # Errors
///
/// [`GetRefError`]: a relative `project`, an invalid key or reference, a
/// socket failure (unreachable, spawn refused, server refusal such as an
/// unresolved project), or an undecodable answer.
///
/// Test: see the module docs.
pub async fn get_ref(
    client: &OnDemandSecrets,
    project: &Path,
    raw: &str,
) -> Result<RefAnswer, GetRefError> {
    if !project.is_absolute() || project.to_str().is_none() {
        return Err(GetRefError::Project);
    }
    let reference = parse_reference(raw)?;
    let scopes: ScopesResponse = call(client, project, method::SCOPES, Value::Null).await?;
    let candidates: Vec<(ScopeKind, VaultName)> = match reference.pinned_vault() {
        // A vault outside the project's scopes is left to `secrets.list`,
        // whose `require_in_scope` refuses it with fixed text.
        Some(vault) => {
            let kind = scopes
                .scopes
                .iter()
                .find(|scope| scope.vault == vault)
                .map_or(ScopeKind::Owner, |scope| scope.kind);
            vec![(kind, vault)]
        }
        None => scopes
            .scopes
            .into_iter()
            .map(|s| (s.kind, s.vault))
            .collect(),
    };
    let mut hit = None;
    for (kind, vault) in candidates {
        let listed: ListResponse =
            call(client, project, method::LIST, json!({ "vault": vault })).await?;
        if let Some(meta) = listed.keys.into_iter().find(|m| &m.name == reference.key()) {
            hit = Some((kind, vault, meta.updated_at));
            break;
        }
    }
    let doctor: DoctorResponse = call(client, project, DOCTOR, Value::Null).await?;
    let (scope, vault, imported_at) = match hit {
        Some((kind, vault, at)) => (Some(kind), Some(vault), Some(at)),
        None => (None, None, None),
    };
    // #7522: names only — DOC-74 §4 T-3 "logs only the reference and the backend".
    tracing::debug!(
        reference = %reference,
        present = scope.is_some(),
        backend = %doctor.selected_backend,
        "secrets_get_ref answered"
    );
    Ok(RefAnswer {
        reference: reference.to_string(),
        key: reference.key().clone(),
        present: scope.is_some(),
        scope,
        vault,
        backend: doctor.selected_backend,
        imported_at,
    })
}
