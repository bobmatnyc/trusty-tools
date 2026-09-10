//! Typed Concierge operations reuse Settings handlers and their validation (#3931/#7361).
use crate::tools::{ToolExecutor, ToolResult};
use axum::{Json, extract::Path, http::StatusCode, response::Response};
use serde::Deserialize;
use serde_json::{Value, json};
type Error = (StatusCode, Json<Value>);
pub(super) fn config_revision(raw: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(raw.as_bytes()))
}
pub(super) fn patch_grants(
    doc: &mut toml_edit::DocumentMut,
    req: &super::agent_patch::PatchAgentRequest,
) -> Result<(), String> {
    for (table, key, entries) in [
        ("permissions", "scopes", &req.scopes),
        ("skills", "allow", &req.skills_allow),
    ] {
        if let Some(entries) = entries {
            if entries.len() > 256
                || entries
                    .iter()
                    .any(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_whitespace))
            {
                return Err(format!(
                    "{table}.{key} must contain at most 256 nonempty grant patterns"
                ));
            }
            let target = doc
                .entry(table)
                .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
                .as_table_like_mut()
                .ok_or_else(|| format!("{table} is not a table"))?;
            let mut array = toml_edit::Array::new();
            for entry in entries {
                array.push(entry.as_str());
            }
            target.insert(key, toml_edit::value(array));
        }
    }
    // #3931: preserve authored union defaults, but persist exact operator selections.
    for (flag, selected) in [
        ("replace_scopes", req.scopes.is_some()),
        ("replace_tools", req.tools_allow.is_some()),
        ("replace_skills", req.skills_allow.is_some()),
        (
            "replace_subagents",
            req.subagents_delegate_allowed.is_some(),
        ),
    ] {
        if selected {
            doc.entry("permissions")
                .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
                .as_table_like_mut()
                .ok_or("permissions is not a table")?
                .insert(flag, toml_edit::value(true));
        }
    }
    Ok(())
}
fn error(message: impl std::fmt::Display) -> Error {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error":message.to_string()})),
    )
}
fn policy_error(e: crate::knowledge::KnowledgeError) -> Error {
    let status = match e {
        crate::knowledge::KnowledgeError::Conflict => StatusCode::CONFLICT,
        crate::knowledge::KnowledgeError::BadRequest(_) => StatusCode::BAD_REQUEST,
        _ => StatusCode::UNPROCESSABLE_ENTITY,
    };
    (status, Json(json!({"error":e.to_string()})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MemoryPatch {
    revision: String,
    cross_palace_query: bool,
}

pub(super) async fn memory_get(Path(name): Path<String>) -> Result<Json<Value>, Error> {
    let value =
        tokio::task::spawn_blocking(move || crate::assistants::memory_policy::resolve(&name))
            .await
            .map_err(error)?
            .map_err(policy_error)?;
    Ok(Json(serde_json::to_value(value).map_err(error)?))
}
pub(super) async fn memory_patch(
    Path(name): Path<String>,
    Json(patch): Json<MemoryPatch>,
) -> Result<Json<Value>, Error> {
    let value = tokio::task::spawn_blocking(move || {
        crate::assistants::memory_policy::patch(&name, &patch.revision, patch.cross_palace_query)
    })
    .await
    .map_err(error)?
    .map_err(policy_error)?;
    Ok(Json(serde_json::to_value(value).map_err(error)?))
}
async fn response_value(response: Response) -> Result<Value, Error> {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .map_err(error)?;
    let value = serde_json::from_slice(&bytes).map_err(error)?;
    if status.is_success() {
        Ok(value)
    } else {
        Err((status, Json(value)))
    }
}

/// Why: platform help must be a callable capability, with the same contracts as Settings.
/// What: route a closed operation/section vocabulary to one persistence domain per mutation.
/// Test: `settings_reject_unknown_sections_before_io`.
pub(crate) async fn operate(
    name: &str,
    action: &str,
    section: &str,
    payload: Value,
) -> Result<Value, Error> {
    if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
        return Err(error("Invalid assistant name"));
    }
    let dirs = crate::agents::agents_dir_candidates();
    match (action, section) {
        ("settings.get", "memory") => Ok(memory_get(Path(name.into())).await?.0),
        ("settings.patch", "memory") => Ok(memory_patch(
            Path(name.into()),
            Json(serde_json::from_value(payload).map_err(error)?),
        )
        .await?
        .0),
        ("settings.get", "config" | "permissions" | "model" | "provider") => {
            let mut value =
                response_value(super::agent_patch::get_agent_at(&dirs, name).await).await?;
            value["permissions_editable"] = json!(true);
            value["permissions_reason"] = json!(
                "Authenticated operator configuration; RBAC and platform delegation ceilings still apply at execution."
            );
            Ok(value)
        }
        ("settings.get", "personality") => {
            response_value(super::agent_patch::persona_at(&dirs, name).await).await
        }
        (
            "settings.patch",
            "config" | "model" | "provider" | "personality" | "subagents" | "permissions"
            | "skills",
        ) => {
            let request: super::agent_patch::PatchAgentRequest =
                serde_json::from_value(payload).map_err(error)?;
            if request.revision.is_none() {
                return Err(error(
                    "Read config and provide its revision before changing settings",
                ));
            }
            response_value(super::agent_patch::patch_agent_at(&dirs, name, request).await).await
        }
        ("settings.get", "listeners") => super::agent_listeners::read(name).await,
        ("settings.patch", "listeners") => {
            super::agent_listeners::write(name, serde_json::from_value(payload).map_err(error)?)
                .await
        }
        ("settings.get", "channels") => super::agent_channels::read(name).await,
        ("settings.patch", "channels") => Ok(super::agent_channels::put_route(
            Path(name.into()),
            Json(serde_json::from_value(payload).map_err(error)?),
        )
        .await?
        .0),
        ("settings.get", "knowledge" | "projects") => {
            Ok(super::knowledge_pipeline::get(Path(name.into())).await?.0)
        }
        ("settings.patch", "projects") => {
            super::knowledge_pipeline::configure_projects(name, payload).await
        }
        ("settings.patch", "knowledge") => Ok(super::knowledge_pipeline::patch(
            Path(name.into()),
            Json(serde_json::from_value(payload).map_err(error)?),
        )
        .await?
        .0),
        ("settings.get", "skills") => {
            response_value(
                super::agent_skills::skills_at(
                    &dirs,
                    name,
                    &std::env::current_dir().map_err(error)?,
                )
                .await,
            )
            .await
        }
        ("settings.get", "subagents") => {
            response_value(
                super::agent_subagents::subagents_at(
                    &dirs,
                    name,
                    &std::env::current_dir().map_err(error)?,
                )
                .await,
            )
            .await
        }
        ("platform.health", _) => {
            match crate::tools::system_status::SystemStatusTool::new(name)
                .execute(json!({}))
                .await
            {
                ToolResult::Success(report) => Ok(json!({"assistant":name,"report":report})),
                ToolResult::Error { message, .. } => Err(error(message)),
            }
        }
        _ => Err(error(
            "Use settings.get/settings.patch with memory, config, model, provider, personality, permissions, skills, subagents, listeners, channels, knowledge or projects; use platform.health for diagnostics. Installed skill moves use delegate_skill_configuration.",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn settings_reject_unknown_sections_before_io() {
        assert!(
            operate(
                "assistant",
                "settings.patch",
                "url",
                json!({"url":"http://example.com"})
            )
            .await
            .is_err()
        );
        assert!(
            operate("../other", "settings.get", "config", json!({}))
                .await
                .is_err()
        );
        assert!(
            operate("assistant", "settings.patch", "permissions", json!({}))
                .await
                .is_err()
        );
        assert!(
            operate(
                "assistant",
                "settings.patch",
                "config",
                json!({"model_id":"x"})
            )
            .await
            .is_err()
        );
    }
}

#[cfg(test)]
#[path = "assistant_settings_cas_tests.rs"]
mod cas_tests;
