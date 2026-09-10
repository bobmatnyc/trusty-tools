//! One-time repair of shipped recall-only assistant grants (#7360).
use anyhow::Result;
use std::path::PathBuf;

/// Interactive chat compatibility repair; source-driven turns cannot mutate Settings.
pub async fn migrate_chat(name: &str, input: &str) -> Result<()> {
    if crate::listeners::wake::LISTENER_CHAT_EVENT
        .try_with(|_| ())
        .is_ok()
        || input.starts_with(crate::listeners::wake::ASK_FIRST_PREAMBLE)
    {
        return Ok(());
    }
    migrate(&crate::agents::agents_dir_candidates(), name).await
}

/// Why: legacy configs grant the write scope but omit the durable tool names.
/// What: persist the repair once; any Settings revision or skill restriction prevents expansion.
/// Test: `migration_preserves_later_tool_revocation`.
pub async fn migrate(dirs: &[PathBuf], name: &str) -> Result<()> {
    let Some((manifest, _)) = crate::api::server::agent_patch::resolve_agent_paths(dirs, name)
    else {
        return Ok(());
    };
    let _guard = crate::knowledge::execution::mutation_guard(&manifest).await?;
    let raw = tokio::fs::read_to_string(&manifest).await?;
    let mut doc = raw.parse::<toml_edit::DocumentMut>()?;
    if doc.get("settings_revision").is_some() {
        return Ok(());
    }
    if !doc
        .get("tools")
        .and_then(|t| t.get("allow"))
        .and_then(toml_edit::Item::as_array)
        .is_some_and(|a| a.iter().any(|p| p.as_str() == Some("memory_recall")))
    {
        return Ok(());
    }
    let config = crate::agents::AgentConfig::by_name_in(dirs, name)?;
    if !super::is_assistant_role(&config.agent.role)
        || config.skills.allow.is_some()
        || !crate::tools::assistant_memory::scope_granted(&config, "memory.write")
    {
        return Ok(());
    }
    let Some(allow) = doc
        .get_mut("tools")
        .and_then(|t| t.get_mut("allow"))
        .and_then(toml_edit::Item::as_array_mut)
    else {
        return Ok(());
    };
    if !allow.iter().any(|p| p.as_str() == Some("memory_recall")) {
        return Ok(());
    }
    for name in ["memory_remember", "memory_write"] {
        if !allow.iter().any(|p| p.as_str() == Some(name)) {
            allow.push(name);
        }
    }
    doc["settings_revision"] = toml_edit::value(uuid::Uuid::new_v4().to_string());
    let path = manifest.canonicalize()?;
    crate::knowledge::persistence::write_bytes(&path, doc.to_string().as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn migration_preserves_later_tool_revocation() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("fixture.toml");
        let raw = "[agent]\nname='fixture'\nrole='assistant'\nmodel='fixture'\ndescription='test'\n[llm]\nmax_tokens=100\ntemperature=0.0\n[system_prompt]\ncontent='test'\n[tools]\nallow=['memory_recall']\nscopes=['memory.*']\n";
        tokio::fs::write(&path, raw).await.unwrap();
        let dirs = [tmp.path().to_path_buf()];
        migrate(&dirs, "fixture").await.unwrap();
        let mut doc = std::fs::read_to_string(&path)
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert!(
            doc["tools"]["allow"]
                .as_array()
                .unwrap()
                .iter()
                .any(|x| x.as_str() == Some("memory_remember"))
        );
        doc["tools"]["allow"] = toml_edit::value(toml_edit::Array::from_iter(["memory_recall"]));
        std::fs::write(&path, doc.to_string()).unwrap();
        migrate(&dirs, "fixture").await.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), doc.to_string());
        std::fs::write(&path, format!("{raw}\n[skills]\nallow=[]\n")).unwrap();
        migrate(&dirs, "fixture").await.unwrap();
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("memory_remember")
        );
    }
}
