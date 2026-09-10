//! Intake records pending work only; it never runs inference on a listener thread.
use super::{Context, catalog, disk};
use crate::listeners::store::StoredEvent;
use chrono::Utc;

/// Admit an already authorized source event, with no body persisted in pipeline state.
/// Test: core `late_record_is_admitted_once_without_widening_history`.
async fn admit(
    context: Context,
    source: crate::knowledge::SourceDescriptor,
    event: &StoredEvent,
) -> Result<(), super::Error> {
    let record_time = chrono::DateTime::parse_from_rfc3339(&event.ts)
        .map_err(super::bad)?
        .with_timezone(&Utc);
    let event_id = event.id.clone();
    let store = context.store();
    disk(move || {
        store.enqueue_event(
            &source.id,
            &source.revision,
            &event_id,
            record_time,
            Utc::now(),
        )
    })
    .await?;
    replay(&context).await?;
    Ok(())
}

/// Retry pending identities on startup, sync, or the next source arrival.
pub(super) async fn replay(context: &Context) -> Result<(), super::Error> {
    for _ in 0..4 {
        let context = context.reload().await?;
        let Some(state) = context.state().await? else {
            return Ok(());
        };
        let state = match context.current(&state.revision).await {
            Ok(s) => s,
            Err((axum::http::StatusCode::CONFLICT, _)) => continue,
            Err(e) => return Err(e),
        };
        let sources = context.sources(&state.projects_by_chat)?;
        let store = context.store();
        match disk(move || store.replay_inbox(&state.revision, &sources, Utc::now())).await {
            Ok(_) => return Ok(()),
            Err((axum::http::StatusCode::CONFLICT, _)) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(super::core_error(
        crate::knowledge::KnowledgeError::Conflict,
    ))
}

/// Gmail/Google listener events retain both configured filter stages before admission.
pub(crate) async fn listener(event: &StoredEvent) {
    if !event.included {
        return;
    }
    let dirs = crate::agents::agents_dir_candidates();
    for id in crate::assistants::discover_instances(&dirs) {
        let result = async {
            let context = Context::load(id.as_str()).await?;
            let source = context.config.listeners.iter().find_map(|binding| {
                if !crate::listeners::wake::binding_matches_event(binding, event) {
                    return None;
                }
                let listener = context
                    .listeners
                    .iter()
                    .find(|l| l.name == binding.name && l.filter.matches_labels(&event.labels))?;
                catalog::listener_source(listener, binding)
            });
            if let Some(source) = source {
                admit(context, source, event).await?;
            }
            Ok::<_, super::Error>(())
        }
        .await;
        if let Err((status, _)) = result {
            tracing::warn!(assistant=%id, %status, "knowledge intake pending; pipeline status or source configuration needs attention");
        }
    }
}

/// Called after Slack's authenticated sender, target and Assistant allowlist checks.
pub(crate) async fn slack(
    name: &str,
    binding: &super::super::agent_channels::Binding,
    event: &StoredEvent,
) {
    let result = async {
        let context = Context::load(name).await?;
        let Some(current) = context.channels.iter().find(|b| b.id == binding.id) else {
            return Ok(());
        };
        let Some(source) = catalog::slack_source(current) else {
            return Ok(());
        };
        if catalog::slack_source(binding).as_ref() != Some(&source) {
            return Ok(());
        }
        admit(context, source, event).await
    }
    .await;
    if let Err((status, _)) = result {
        tracing::warn!(assistant=name, %status, "Slack knowledge intake pending; inspect pipeline status");
    }
}
