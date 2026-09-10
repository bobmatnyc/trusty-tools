use super::super::tests::{context, entry, fixture};
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

fn output() -> Extraction {
    crate::knowledge::extraction::validate(r#"{"entities":[{"id":"maya","type":"person","name":"Maya","claims":[{"text":"Leads Atlas","evidence_quote":"Maya leads Atlas."}]}],"relationships":[]}"#,"Maya leads Atlas.").unwrap()
}
async fn prepared() -> (
    tempfile::TempDir,
    Context,
    SourceDescriptor,
    SourceItem,
    String,
) {
    let (tmp, dirs, root) = fixture();
    let folder = tmp.path().join("project");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("notes.md"), "Maya leads Atlas.").unwrap();
    let ctx = context(&dirs, &root, vec![entry(folder.clone())]).await;
    let Json(_) = super::super::update_projects(
        ctx,
        Projects {
            revision: String::new(),
            scope: Some("assistant".into()),
            chat_id: String::new(),
            projects: vec![
                folder
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            ],
        },
    )
    .await
    .unwrap();
    let ctx = context(&dirs, &root, vec![entry(folder)]).await;
    let state = ctx.state().await.unwrap().unwrap();
    let source = ctx.sources(&state.project_selections()).unwrap().remove(0);
    let item = inputs(&ctx, &state, &source).await.unwrap().remove(0);
    let key = catalog::digest(format!("{}:{}", source.id, item.item_id));
    (tmp, ctx, source, item, key)
}

#[tokio::test]
async fn extraction_checkpoint_retries_publication_without_repeating_inference() {
    let (_tmp, mut ctx, source, item, key) = prepared().await;
    let root = ctx.state().await.unwrap().unwrap().store.root;
    let index = ctx.state().await.unwrap().unwrap().store.index_id;
    let fail = Arc::new(AtomicBool::new(true));
    let switch = fail.clone();
    let daemon = crate::uds_mock::spawn(move |method, params| {
        let root = root.clone();
        let index = index.clone();
        let fail = fail.clone();
        let method = method.to_owned();
        Box::pin(async move {
            if method == trusty_common::search_rpc::METHOD_INDEXES_LIST {
                return Ok(json!({"indexes":[{"id":index,"root_path":root}]}));
            }
            if method == "search.index.config.get" {
                return Ok(json!({"extensions":["md"]}));
            }
            if method == trusty_common::search_rpc::METHOD_INDEX_STATUS {
                return Ok(json!({"root_path":root}));
            }
            if method == "search.index.file.put" && fail.load(Ordering::SeqCst) {
                return Err(crate::uds_mock::RpcError::internal(
                    "synthetic index failure",
                ));
            }
            Ok(params)
        })
    })
    .await;
    ctx.search_socket = Some(daemon.socket().to_path_buf());
    assert!(
        process_using(&ctx, &source, &item, &key, |_, _| async {
            Ok((output(), "fixture-model".into()))
        })
        .await
        .is_err()
    );
    let mut saved = ctx.store().checkpoints().unwrap().remove(&key).unwrap();
    assert!(saved.output.is_some());
    assert_ne!(saved.status, "completed");
    saved.next_attempt_at = 0;
    ctx.store().checkpoint(&key, saved).unwrap();
    switch.store(false, Ordering::SeqCst);
    process_using(&ctx, &source, &item, &key, |_, _| async {
        panic!("checkpoint must avoid a second inference")
    })
    .await
    .unwrap();
    assert_eq!(ctx.store().checkpoints().unwrap()[&key].status, "completed");
    process_using(&ctx, &source, &item, &key, |_, _| async {
        panic!("restart must preserve completion")
    })
    .await
    .unwrap();
    // A failed new generation must retain the old published generation for withdrawal.
    let path = ctx.state().await.unwrap().unwrap().assistant_projects[0].clone();
    std::fs::write(
        std::path::Path::new(&path).join("notes.md"),
        "Maya leads Atlas. Revised.",
    )
    .unwrap();
    let state = ctx.state().await.unwrap().unwrap();
    let changed = inputs(&ctx, &state, &source).await.unwrap().remove(0);
    assert!(
        process_using(&ctx, &source, &changed, &key, |_, _| async {
            Err(anyhow::anyhow!("synthetic provider unavailable"))
        })
        .await
        .is_err()
    );
    let pending = ctx.store().checkpoints().unwrap().remove(&key).unwrap();
    assert!(pending.output.is_none());
    assert!(pending.materialized_fingerprint.is_some());
    ctx.store()
        .update_assistant_projects(&state.revision, &[], &[], Utc::now())
        .unwrap();
    sources::withdraw_missing(&ctx, &BTreeSet::new(), &BTreeSet::new())
        .await
        .unwrap();
    assert_eq!(ctx.store().checkpoints().unwrap()[&key].status, "cancelled");
    let kb = KbStore::new(state.store.root, Profile::default_profile());
    for (_, path) in kb.entity_files("notes").unwrap() {
        assert!(
            std::fs::read_to_string(path)
                .unwrap()
                .contains("source_status: deleted")
        );
    }
}

#[tokio::test]
async fn revoked_source_cannot_publish_after_inference() {
    let (_tmp, ctx, source, item, key) = prepared().await;
    let store = ctx.store();
    let before = ctx.state().await.unwrap().unwrap();
    let result = process_using(&ctx, &source, &item, &key, move |_, _| async move {
        store
            .update_assistant_projects(&before.revision, &[], &[], Utc::now())
            .unwrap();
        Ok((output(), "fixture-model".into()))
    })
    .await;
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("authorization changed")
    );
    let kb = KbStore::new(
        ctx.state().await.unwrap().unwrap().store.root,
        Profile::default_profile(),
    );
    assert!(kb.entity_files("notes").unwrap().is_empty());
}

