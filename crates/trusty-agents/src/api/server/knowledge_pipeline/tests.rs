use super::*;
use crate::registry::{ProjectEntry, ProjectStatus};

fn fixture() -> (tempfile::TempDir, Vec<PathBuf>, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let agents = tmp.path().join("agents");
    std::fs::create_dir(&agents).unwrap();
    std::fs::write(agents.join("assistant.toml"), "[agent]\nname='assistant'\nrole='assistant'\nmodel='fixture'\ndescription='fixture'\n[llm]\nmax_tokens=128\ntemperature=0.0\n[system_prompt]\ncontent='fixture'\n").unwrap();
    std::fs::write(agents.join("twin.toml"), "[agent]\nname='twin'\nrole='assistant'\nextends='assistant'\nmodel='fixture'\ndescription='fixture'\n[llm]\nmax_tokens=128\ntemperature=0.0\n[system_prompt]\ncontent='fixture'\n").unwrap();
    let homes = tmp.path().canonicalize().unwrap().join("homes");
    (tmp, vec![agents], homes)
}
fn entry(path: PathBuf) -> ProjectEntry {
    ProjectEntry {
        path,
        name: "Fixture project".into(),
        last_run: None,
        status: ProjectStatus::Active,
        last_connected: None,
        pm_count: 0,
        is_self: false,
        git_origin: None,
        open_issues_count: None,
        open_prs_count: None,
    }
}
async fn context(dirs: &[PathBuf], root: &std::path::Path, projects: Vec<ProjectEntry>) -> Context {
    Context::at(dirs, root.to_path_buf(), "twin", projects, vec![])
        .await
        .unwrap()
}
async fn initialized(dirs: &[PathBuf], root: &std::path::Path) -> KnowledgeState {
    let ctx = context(dirs, root, vec![]).await;
    let state = ctx.store().initialize(Utc::now(), None).unwrap();
    ctx.bind_new_store(&state).await.unwrap();
    ctx.store().confirm_binding(&state.revision).unwrap()
}

#[tokio::test]
async fn interrupted_binding_setup_recovers_after_manifest_conflict_and_restart() {
    let (_tmp, dirs, root) = fixture();
    let ctx = context(&dirs, &root, vec![]).await;
    let state = ctx.store().initialize(Utc::now(), None).unwrap();
    let raw = format!("{}\n# concurrently edited\n", ctx.raw);
    std::fs::write(&ctx.manifest, raw).unwrap();
    assert!(matches!(
        initialize(
            ctx,
            Reconcile {
                revision: Some(state.revision.clone())
            }
        )
        .await,
        Err((StatusCode::CONFLICT, _))
    ));
    let ctx = context(&dirs, &root, vec![]).await;
    assert!(!ctx.state().await.unwrap().unwrap().binding_confirmed);
    // Also exercise a restart after the manifest write but before confirmation.
    ctx.bind_new_store(&state).await.unwrap();
    let ctx = context(&dirs, &root, vec![]).await;
    let Json(result) = initialize(
        ctx,
        Reconcile {
            revision: Some(state.revision),
        },
    )
    .await
    .unwrap();
    assert_eq!(result["pipeline"]["binding_confirmed"], true);
    assert_eq!(
        result["pipeline"]["store"]["root"],
        state.store.root.to_string_lossy().as_ref()
    );
}

#[tokio::test]
async fn assistant_pipeline_rejects_specialists_without_creating_state() {
    let (_tmp, dirs, root) = fixture();
    std::fs::write(
        dirs[0].join("engineer-assistant.toml"),
        "[agent]\nname='engineer-assistant'\nrole='engineer'\n",
    )
    .unwrap();
    let result = Context::at(&dirs, root.clone(), "engineer-assistant", vec![], vec![]).await;
    assert!(matches!(result, Err((StatusCode::UNPROCESSABLE_ENTITY, _))));
    assert!(!root.exists());
    assert!(
        Context::at(&dirs, root.clone(), "../twin", vec![], vec![])
            .await
            .is_err()
    );
    assert!(!root.exists());
    let ctx = context(&dirs, &root, vec![]).await;
    assert!(ctx.state().await.unwrap().is_none());
    assert!(!root.exists(), "GET must not provision directories");
}

#[tokio::test]
async fn pipeline_projects_are_revisioned_and_registered() {
    let (tmp, dirs, root) = fixture();
    let project = tmp.path().join("project");
    let unregistered = tmp.path().join("other");
    std::fs::create_dir(&project).unwrap();
    std::fs::create_dir(&unregistered).unwrap();
    let state = initialized(&dirs, &root).await;
    let ctx = context(&dirs, &root, vec![entry(project.clone())]).await;
    let invalid = update_projects(
        ctx,
        Projects {
            revision: state.revision.clone(),
            chat_id: "chat-a".into(),
            projects: vec![unregistered.to_string_lossy().into_owned()],
        },
    )
    .await;
    assert!(matches!(invalid, Err((StatusCode::BAD_REQUEST, _))));
    let ctx = context(&dirs, &root, vec![entry(project.clone())]).await;
    let Json(result) = update_projects(
        ctx,
        Projects {
            revision: state.revision.clone(),
            chat_id: "chat-a".into(),
            projects: vec![project.to_string_lossy().into_owned()],
        },
    )
    .await
    .unwrap();
    assert_eq!(result["pipeline"]["sources"].as_array().unwrap().len(), 1);
    assert_eq!(
        result["pipeline"]["jobs"][0]["status"],
        "blocked_on_dependency"
    );
    assert_eq!(result["index"]["connected"], false);
    let ctx = context(&dirs, &root, vec![entry(project)]).await;
    assert!(matches!(
        update_projects(
            ctx,
            Projects {
                revision: state.revision,
                chat_id: "chat-a".into(),
                projects: vec![]
            }
        )
        .await,
        Err((StatusCode::CONFLICT, _))
    ));
}

