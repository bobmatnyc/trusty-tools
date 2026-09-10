//! Source admission and interval-scoped extraction execution (#4283).
use super::*;
use crate::knowledge::{JobStatus, KnowledgeJob, SourceKind};

fn in_window(job: &KnowledgeJob, item: &SourceItem) -> anyhow::Result<bool> {
    let time = chrono::DateTime::parse_from_rfc3339(
        item.timestamp
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Source modified timestamp unavailable"))?,
    )?
    .with_timezone(&Utc);
    Ok(time >= job.window.start && time < job.window.end)
}
fn job_matches(job: &KnowledgeJob, source: &SourceDescriptor) -> bool {
    job.source_id == source.id
        && job.source_revision == source.revision
        && job.status != JobStatus::Cancelled
}
/// Why: project snapshots outside requested history must never be sent to inference.
/// What: admit requested half-open monthly windows and ongoing arrivals since initialization.
/// Test: `reviewed_project_snapshots_follow_requested_intervals`.
fn project_item_authorized(
    state: &KnowledgeState,
    source: &SourceDescriptor,
    item: &SourceItem,
) -> anyhow::Result<bool> {
    let time = chrono::DateTime::parse_from_rfc3339(
        item.timestamp
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Source modified timestamp unavailable"))?,
    )?
    .with_timezone(&Utc);
    if time >= state.anchor_at && time <= Utc::now() {
        return Ok(true);
    }
    for job in state.jobs.iter().filter(|j| job_matches(j, source)) {
        if in_window(job, item)? {
            return Ok(true);
        }
    }
    Ok(false)
}
pub(super) async fn run_context(context: &Context) -> anyhow::Result<()> {
    let Some(state) = context.state().await.map_err(api_error)? else {
        return Ok(());
    };
    if state.paused {
        return Ok(());
    }
    let Some(_worker) = context.store().worker_lock()? else {
        return Ok(());
    };
    let sources = context
        .sources(&state.project_selections())
        .map_err(api_error)?;
    let mut seen = BTreeSet::new();
    let mut scanned = BTreeSet::new();
    let mut outcomes = Vec::new();
    for source in sources {
        let active = context.reload().await.map_err(api_error)?;
        let active_state = active
            .state()
            .await
            .map_err(api_error)?
            .ok_or_else(|| anyhow::anyhow!("Knowledge state missing"))?;
        if active_state.paused {
            break;
        }
        if !active
            .sources(&active_state.project_selections())
            .map_err(api_error)?
            .contains(&source)
        {
            continue;
        }
        let jobs: Vec<_> = active_state
            .jobs
            .iter()
            .filter(|j| {
                job_matches(j, &source)
                    && (source.kind == SourceKind::Project || j.record.is_some())
            })
            .cloned()
            .collect();
        match inputs(&active, &active_state, &source).await {
            Err(error) => {
                for job in jobs {
                    outcomes.push((job.id, false, error.to_string()));
                }
            }
            Ok(items) => {
                scanned.insert(source.id.clone());
                let mut failed = BTreeSet::new();
                for item in &items {
                    let key = catalog::digest(format!("{}:{}", source.id, item.item_id));
                    seen.insert(key.clone());
                    if let Err(error) = process(&active, &source, item, &key).await {
                        failed.insert(item.item_id.clone());
                        tracing::warn!(source=%source.id,%error,"Automatic extraction pending");
                    }
                }
                for job in jobs {
                    let mut complete = job.record.is_none();
                    for item in &items {
                        let member = if let Some(record) = &job.record {
                            record.event_digest
                                == crate::knowledge::event_digest(
                                    &source.id,
                                    &source.revision,
                                    &item.item_id,
                                )
                        } else {
                            in_window(&job, item)?
                        };
                        if member {
                            if failed.contains(&item.item_id) {
                                complete = false;
                                break;
                            }
                            if job.record.is_some() {
                                complete = true;
                            }
                        }
                    }
                    outcomes.push((
                        job.id,
                        complete,
                        if complete {
                            "Authorized interval snapshots published"
                        } else {
                            "An interval item is pending extraction or publication"
                        }
                        .into(),
                    ));
                }
            }
        }
    }
    withdraw_missing(context, &seen, &scanned).await?;
    for (job, completed, reason) in outcomes {
        context.store().execution_status(&job, completed, &reason)?;
    }
    Ok(())
}
pub(super) async fn withdraw_missing(
    context: &Context,
    seen: &BTreeSet<String>,
    scanned: &BTreeSet<String>,
) -> anyhow::Result<()> {
    for (key, mut checkpoint) in context.store().checkpoints()? {
        if checkpoint.status == "cancelled" || !checkpoint.needs_withdrawal() || seen.contains(&key)
        {
            continue;
        }
        let _gate = crate::knowledge::execution::mutation_guard(&context.manifest).await?;
        let current = context.reload().await.map_err(api_error)?;
        let state = current
            .state()
            .await
            .map_err(api_error)?
            .ok_or_else(|| anyhow::anyhow!("Knowledge state missing"))?;
        let eligible = current
            .sources(&state.project_selections())
            .map_err(api_error)?;
        let missing = if let Some(source) = eligible.iter().find(|s| s.id == checkpoint.source_id) {
            scanned.contains(&source.id)
                && !inputs(&current, &state, source)
                    .await?
                    .iter()
                    .any(|i| i.item_id == checkpoint.item_id)
        } else {
            true
        };
        if missing {
            withdraw(&current, &state, &key).await?;
            checkpoint.status = "cancelled".into();
            checkpoint.output = None;
            checkpoint.materialized_fingerprint = None;
            current.store().checkpoint(&key, checkpoint)?;
        }
    }
    Ok(())
}
pub(super) async fn inputs(
    context: &Context,
    state: &KnowledgeState,
    source: &SourceDescriptor,
) -> anyhow::Result<Vec<SourceItem>> {
    if source.kind == crate::knowledge::SourceKind::Project {
        let path = state
            .project_selections()
            .into_values()
            .flatten()
            .find(|p| format!("project-{}", catalog::digest(p)) == source.id)
            .ok_or_else(|| anyhow::anyhow!("Project no longer selected"))?;
        let root = PathBuf::from(path).canonicalize()?;
        let scan = tokio::task::spawn_blocking(move || {
            trusty_kb::okg::docstore::scan(
                &root,
                &[],
                true,
                12000,
                &trusty_kb::okg::policy::DocStorePolicy::new(vec![root.clone()]),
            )
        })
        .await??;
        anyhow::ensure!(
            scan.errors.is_empty(),
            "Source scan incomplete: {}",
            scan.errors.join("; ")
        );
        anyhow::ensure!(
            scan.items.len() <= 512,
            "Source exceeds 512 text chunks; narrow the selected folder"
        );
        return scan
            .items
            .into_iter()
            .filter_map(|item| match project_item_authorized(state, source, &item) {
                Ok(true) => Some(Ok(item)),
                Ok(false) => None,
                Err(e) => Some(Err(e)),
            })
            .collect();
    }
    let events = crate::listeners::store::EventStore::read_events(Some(10000)).await?;
    event_inputs(context, state, source, events)
}
pub(super) fn event_inputs(
    context: &Context,
    state: &KnowledgeState,
    source: &SourceDescriptor,
    events: Vec<crate::listeners::store::StoredEvent>,
) -> anyhow::Result<Vec<SourceItem>> {
    let mut out = vec![];
    for event in events {
        if !event.included {
            continue;
        }
        let digest = crate::knowledge::event_digest(&source.id, &source.revision, &event.id);
        if !state.admitted_events.contains(&digest) {
            continue;
        }
        let permitted = if source.kind == crate::knowledge::SourceKind::Slack {
            context
                .channels
                .iter()
                .any(|b| catalog::slack_source(b).as_ref() == Some(source))
        } else {
            context.config.listeners.iter().any(|b| {
                crate::listeners::wake::binding_matches_event(b, &event)
                    && context.listeners.iter().any(|l| {
                        l.filter.matches_labels(&event.labels)
                            && catalog::listener_source(l, b).as_ref() == Some(source)
                    })
            })
        };
        if !permitted {
            continue;
        }
        let body = format!(
            "{}\n{}",
            event.subject.as_deref().unwrap_or(""),
            event.snippet.as_deref().unwrap_or("")
        );
        anyhow::ensure!(!body.trim().is_empty(), "Source event body unavailable");
        anyhow::ensure!(body.len() <= 48000, "Source event excerpt exceeds limit");
        out.push(SourceItem {
            item_id: event.id.clone(),
            fingerprint: catalog::digest(&body),
            name: source.id.clone(),
            title: event.subject.unwrap_or_else(|| event.id.clone()),
            timestamp: Some(event.ts),
            body,
            fields: BTreeMap::from([("content_scope".into(), "persisted event excerpt".into())]),
            volatile: false,
        });
    }
    Ok(out)
}
