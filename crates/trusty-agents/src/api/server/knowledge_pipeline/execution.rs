//! Automatic, bounded source extraction; no user chat or manual ingest is required (#4283).
use super::*;
use crate::knowledge::{execution::Checkpoint, extraction::Extraction};
use std::collections::BTreeSet;
use trusty_kb::{
    okg::{
        ingest::SourceItem,
        registry::{Locator, SourceSpec},
    },
    schema::Profile,
    store::KbStore,
};

#[path = "execution_sources.rs"]
mod sources;
#[cfg(test)]
use sources::event_inputs;
use sources::inputs;
pub(super) async fn run(name: &str) -> anyhow::Result<()> {
    sources::run_context(&Context::load(name).await.map_err(api_error)?).await
}
fn api_error((status, Json(value)): Error) -> anyhow::Error {
    anyhow::anyhow!("{status}: {}", value["error"])
}

async fn current(
    context: &Context,
    source: &SourceDescriptor,
    item: &SourceItem,
) -> anyhow::Result<(Context, KnowledgeState)> {
    let current = context.reload().await.map_err(api_error)?;
    let state = current
        .state()
        .await
        .map_err(api_error)?
        .ok_or_else(|| anyhow::anyhow!("Knowledge state unavailable"))?;
    anyhow::ensure!(!state.paused, "Extraction paused");
    anyhow::ensure!(
        current
            .sources(&state.project_selections())
            .map_err(api_error)?
            .contains(source),
        "Source authorization changed"
    );
    let items = inputs(&current, &state, source).await?;
    anyhow::ensure!(
        items.iter().any(|i| i.item_id == item.item_id
            && i.fingerprint == item.fingerprint
            && i.body == item.body),
        "Source content changed during extraction"
    );
    Ok((current, state))
}
async fn process(
    context: &Context,
    source: &SourceDescriptor,
    item: &SourceItem,
    key: &str,
) -> anyhow::Result<()> {
    process_using(context, source, item, key, |config, text| async move {
        crate::knowledge::inference::extract(config, &text).await
    })
    .await
}
async fn process_using<F, Fut>(
    context: &Context,
    source: &SourceDescriptor,
    item: &SourceItem,
    key: &str,
    extract: F,
) -> anyhow::Result<()>
where
    F: FnOnce(AgentConfig, String) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<(Extraction, String)>>,
{
    let now = Utc::now().timestamp();
    let fingerprint = catalog::digest(format!(
        "schema1:{}:{}:{}",
        source.revision,
        item.fingerprint,
        crate::knowledge::inference::fingerprint(&context.config)
    ));
    let existing = context.store().checkpoints()?.remove(key);
    if existing
        .as_ref()
        .is_some_and(|c| c.fingerprint == fingerprint && c.status == "completed")
    {
        return Ok(());
    }
    anyhow::ensure!(
        !existing
            .as_ref()
            .is_some_and(|c| c.fingerprint == fingerprint && c.next_attempt_at > now),
        "Extraction retry is scheduled"
    );
    let materialized_fingerprint = existing.as_ref().and_then(|c| {
        c.materialized_fingerprint
            .clone()
            .or_else(|| c.needs_withdrawal().then(|| c.fingerprint.clone()))
    });
    let mut checkpoint = match existing {
        Some(c) if c.fingerprint == fingerprint => c,
        _ => Checkpoint {
            source_id: source.id.clone(),
            item_id: item.item_id.clone(),
            fingerprint,
            model: context.config.agent.model.clone(),
            status: "running".into(),
            attempts: 0,
            next_attempt_at: 0,
            lease_owner: uuid::Uuid::new_v4().to_string(),
            lease_until: now + 300,
            output: None,
            materialized_fingerprint,
            last_error: None,
        },
    };
    checkpoint.attempts += 1;
    checkpoint.status = "running".into();
    checkpoint.lease_until = now + 300;
    context.store().checkpoint(key, checkpoint.clone())?;
    let result = async {
        if checkpoint.output.is_none() {
            let (fresh, _) = current(context, source, item).await?;
            anyhow::ensure!(
                crate::knowledge::inference::fingerprint(&fresh.config)
                    == crate::knowledge::inference::fingerprint(&context.config),
                "Extraction provider configuration changed"
            );
            let (output, model) = tokio::time::timeout(
                std::time::Duration::from_secs(180),
                extract(fresh.config, item.body.clone()),
            )
            .await??;
            checkpoint.output = Some(output);
            checkpoint.model = model;
            checkpoint.status = "publication_pending".into();
            context.store().checkpoint(key, checkpoint.clone())?;
        }
        let _gate = crate::knowledge::execution::mutation_guard(&context.manifest).await?;
        let (current, state) = current(context, source, item).await?;
        let latest = current
            .store()
            .checkpoints()?
            .remove(key)
            .ok_or_else(|| anyhow::anyhow!("Extraction claim disappeared"))?;
        anyhow::ensure!(
            latest.lease_owner == checkpoint.lease_owner
                && latest.fingerprint == checkpoint.fingerprint
                && checkpoint.lease_until >= Utc::now().timestamp(),
            "Extraction lease superseded"
        );
        let output = checkpoint
            .output
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Validated extraction missing"))?;
        // #4283: journal potential side effects before publication, retaining older generations on refresh failure.
        checkpoint.materialized_fingerprint = Some(checkpoint.fingerprint.clone());
        current.store().checkpoint(key, checkpoint.clone())?;
        publish(&current, &state, key, item, output, &checkpoint.model).await?;
        if let Err(error) = self::current(&current, source, item).await {
            withdraw(&current, &state, key).await?;
            return Err(error);
        }
        checkpoint.status = "completed".into();
        checkpoint.last_error = None;
        checkpoint.lease_until = 0;
        context.store().checkpoint(key, checkpoint.clone())?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if let Err(error) = &result {
        checkpoint.status = "retryable".into();
        checkpoint.last_error = Some(error.to_string());
        checkpoint.next_attempt_at = now + (5i64 * 2i64.pow(checkpoint.attempts.min(6))).min(300);
        checkpoint.lease_until = 0;
        context.store().checkpoint(key, checkpoint)?;
    }
    result
}
fn spec(key: &str, root: &std::path::Path) -> SourceSpec {
    let mut spec = SourceSpec::new(
        &format!("extracted-{key}"),
        Some("notes"),
        Locator::DocStore {
            path: root.to_string_lossy().into_owned(),
            extensions: vec![],
            recursive: false,
        },
        &Utc::now().to_rfc3339(),
    );
    // This source denotes exactly ONE complete extracted item, never a partial external corpus.
    spec.tombstone_deleted = true;
    spec
}
async fn publish(
    context: &Context,
    state: &KnowledgeState,
    key: &str,
    input: &SourceItem,
    output: &Extraction,
    model: &str,
) -> anyhow::Result<()> {
    let store = KbStore::new(state.store.root.clone(), Profile::default_profile());
    let spec = spec(key, &state.store.root);
    let mut registry = trusty_kb::okg::registry::SourceRegistry::load(&store.root)?;
    registry.upsert(spec.clone());
    registry.save(&store.root)?;
    let items = entities(key, input, output, model);
    let report = store.ingest_items(&spec, &items, true, &Utc::now().to_rfc3339())?;
    anyhow::ensure!(
        report.errors.is_empty(),
        "Entity ingest incomplete: {:?}",
        report.errors
    );
    feed(context, state, &store, &spec).await
}
fn entities(key: &str, input: &SourceItem, output: &Extraction, model: &str) -> Vec<SourceItem> {
    let ids: BTreeMap<_, _> = output
        .entities
        .iter()
        .map(|e| {
            (
                e.id.clone(),
                format!(
                    "entity-{}",
                    catalog::digest(format!("{key}:{}:{}", e.kind, e.name.to_lowercase()))
                ),
            )
        })
        .collect();
    output.entities.iter().map(|entity| {
        let id=ids[&entity.id].clone();
        let outgoing:Vec<_>=output.relationships.iter().filter(|r|r.subject==entity.id).collect();
        let relationships:Vec<_>=outgoing.iter().map(|r|json!({"predicate":r.predicate,"target":ids[&r.object],"evidence_quote":r.evidence_quote})).collect();
        let body=json!({"name":entity.name,"type":entity.kind,"claims":entity.claims,"relationships":relationships}).to_string();
        let mut fields=BTreeMap::from([("entity_type".into(),entity.kind.clone()),("extraction_model".into(),model.into()),("source_record".into(),input.item_id.clone()),("source_fingerprint".into(),input.fingerprint.clone())]);
        for relation in outgoing {
            let link=format!("[[{}]]",ids[&relation.object]);
            fields.entry(relation.predicate.clone()).and_modify(|v|{v.push(' ');v.push_str(&link)}).or_insert(link);
        }
        SourceItem{item_id:id.clone(),name:id,title:entity.name.clone(),fingerprint:catalog::digest(&body),timestamp:input.timestamp.clone(),body,fields,volatile:false}
    }).collect()
}
async fn withdraw(context: &Context, state: &KnowledgeState, key: &str) -> anyhow::Result<()> {
    let store = KbStore::new(state.store.root.clone(), Profile::default_profile());
    let spec = spec(key, &state.store.root);
    let report = store.ingest_items(&spec, &[], true, &Utc::now().to_rfc3339())?;
    anyhow::ensure!(report.errors.is_empty(), "Entity withdrawal incomplete");
    feed(context, state, &store, &spec).await
}
async fn feed(
    context: &Context,
    state: &KnowledgeState,
    store: &KbStore,
    spec: &SourceSpec,
) -> anyhow::Result<()> {
    let socket = context
        .search_socket
        .clone()
        .ok_or_else(|| anyhow::anyhow!("trusty-search unavailable"))?;
    indexing::at(&socket, &state.store, true).await?;
    let report = crate::stores::index_feed::feed_source(
        store,
        spec,
        &state.store.index_id,
        &crate::stores::index_feed_rpc::RpcIndexFeed::new(socket),
        &Utc::now().to_rfc3339(),
    )
    .await?;
    anyhow::ensure!(
        report.pending == 0
            && report.errors.is_empty()
            && report.reason.is_none()
            && report.index.as_deref() == Some(state.store.index_id.as_str()),
        "Index publication not acknowledged: {:?}",
        report
    );
    Ok(())
}

#[cfg(test)]
#[path = "execution_tests.rs"]
mod tests;