#[test]
fn host_identity_scopes_equal_names_to_the_source_item() {
    let input = SourceItem {
        item_id: "one".into(),
        fingerprint: "hash".into(),
        name: "one".into(),
        title: "one".into(),
        timestamp: None,
        body: "Maya leads Atlas.".into(),
        fields: BTreeMap::new(),
        volatile: false,
    };
    let one = entities("source-one", &input, &output(), "fixture");
    let two = entities("source-two", &input, &output(), "fixture");
    assert_eq!(one[0].name, one[0].item_id);
    assert_ne!(one[0].name, two[0].name);
    assert_eq!(one[0].title, "Maya");
}

#[tokio::test]
async fn persisted_listener_excerpt_requires_current_admission_and_binding() {
    use crate::listeners::store::{EventStore, StoredEvent};
    let (tmp, dirs, root) = fixture();
    let mut ctx = context(&dirs, &root, vec![]).await;
    ctx.listeners.push(
        toml::from_str(
            "name='mail'\nconnector='gmail'\nenabled=true\nidentity='synthetic-account'\n",
        )
        .unwrap(),
    );
    ctx.config
        .listeners
        .push(toml::from_str("name='mail'\nenabled=true\n").unwrap());
    let source = ctx.sources(&BTreeMap::new()).unwrap().remove(0);
    let mut state = ctx.store().initialize(Utc::now(), None).unwrap();
    let event = StoredEvent {
        id: "mail:synthetic-1".into(),
        listener_id: "mail".into(),
        provider: "gmail".into(),
        event_type: "message.received".into(),
        ts: Utc::now().to_rfc3339(),
        from: Some("fixture@example.invalid".into()),
        subject: Some("Atlas".into()),
        snippet: Some("Maya leads Atlas.".into()),
        included: true,
        labels: vec![],
    };
    let events = tmp.path().join("events");
    EventStore::append_at(&events, &event).await.unwrap();
    let loaded = EventStore::read_events_at(&events, None).await.unwrap();
    assert!(
        event_inputs(&ctx, &state, &source, loaded.clone())
            .unwrap()
            .is_empty()
    );
    state.admitted_events.push(crate::knowledge::event_digest(
        &source.id,
        &source.revision,
        &event.id,
    ));
    let items = event_inputs(&ctx, &state, &source, loaded.clone()).unwrap();
    assert_eq!(items.len(), 1);
    assert!(
        crate::knowledge::extraction::validate(
            &serde_json::to_string(&output()).unwrap(),
            &items[0].body
        )
        .is_ok()
    );
    ctx.config.listeners[0].enabled = false;
    assert!(
        event_inputs(&ctx, &state, &source, loaded)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn pausing_during_one_item_prevents_later_inference() {
    let (_tmp, ctx, source, item, key) = prepared().await;
    let result = process_using(&ctx, &source, &item, &key, |_, _| async {
        let state = ctx.store().status().unwrap().unwrap();
        ctx.store()
            .set_paused(&state.revision, true, Utc::now())
            .unwrap();
        Ok((output(), "fixture".into()))
    })
    .await;
    assert!(result.unwrap_err().to_string().contains("paused"));
    let result = process_using(&ctx, &source, &item, "later-item", |_, _| {
        std::future::ready(Err(anyhow::anyhow!("Unexpected inference invocation")))
    })
    .await;
    assert!(result.unwrap_err().to_string().contains("paused"));
}

#[tokio::test]
async fn reviewed_extraction_status_never_exposes_pending_candidates() {
    let (_tmp, ctx, source, item, key) = prepared().await;
    assert!(
        process_using(&ctx, &source, &item, &key, |_, _| async {
            Ok((output(), "fixture".into()))
        })
        .await
        .is_err()
    );
    let Json(status) = ctx
        .envelope(ctx.state().await.unwrap(), false)
        .await
        .unwrap();
    let exposed = status["extraction"].to_string();
    assert!(
        !exposed.contains("Maya"),
        "unpublished entity exposed: {exposed}"
    );
    assert!(!exposed.contains("evidence_quote"));
    assert!(!exposed.contains("output"));
}

#[tokio::test]
async fn reviewed_project_snapshots_follow_requested_intervals() {
    let (_tmp, ctx, source, _, _) = prepared().await;
    let state = ctx.state().await.unwrap().unwrap();
    let project = std::path::PathBuf::from(&state.assistant_projects[0]);
    let previous = state
        .jobs
        .iter()
        .find(|j| j.source_id == source.id)
        .unwrap()
        .clone();
    let old_time = previous.window.start - chrono::Duration::days(1);
    let old = project.join("older.md");
    std::fs::write(&old, "Maya leads Atlas.").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(old_time.into()))
        .unwrap();
    let allowed = inputs(&ctx, &state, &source).await.unwrap();
    assert_eq!(allowed.len(), 1);
    assert_eq!(allowed[0].item_id, "notes.md");
    let extended = ctx
        .store()
        .extend_history(&state.revision, 1, Utc::now())
        .unwrap();
    assert_eq!(inputs(&ctx, &extended, &source).await.unwrap().len(), 2);
    ctx.store()
        .execution_status(&previous.id, true, "First interval completed")
        .unwrap();
    let updated = ctx.store().status().unwrap().unwrap();
    assert_eq!(
        updated
            .jobs
            .iter()
            .find(|j| j.id == previous.id)
            .unwrap()
            .status,
        crate::knowledge::JobStatus::Completed
    );
    assert!(
        updated
            .jobs
            .iter()
            .filter(|j| j.id != previous.id)
            .all(|j| j.status != crate::knowledge::JobStatus::Completed)
    );
}