#[test]
fn registered_selection_rejects_unregistered_and_deduplicates_aliases() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("p");
    std::fs::create_dir(&project).unwrap();
    let registry = catalog::registered(vec![entry(project.clone())]);
    let paths = vec![
        project.to_string_lossy().into_owned(),
        project.join(".").to_string_lossy().into_owned(),
    ];
    assert_eq!(
        catalog::validate_projects(&paths, &registry).unwrap().len(),
        1
    );
    std::fs::remove_dir(project).unwrap();
    assert!(catalog::validate_projects(&paths, &registry).is_err());
}

#[tokio::test]
async fn catalogue_never_promotes_registration_or_disabled_channels_to_sources() {
    let (tmp, dirs, root) = fixture();
    let project = tmp.path().join("p");
    std::fs::create_dir(&project).unwrap();
    let mut ctx = context(&dirs, &root, vec![entry(project)]).await;
    assert!(ctx.sources(&BTreeMap::new()).unwrap().is_empty());
    ctx.listeners.push(
        toml::from_str("name='mail'\nconnector='gmail'\nenabled=true\nidentity='fixture-account'")
            .unwrap(),
    );
    ctx.config
        .listeners
        .push(toml::from_str("name='mail'\nenabled=false").unwrap());
    assert!(ctx.sources(&BTreeMap::new()).unwrap().is_empty());
    ctx.config.listeners[0].enabled = true;
    let before = ctx.sources(&BTreeMap::new()).unwrap();
    assert_eq!(before.len(), 1);
    ctx.listeners[0].identity = Some("changed-account".into());
    let after = ctx.sources(&BTreeMap::new()).unwrap();
    assert_ne!(before[0].revision, after[0].revision);
}

#[tokio::test]
async fn protected_binding_cannot_be_removed_or_retargeted() {
    let (_tmp, dirs, root) = fixture();
    let state = initialized(&dirs, &root).await;
    let mut ctx = context(&dirs, &root, vec![]).await;
    assert!(ctx.current(&state.revision).await.is_ok());
    ctx.config.stores.bindings[0].index = Some("foreign".into());
    assert!(matches!(
        ctx.current(&state.revision).await,
        Err((StatusCode::CONFLICT, _))
    ));
    ctx.config.stores.bindings.clear();
    assert!(matches!(
        ctx.current(&state.revision).await,
        Err((StatusCode::CONFLICT, _))
    ));
}

#[tokio::test]
async fn index_collision_never_reindexes_a_foreign_root() {
    let tmp = tempfile::tempdir().unwrap();
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let saved = calls.clone();
    let daemon = crate::uds_mock::spawn(move |method, _| {
        saved.lock().unwrap().push(method.to_owned());
        Box::pin(async { Ok(json!({"indexes":[{"id":"protected","root_path":"/foreign"}]})) })
    })
    .await;
    let result = indexing::at(
        daemon.socket(),
        &ProtectedStore {
            root: tmp.path().into(),
            index_id: "protected".into(),
            protected: true,
        },
        true,
    )
    .await;
    assert!(result.is_err());
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        &[trusty_common::search_rpc::METHOD_INDEXES_LIST]
    );
}

#[tokio::test]
async fn replay_reloads_retargeted_channels_instead_of_reviving_old_scope() {
    let (_tmp, dirs, root) = fixture();
    let state = initialized(&dirs, &root).await;
    let path = dirs[0].join("twin.channels.json");
    let binding = json!([{"id":"team","name":"Team","provider":"slack","target":"C1234","enabled":true,"receive_enabled":true}]);
    std::fs::write(&path, binding.to_string()).unwrap();
    let old_context = context(&dirs, &root, vec![]).await;
    let old_sources = old_context.sources(&BTreeMap::new()).unwrap();
    let state = old_context
        .store()
        .reconcile(&state.revision, &old_sources, Utc::now())
        .unwrap();
    old_context
        .store()
        .enqueue_event(
            &old_sources[0].id,
            &old_sources[0].revision,
            "message-old",
            Utc::now(),
            Utc::now(),
        )
        .unwrap();
    std::fs::write(&path, binding.to_string().replace("C1234", "C9999")).unwrap();
    let fresh = context(&dirs, &root, vec![]).await;
    let new_sources = fresh.sources(&BTreeMap::new()).unwrap();
    fresh
        .store()
        .reconcile(&state.revision, &new_sources, Utc::now())
        .unwrap();
    intake::replay(&old_context).await.unwrap();
    let state = fresh.state().await.unwrap().unwrap();
    assert_eq!(state.sources, new_sources);
    assert!(
        state
            .jobs
            .iter()
            .filter(|job| job.source_revision == old_sources[0].revision)
            .all(|job| job.status == crate::knowledge::JobStatus::Cancelled)
    );
    assert!(
        state.admitted_events.is_empty(),
        "revoked source must not admit the pending record"
    );
}
