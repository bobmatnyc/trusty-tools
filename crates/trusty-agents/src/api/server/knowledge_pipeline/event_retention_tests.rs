//! Bounded event processing must not invent upstream deletion (#4283).
use super::super::tests::{context, fixture};
use super::*;
use crate::listeners::store::{EventStore, StoredEvent};
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn published_event_survives_unrelated_event_batch_rollover() {
    if std::env::var_os("TRUSTY_EVENT_RETENTION_CHILD").is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let status = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "api::server::knowledge_pipeline::execution::event_retention_tests::published_event_survives_unrelated_event_batch_rollover"])
            .env("TRUSTY_EVENT_RETENTION_CHILD", "1").env("HOME", &root)
            .env("TAGENT_PROJECT_DIR", &root).env("TAGENT_CONFIG_DIR", root.join("config"))
            .env("TAGENT_ASSISTANTS_DIR", root.join("homes"))
            .env("TRUSTY_DATA_DIR_OVERRIDE", root.join("data"))
            .env("TRUSTY_MEMORY_SOCKET", root.join("absent-memory.sock"))
            .env("TRUSTY_SEARCH_SOCKET", root.join("absent-search.sock"))
            .env_remove("OPEN_MPM_CONFIG_DIR").env_remove("OPEN_MPM_PROJECT_DIR")
            .status().await.unwrap();
        assert!(status.success());
        return;
    }
    let _home = crate::test_env::lock_home();
    let (_tmp, dirs, homes) = fixture();
    let manifest = dirs[0].join("twin.toml");
    let raw = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        &manifest,
        format!("{raw}\n[[listeners]]\nname='mail'\nenabled=true\n"),
    )
    .unwrap();
    let mut ctx = context(&dirs, &homes, vec![]).await;
    ctx.listeners.push(
        toml::from_str("name='mail'\nconnector='gmail'\nenabled=true\nidentity='synthetic'\n")
            .unwrap(),
    );
    let source = ctx.sources(&BTreeMap::new()).unwrap().remove(0);
    let initial = ctx.store().initialize(Utc::now(), None).unwrap();
    let state = ctx
        .store()
        .reconcile(&initial.revision, std::slice::from_ref(&source), Utc::now())
        .unwrap();
    let event = StoredEvent {
        id: "mail:published".into(),
        listener_id: "mail".into(),
        provider: "gmail".into(),
        event_type: "message.received".into(),
        ts: Utc::now().to_rfc3339(),
        from: None,
        subject: Some("Atlas".into()),
        snippet: Some("Maya leads Atlas.".into()),
        included: true,
        labels: vec![],
    };
    let dir = crate::listeners::store::events_dir().unwrap();
    EventStore::append_at(&dir, &event).await.unwrap();
    let state = ctx
        .store()
        .admit_event(
            &state.revision,
            &source.id,
            &source.revision,
            &event.id,
            chrono::DateTime::parse_from_rfc3339(&event.ts)
                .unwrap()
                .with_timezone(&Utc),
            Utc::now(),
        )
        .unwrap();
    let root = state.store.root.clone();
    let index = state.store.index_id.clone();
    let published = Arc::new(Mutex::new(BTreeMap::<String, String>::new()));
    let visible = published.clone();
    let daemon = crate::uds_mock::spawn(move |method, params| {
        let root = root.clone();
        let index = index.clone();
        let published = published.clone();
        let method = method.to_owned();
        Box::pin(async move {
            match method.as_str() {
                trusty_common::search_rpc::METHOD_INDEXES_LIST => {
                    Ok(json!({"indexes":[{"id":index,"root_path":root}]}))
                }
                "search.index.config.get" => Ok(json!({"extensions":["md"]})),
                trusty_common::search_rpc::METHOD_INDEX_STATUS => Ok(json!({"root_path":root})),
                "search.index.file.put" => {
                    published.lock().unwrap().insert(
                        params["body"]["path"].as_str().unwrap().into(),
                        params["body"]["content"].as_str().unwrap().into(),
                    );
                    Ok(json!({}))
                }
                "search.index.file.remove" => {
                    published
                        .lock()
                        .unwrap()
                        .remove(params["body"]["path"].as_str().unwrap());
                    Ok(json!({}))
                }
                _ => Ok(params),
            }
        })
    })
    .await;
    ctx.search_socket = Some(daemon.socket().to_path_buf());
    let item = inputs(&ctx, &state, &source).await.unwrap().remove(0);
    let key = catalog::digest(format!("{}:{}", source.id, item.item_id));
    process_using(&ctx, &source, &item, &key, |_, _| async {
        Ok((crate::knowledge::extraction::validate(r#"{"entities":[{"id":"maya","type":"person","name":"Maya","claims":[{"text":"Leads Atlas","evidence_quote":"Maya leads Atlas."}]}],"relationships":[]}"#,"Maya leads Atlas.")?, "fixture".into()))
    }).await.unwrap();
    assert!(
        visible
            .lock()
            .unwrap()
            .values()
            .any(|text| text.contains("Maya"))
    );
    for n in 0..10000 {
        let mut unrelated = event.clone();
        unrelated.id = format!("other:{n}");
        unrelated.listener_id = "other".into();
        EventStore::append_at(&dir, &unrelated).await.unwrap();
    }
    assert!(inputs(&ctx, &state, &source).await.unwrap().is_empty());
    sources::withdraw_missing(&ctx, &BTreeSet::new(), &BTreeSet::from([source.id.clone()]))
        .await
        .unwrap();
    assert_eq!(ctx.store().checkpoints().unwrap()[&key].status, "completed");
    assert!(
        visible
            .lock()
            .unwrap()
            .values()
            .any(|text| text.contains("Maya"))
    );
    let kb = KbStore::new(state.store.root, Profile::default_profile());
    for (_, path) in kb.entity_files("notes").unwrap() {
        assert!(
            !std::fs::read_to_string(path)
                .unwrap()
                .contains("source_status: deleted")
        );
    }
    // Explicit exclusion remains withdrawal authority even after the event leaves the batch.
    EventStore::set_filter_at(&dir, &event.event_type, false)
        .await
        .unwrap();
    sources::withdraw_missing(&ctx, &BTreeSet::new(), &BTreeSet::from([source.id]))
        .await
        .unwrap();
    assert_eq!(ctx.store().checkpoints().unwrap()[&key].status, "cancelled");
    assert!(visible.lock().unwrap().is_empty());
}
