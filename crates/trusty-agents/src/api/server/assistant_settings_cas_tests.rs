//! Settings revisions remain atomic across two API processes (#3931).
#[tokio::test]
async fn config_cas_is_cross_process() {
    use super::super::agent_patch::{PatchAgentRequest, patch_agent_at};
    if let Ok(raw) = std::env::var("TRUSTY_SETTINGS_CAS_CASE") {
        let root = std::path::PathBuf::from(std::env::var("TRUSTY_SETTINGS_CAS_ROOT").unwrap());
        let expected = std::fs::read_to_string(root.join("revision")).unwrap();
        let req: PatchAgentRequest =
            serde_json::from_value(serde_json::json!({"revision":expected,"tools_allow":[raw]}))
                .unwrap();
        let result = patch_agent_at(&[root.join("agents")], "fixture", req).await;
        std::fs::write(
            root.join(format!("result-{raw}")),
            result.status().as_u16().to_string(),
        )
        .unwrap();
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let dirs = root.join("agents");
    std::fs::create_dir(&dirs).unwrap();
    let raw = "[agent]\nname='fixture'\nrole='assistant'\nmodel='fixture'\ndescription='Synthetic'\n[llm]\nmax_tokens=128\ntemperature=0.0\n[system_prompt]\ncontent='Synthetic'\n[tools]\nallow=[]\n";
    std::fs::write(dirs.join("fixture.toml"), raw).unwrap();
    std::fs::write(root.join("revision"), super::config_revision(raw)).unwrap();
    let mut children = vec![];
    for case in ["memory_recall", "memory_write"] {
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "api::server::assistant_settings::cas_tests::config_cas_is_cross_process",
                "--nocapture",
            ])
            .env("TRUSTY_SETTINGS_CAS_CASE", case)
            .env("TRUSTY_SETTINGS_CAS_ROOT", root)
            .env("HOME", root)
            .env("TAGENT_PROJECT_DIR", root)
            .env("TAGENT_CONFIG_DIR", &dirs)
            .env("TAGENT_ASSISTANTS_DIR", root.join("homes"))
            .env("TRUSTY_DATA_DIR_OVERRIDE", root.join("data"))
            .env("TRUSTY_MEMORY_SOCKET", root.join("missing-memory.sock"))
            .env_remove("OPEN_MPM_CONFIG_DIR")
            .env_remove("OPEN_MPM_PROJECT_DIR");
        children.push(command.spawn().unwrap());
    }
    for mut child in children {
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(30), child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
    }
    let mut statuses = [
        std::fs::read_to_string(root.join("result-memory_recall")).unwrap(),
        std::fs::read_to_string(root.join("result-memory_write")).unwrap(),
    ];
    statuses.sort();
    assert_eq!(statuses, ["200", "409"]);
}

#[tokio::test]
async fn settings_grant_replacements_survive_extends() {
    use super::super::agent_patch::{PatchAgentRequest, patch_agent_at};
    let tmp = tempfile::tempdir().unwrap();
    let dirs = tmp.path().join("agents");
    std::fs::create_dir(&dirs).unwrap();
    let base = "[agent]\nname='base'\nrole='assistant'\nmodel='fixture'\ndescription='Synthetic'\n[llm]\nmax_tokens=128\ntemperature=0.0\n[system_prompt]\ncontent='Synthetic'\n[tools]\nallow=['memory_write','memory_recall']\nscopes=['memory.write','memory.read']\n[skills]\nallow=['first','second']\n[subagents]\ndelegate_allowed=['research-agent','project-manager']\n";
    std::fs::write(dirs.join("base.toml"), base).unwrap();
    let path = dirs.join("fixture.toml");
    std::fs::write(&path, "[agent]\nname='fixture'\nrole='assistant'\nextends='base'\nmodel='fixture'\ndescription='Synthetic'\n[llm]\nmax_tokens=128\ntemperature=0.0\n[system_prompt]\ncontent='Synthetic'\n").unwrap();
    for empty in [false, true] {
        let tools = if empty { vec![] } else { vec!["memory_recall"] };
        let scopes = if empty { vec![] } else { vec!["memory.read"] };
        let skills = if empty { vec![] } else { vec!["first"] };
        let delegates = if empty {
            vec![]
        } else {
            vec![crate::agents::delegation::ASSISTANT_REACHABLE_SUBAGENTS[0]]
        };
        let raw = std::fs::read_to_string(&path).unwrap();
        let request: PatchAgentRequest = serde_json::from_value(serde_json::json!({"revision":super::config_revision(&raw),"scopes":scopes,"tools_allow":tools,"skills_allow":skills,"subagents_delegate_allowed":delegates})).unwrap();
        assert_eq!(
            patch_agent_at(std::slice::from_ref(&dirs), "fixture", request)
                .await
                .status(),
            axum::http::StatusCode::OK
        );
        // Re-open the persisted files through the production inheritance resolver.
        for _ in 0..2 {
            let config = crate::agents::extends::resolve("fixture", &|name| {
                let raw = std::fs::read_to_string(dirs.join(format!("{name}.toml"))).ok()?;
                toml::from_str(&raw).ok()
            })
            .unwrap();
            assert_eq!(config.tools.allow.unwrap(), tools);
            assert_eq!(config.permissions.scopes.unwrap(), scopes);
            assert_eq!(config.skills.allow.unwrap(), skills);
            assert_eq!(config.subagents.delegate_allowed.unwrap(), delegates);
        }
    }
}
