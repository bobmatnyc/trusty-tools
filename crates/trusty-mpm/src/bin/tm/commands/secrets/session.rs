//! One `tm secrets` call's context and its socket calls (#7521).
//!
//! Why: every verb needs the same three things — a client, the project
//! directory, and an error text that cannot carry a value — so they live
//! once here.
//! What: [`Ctx`]; [`Ctx::call`] / [`Ctx::call_raw`] (one `secrets.*` request,
//! the project folded into the params); [`Ctx::project_vault`] (the project
//! scope from `secrets.scopes`); [`describe`] (a `ClientError` as fixed text:
//! the server's own fixed message, a spawn failure, or a transport failure
//! with its detail dropped); [`entry_key`] (`KEY` or `<group>.<KEY>`) and
//! [`check_group`].
//! Test: `tests.rs` — `every_verb_fails_without_a_value_when_the_socket_is_unreachable`,
//! `set_with_a_group_namespaces_the_key`.

use std::path::Path;

use anyhow::{anyhow, bail};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use trusty_secrets::api::methods::{ScopeKind, ScopesResponse, method};
use trusty_secrets::server::{ClientError, ErrorKind, OnDemandSecrets, PROJECT_FIELD};
use trusty_secrets::{SecretKey, VaultName};

use super::ValueSource;

/// Longest group label accepted. A longer argument is more likely a value
/// typed where the group goes than a label.
const MAX_GROUP_LEN: usize = 16;

/// Everything one verb runs against.
pub(crate) struct Ctx<'a> {
    /// The socket client; starts the server when nothing answers.
    pub(crate) client: &'a OnDemandSecrets,
    /// The directory whose checkout names the project.
    pub(crate) project: &'a Path,
    /// The default `set` source.
    pub(crate) clipboard: &'a dyn ValueSource,
    /// The `--value -` source.
    pub(crate) stdin: &'a dyn ValueSource,
}

impl Ctx<'_> {
    /// `params` with `project` added; `params` must be an object or null.
    fn params(&self, params: Value) -> anyhow::Result<Value> {
        let project = self
            .project
            .to_str()
            .ok_or_else(|| anyhow!("tm secrets: the working directory is not UTF-8"))?;
        let mut fields = match params {
            Value::Object(fields) => fields,
            _ => Map::new(),
        };
        fields.insert(PROJECT_FIELD.to_owned(), Value::String(project.to_owned()));
        Ok(Value::Object(fields))
    }

    /// One request with the project folded in; the raw client result.
    pub(crate) async fn call_raw(
        &self,
        method: &str,
        params: Value,
    ) -> anyhow::Result<Result<Value, ClientError>> {
        let params = self.params(params)?;
        Ok(self.client.call(method, params).await)
    }

    /// One request, decoded as `T`; every failure is [`describe`]d.
    pub(crate) async fn call<T: DeserializeOwned>(
        &self,
        method: &'static str,
        params: Value,
    ) -> anyhow::Result<T> {
        let result = self
            .call_raw(method, params)
            .await?
            .map_err(|e| anyhow!(describe(&e, self.client.socket())))?;
        serde_json::from_value(result)
            .map_err(|_| anyhow!("tm secrets: {method}: the answer did not decode"))
    }

    /// The project vault from `secrets.scopes` — the only scope the CLI
    /// writes (#7521, DOC-74 §15.3).
    pub(crate) async fn project_vault(&self) -> anyhow::Result<VaultName> {
        let scopes: ScopesResponse = self.call(method::SCOPES, Value::Null).await?;
        scopes
            .scopes
            .into_iter()
            .find(|scope| scope.kind == ScopeKind::Project)
            .map(|scope| scope.vault)
            .ok_or_else(|| anyhow!("tm secrets: the server reported no project scope"))
    }
}

/// A client failure as text that cannot carry a request field.
///
/// Why: the server already answers with fixed text (`ErrorKind::to_rpc`); a
/// transport error's detail is a library message, so it is dropped.
pub(crate) fn describe(error: &ClientError, socket: &Path) -> String {
    let socket = socket.display();
    match error {
        ClientError::Rpc(e) => format!("tm secrets: {}", e.message),
        ClientError::Spawn(e) => format!("tm secrets: cannot start trusty-secrets: {e}"),
        ClientError::Transport(_) => {
            format!("tm secrets: the request did not cross the trusty-secrets socket {socket}")
        }
        ClientError::HomeUnavailable => "tm secrets: the home directory is unavailable".to_owned(),
        ClientError::EmptyResponse => {
            format!("tm secrets: trusty-secrets at {socket} answered without a result")
        }
        // #7521: `ClientError` is `#[non_exhaustive]`; a later variant gets
        // fixed text, never its own detail.
        _ => format!("tm secrets: trusty-secrets at {socket} failed"),
    }
}

/// The machine-readable kind of a server refusal, e.g. `project_unresolved`.
pub(crate) fn rpc_kind(error: &ClientError) -> Option<&'static str> {
    match error {
        ClientError::Rpc(e) => e.kind.map(ErrorKind::as_str),
        _ => None,
    }
}

/// The stored key for `key` under the optional `group` label.
///
/// Why: DOC-74 §9 — the label namespaces the key, and the same key and group
/// always name the same entry. A group holds no `.`, so `<group>.` is
/// unambiguous.
/// What: `KEY`, or `<group>.<KEY>` with `group` 1–16 of `[A-Za-z0-9_-]`.
/// Neither error quotes the argument, which may be a mistyped value.
/// Test: `set_with_a_group_namespaces_the_key`,
/// `set_refuses_a_value_on_the_command_line_without_echoing_it`.
pub(crate) fn entry_key(key: &str, group: Option<&str>) -> anyhow::Result<SecretKey> {
    check_group(group)?;
    let name = match group {
        None => key.to_owned(),
        Some(group) => format!("{group}.{key}"),
    };
    SecretKey::new(&name).map_err(|e| anyhow!("tm secrets: {e}"))
}

/// Refuse a group label outside 1–16 of `[A-Za-z0-9_-]`, without quoting it.
///
/// Why: #7521 — `import` checks the label once, before any key is sent.
/// Test: `set_with_a_group_namespaces_the_key`,
/// `import_with_an_invalid_group_stores_nothing`.
pub(crate) fn check_group(group: Option<&str>) -> anyhow::Result<()> {
    let Some(group) = group else {
        return Ok(());
    };
    let valid = !group.is_empty()
        && group.len() <= MAX_GROUP_LEN
        && group
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'));
    if !valid {
        bail!("tm secrets: a group is 1 to 16 of [A-Za-z0-9_-]");
    }
    Ok(())
}
