//! Revision-aware listener bindings shared by the operator API and self-only tool.
use super::{agent_patch::resolve_agent_paths, agent_stores::is_valid_agent_name};
use crate::listeners::config::{AgentListenerBinding, ListenerConfig};
use axum::{Json, extract::Path as AxumPath, http::StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
pub(crate) type ConfigError = (StatusCode, Json<Value>);
fn error(status: StatusCode, message: impl std::fmt::Display) -> ConfigError {
    (status, Json(json!({"error":message.to_string()})))
}
fn internal(message: impl std::fmt::Display) -> ConfigError {
    error(StatusCode::INTERNAL_SERVER_ERROR, message)
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListenerUpdate {
    pub revision: String,
    pub listeners: Vec<AgentListenerBinding>,
}
fn revision(raw: &str) -> String {
    format!("{:x}", Sha256::digest(raw.as_bytes()))
}
fn parse(raw: &str) -> Result<Vec<AgentListenerBinding>, ConfigError> {
    #[derive(Deserialize)]
    struct Config {
        #[serde(default)]
        listeners: Vec<AgentListenerBinding>,
    }
    toml::from_str::<Config>(raw)
        .map(|c| c.listeners)
        .map_err(|e| error(StatusCode::UNPROCESSABLE_ENTITY, e))
}
fn manifest(dirs: &[PathBuf], name: &str) -> Result<PathBuf, ConfigError> {
    if !is_valid_agent_name(name) {
        return Err(error(StatusCode::BAD_REQUEST, "Invalid assistant name"));
    }
    resolve_agent_paths(dirs, name)
        .map(|(path, _)| path)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "Assistant manifest not found"))
}
fn inherited(dirs: &[PathBuf], raw: &str) -> Result<Vec<AgentListenerBinding>, ConfigError> {
    let config: toml::Value = toml::from_str(raw).map_err(internal)?;
    let Some(parent) = config
        .get("agent")
        .and_then(|a| a.get("extends"))
        .and_then(toml::Value::as_str)
    else {
        return Ok(vec![]);
    };
    if !is_valid_agent_name(parent) {
        return Err(error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid inherited assistant name",
        ));
    }
    crate::agents::AgentConfig::by_name_in(dirs, parent)
        .map(|cfg| cfg.listeners)
        .map_err(|e| error(StatusCode::UNPROCESSABLE_ENTITY, e))
}
fn effective(dirs: &[PathBuf], raw: &str) -> Result<Vec<AgentListenerBinding>, ConfigError> {
    let mut bindings = inherited(dirs, raw)?;
    for binding in parse(raw)? {
        if let Some(old) = bindings.iter_mut().find(|old| old.name == binding.name) {
            *old = binding;
        } else {
            bindings.push(binding);
        }
    }
    Ok(bindings)
}
fn config_revision(raw: &str, listeners: &[AgentListenerBinding]) -> String {
    revision(&format!(
        "{raw}\n{}",
        serde_json::to_string(listeners).unwrap_or_default()
    ))
}
fn response(
    dirs: &[PathBuf],
    name: &str,
    raw: &str,
    available: &[ListenerConfig],
) -> Result<Value, ConfigError> {
    let listeners = effective(dirs, raw)?;
    Ok(
        json!({"agent":name,"revision":config_revision(raw,&listeners),"listeners":listeners,"inherited_names":inherited(dirs,raw)?.iter().map(|b|b.name.clone()).collect::<Vec<_>>(),"available_listeners":available.iter().map(|v|json!({"name":v.name,"connector":v.connector,"identity":v.identity,"enabled":v.enabled})).collect::<Vec<_>>() }),
    )
}
pub(crate) async fn read_at(
    dirs: &[PathBuf],
    name: &str,
    available: &[ListenerConfig],
) -> Result<Value, ConfigError> {
    let raw = tokio::fs::read_to_string(manifest(dirs, name)?)
        .await
        .map_err(internal)?;
    response(dirs, name, &raw, available)
}
pub(crate) async fn write_at(
    dirs: &[PathBuf],
    name: &str,
    available: &[ListenerConfig],
    mut update: ListenerUpdate,
) -> Result<Value, ConfigError> {
    let _lock = super::AGENT_CONFIG_WRITE_LOCK.lock().await;
    let path = manifest(dirs, name)?;
    let raw = tokio::fs::read_to_string(&path).await.map_err(internal)?;
    let existing = effective(dirs, &raw)?;
    if config_revision(&raw, &existing) != update.revision {
        return Err(error(
            StatusCode::CONFLICT,
            "Listener settings changed. Reload before saving.",
        ));
    }
    for mut binding in inherited(dirs, &raw)? {
        if !update.listeners.iter().any(|b| b.name == binding.name) {
            binding.enabled = false;
            update.listeners.push(binding);
        }
    }
    if update.listeners.len() > 32 {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "At most 32 listener bindings are supported",
        ));
    }
    let mut names = std::collections::HashSet::new();
    for binding in &update.listeners {
        binding
            .validate()
            .map_err(|e| error(StatusCode::BAD_REQUEST, e))?;
        if !names.insert(&binding.name) {
            return Err(error(
                StatusCode::BAD_REQUEST,
                "Duplicate listener bindings",
            ));
        }
        if !available.iter().any(|v| v.name == binding.name)
            && (binding.enabled || !existing.iter().any(|v| v.name == binding.name))
        {
            return Err(error(
                StatusCode::BAD_REQUEST,
                format!("Listener {} is not configured by the harness", binding.name),
            ));
        }
    }
    #[derive(Serialize)]
    struct Bindings<'a> {
        listeners: &'a [AgentListenerBinding],
    }
    let replacement = toml::to_string(&Bindings {
        listeners: &update.listeners,
    })
    .map_err(internal)?
    .parse::<toml_edit::DocumentMut>()
    .map_err(internal)?;
    let mut document = raw.parse::<toml_edit::DocumentMut>().map_err(internal)?;
    document["listeners"] = replacement["listeners"].clone();
    let updated = document.to_string();
    // Detect concurrent writes by other editors before atomically replacing.
    if tokio::fs::read_to_string(&path).await.map_err(internal)? != raw
        || config_revision(&raw, &effective(dirs, &raw)?) != update.revision
    {
        return Err(error(
            StatusCode::CONFLICT,
            "Assistant configuration changed. Reload before saving.",
        ));
    }
    atomic_write(&path, updated.as_bytes())
        .await
        .map_err(internal)?;
    response(dirs, name, &updated, available)
}
pub(crate) async fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let temp = path.with_extension(format!("listeners-{}-{suffix}.tmp", std::process::id()));
    let result = async {
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            options.mode(0o600);
        }
        let mut file = options.open(&temp).await?;
        file.write_all(bytes).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&temp, path).await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temp).await;
    }
    result
}
pub(crate) async fn read(name: &str) -> Result<Value, ConfigError> {
    let global = crate::mcp::config::GlobalConfig::load().await;
    read_at(
        &crate::agents::agents_dir_candidates(),
        name,
        &global.listeners,
    )
    .await
}
pub(crate) async fn write(name: &str, update: ListenerUpdate) -> Result<Value, ConfigError> {
    let global = crate::mcp::config::GlobalConfig::load().await;
    write_at(
        &crate::agents::agents_dir_candidates(),
        name,
        &global.listeners,
        update,
    )
    .await
}
pub(super) async fn get_listeners(
    AxumPath(name): AxumPath<String>,
) -> Result<Json<Value>, ConfigError> {
    read(&name).await.map(Json)
}
pub(super) async fn put_listeners(
    AxumPath(name): AxumPath<String>,
    Json(update): Json<ListenerUpdate>,
) -> Result<Json<Value>, ConfigError> {
    write(&name, update).await.map(Json)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn listener_and_model_updates_share_manifest_transaction_lock() {
        use super::super::agent_patch::{PatchAgentRequest, patch_agent_at};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.toml");
        tokio::fs::write(&path,"[agent]\nname='fixture'\nrole='assistant'\nrunner='subprocess'\nmodel='openai/gpt-4o-mini'\ndescription='fixture'\n").await.unwrap();
        let dirs = [dir.path().to_path_buf()];
        let available =
            [toml::from_str::<ListenerConfig>("name='mail'\nconnector='gmail'").unwrap()];
        let initial = read_at(&dirs, "fixture", &available).await.unwrap();
        let revision = initial["revision"].as_str().unwrap().to_owned();
        let guard = super::super::AGENT_CONFIG_WRITE_LOCK.lock().await;
        let patch = patch_agent_at(
            &dirs,
            "fixture",
            PatchAgentRequest {
                model_id: Some("openai/gpt-4o".into()),
                ..Default::default()
            },
        );
        tokio::pin!(patch);
        tokio::select! { biased; _=&mut patch=>panic!("model patch bypassed manifest lock"), _=tokio::task::yield_now()=>{} }
        let put = write_at(
            &dirs,
            "fixture",
            &available,
            ListenerUpdate {
                revision,
                listeners: vec![AgentListenerBinding {
                    name: "mail".into(),
                    instructions: "invoice".into(),
                    ..Default::default()
                }],
            },
        );
        tokio::pin!(put);
        tokio::select! { biased; _=&mut put=>panic!("listener update bypassed manifest lock"), _=tokio::task::yield_now()=>{} }
        drop(guard);
        assert_eq!(patch.await.status(), StatusCode::OK);
        assert_eq!(put.await.unwrap_err().0, StatusCode::CONFLICT);
        let fresh = read_at(&dirs, "fixture", &available).await.unwrap();
        write_at(
            &dirs,
            "fixture",
            &available,
            ListenerUpdate {
                revision: fresh["revision"].as_str().unwrap().into(),
                listeners: vec![AgentListenerBinding {
                    name: "mail".into(),
                    instructions: "invoice".into(),
                    ..Default::default()
                }],
            },
        )
        .await
        .unwrap();
        let result = patch_agent_at(
            &dirs,
            "fixture",
            PatchAgentRequest {
                model_id: Some("openai/gpt-4o-mini".into()),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(result.status(), StatusCode::OK);
        let raw = tokio::fs::read_to_string(path).await.unwrap();
        assert!(raw.contains("openai/gpt-4o-mini"));
        assert_eq!(parse(&raw).unwrap()[0].instructions, "invoice");
    }
    #[tokio::test]
    async fn listener_inheritance_is_effective_revisioned_and_removed_with_disabled_override() {
        let dir = tempfile::tempdir().unwrap();
        let base_path = dir.path().join("base.toml");
        let base = "[agent]\nname='base'\nrole='assistant'\nmodel='m'\ndescription='fixture'\n[llm]\ntemperature=0.0\nmax_tokens=1024\n[system_prompt]\ncontent='fixture'\n[[listeners]]\nname='mail'\ninstructions='original'\n";
        tokio::fs::write(&base_path, base).await.unwrap();
        tokio::fs::write(
            dir.path().join("child.toml"),
            "[agent]\nname='child'\nextends='base'\n",
        )
        .await
        .unwrap();
        let dirs = [dir.path().to_path_buf()];
        let available =
            [toml::from_str::<ListenerConfig>("name='mail'\nconnector='gmail'").unwrap()];
        let initial = read_at(&dirs, "child", &available).await.unwrap();
        assert_eq!(initial["inherited_names"], json!(["mail"]));
        assert_eq!(initial["listeners"][0]["instructions"], "original");
        tokio::fs::write(&base_path, base.replace("original", "changed"))
            .await
            .unwrap();
        let stale = write_at(
            &dirs,
            "child",
            &available,
            ListenerUpdate {
                revision: initial["revision"].as_str().unwrap().into(),
                listeners: vec![],
            },
        )
        .await
        .unwrap_err();
        assert_eq!(stale.0, StatusCode::CONFLICT);
        let current = read_at(&dirs, "child", &available).await.unwrap();
        let removed = write_at(
            &dirs,
            "child",
            &available,
            ListenerUpdate {
                revision: current["revision"].as_str().unwrap().into(),
                listeners: vec![],
            },
        )
        .await
        .unwrap();
        assert_eq!(removed["listeners"][0]["enabled"], false);
        let raw = tokio::fs::read_to_string(dir.path().join("child.toml"))
            .await
            .unwrap();
        assert!(!parse(&raw).unwrap()[0].enabled);
        assert!(!effective(&dirs, &raw).unwrap()[0].enabled);
        assert_eq!(
            read_at(&dirs, "child", &available).await.unwrap()["listeners"],
            removed["listeners"]
        );
    }
    #[tokio::test]
    async fn listener_revision_prevents_lost_updates_and_preserves_other_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.toml");
        tokio::fs::write(
            &path,
            "# keep this comment\n[agent]\nname='fixture'\nmodel='old'\n",
        )
        .await
        .unwrap();
        let available: ListenerConfig = toml::from_str("name='mail'\nconnector='gmail'").unwrap();
        let dirs = [dir.path().to_path_buf()];
        let available = [available];
        let initial = read_at(&dirs, "fixture", &available).await.unwrap();
        let rev = initial["revision"].as_str().unwrap().to_owned();
        let binding = AgentListenerBinding {
            name: "mail".into(),
            instructions: "Summarize invoices".into(),
            ..Default::default()
        };
        let updated = write_at(
            &dirs,
            "fixture",
            &available,
            ListenerUpdate {
                revision: rev.clone(),
                listeners: vec![binding.clone()],
            },
        )
        .await
        .unwrap();
        assert_eq!(
            updated["listeners"][0]["instructions"],
            "Summarize invoices"
        );
        let text = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(text.contains("keep this comment"));
        assert!(text.contains("model='old'") || text.contains("model = 'old'"));
        assert_eq!(
            write_at(
                &dirs,
                "fixture",
                &available,
                ListenerUpdate {
                    revision: rev,
                    listeners: vec![]
                }
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::CONFLICT
        );
        let current = updated["revision"].as_str().unwrap().to_owned();
        assert_eq!(
            write_at(
                &dirs,
                "fixture",
                &available,
                ListenerUpdate {
                    revision: current,
                    listeners: vec![binding.clone(), binding]
                }
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    #[tokio::test]
    async fn listener_rejects_unknown_sources_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.toml");
        tokio::fs::write(&path, "[agent]\nname='fixture'\n")
            .await
            .unwrap();
        let dirs = [dir.path().to_path_buf()];
        let initial = read_at(&dirs, "fixture", &[]).await.unwrap();
        let req = ListenerUpdate {
            revision: initial["revision"].as_str().unwrap().into(),
            listeners: vec![AgentListenerBinding {
                name: "unknown".into(),
                ..Default::default()
            }],
        };
        assert_eq!(
            write_at(&dirs, "fixture", &[], req).await.unwrap_err().0,
            StatusCode::BAD_REQUEST
        );
        assert!(
            !tokio::fs::read_to_string(&path)
                .await
                .unwrap()
                .contains("listeners")
        );
    }
}
