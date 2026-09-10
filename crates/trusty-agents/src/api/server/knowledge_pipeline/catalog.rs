//! Trusted source catalogue for the Assistant knowledge API (#4531).
//! Registry membership and effective bindings define scope; request bodies cannot supply sources.
use super::{Error, bad, failure};
use crate::{
    agents::AgentConfig,
    knowledge::{SourceDescriptor, SourceKind},
    listeners::config::ListenerConfig,
    registry::{ProjectEntry, ProjectStatus},
};
use axum::http::StatusCode;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

pub(super) fn digest(value: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(value.as_ref()))
}

/// Resolve only existing registered directories; aliases collapse to canonical identity.
/// Test: `registered_selection_rejects_unregistered_and_deduplicates_aliases`.
pub(super) fn registered(entries: Vec<ProjectEntry>) -> BTreeMap<String, String> {
    entries
        .into_iter()
        .filter(|p| p.status != ProjectStatus::Removed)
        .filter_map(|p| {
            let path = p.path.canonicalize().ok()?;
            (path.is_dir() && path.parent().is_some())
                .then(|| (path.to_string_lossy().into_owned(), p.name))
        })
        .collect()
}

pub(super) fn validate_projects(
    paths: &[String],
    registered: &BTreeMap<String, String>,
) -> Result<Vec<String>, Error> {
    if paths.len() > 64 {
        return Err(bad("At most 64 project folders may be attached to a chat"));
    }
    let mut selected = BTreeSet::new();
    for path in paths {
        let canonical = PathBuf::from(path)
            .canonicalize()
            .map_err(|_| bad("Project folder is unavailable"))?;
        let key = canonical.to_string_lossy().into_owned();
        if !registered.contains_key(&key) {
            return Err(bad("Choose a registered project folder"));
        }
        selected.insert(key);
    }
    Ok(selected.into_iter().collect())
}

fn descriptor(
    id: String,
    revision: String,
    kind: SourceKind,
    display_name: String,
) -> SourceDescriptor {
    let mut dependency_reasons = vec![
        "Business-entity NLP extraction and inexpensive inference cleanup require the upstream OKG pipeline (#4283)".into(),
        "Durable extraction execution and searchable publication are pending (#4538)".into(),
    ];
    match kind {
        SourceKind::Project => dependency_reasons
            .push("Current-file modification-time extraction manifests are pending (#4540)".into()),
        SourceKind::Gmail | SourceKind::Gdrive => dependency_reasons
            .push("Complete, resumable monthly source pagination is pending (#4534)".into()),
        SourceKind::Slack => dependency_reasons
            .push("Slack monthly history and delta ingestion are pending (#4546)".into()),
        SourceKind::Gcal => dependency_reasons
            .push("Google Calendar monthly history and delta ingestion are pending (#7329)".into()),
    }
    SourceDescriptor {
        id,
        revision,
        kind,
        display_name,
        dependency_reasons,
    }
}

pub(super) fn listener_source(
    config: &ListenerConfig,
    binding: &crate::listeners::config::AgentListenerBinding,
) -> Option<SourceDescriptor> {
    if !config.enabled || !binding.enabled || binding.validate().is_err() {
        return None;
    }
    let kind = match config.connector.as_str() {
        "gmail" => SourceKind::Gmail,
        "drive" | "gdrive" | "google-drive" => SourceKind::Gdrive,
        "calendar" | "gcal" | "google-calendar" => SourceKind::Gcal,
        _ => return None,
    };
    let scope = serde_json::to_vec(&(config, binding)).ok()?;
    Some(descriptor(
        format!("listener-{}", digest(&config.name)),
        digest(scope),
        kind,
        config.name.clone(),
    ))
}

pub(super) fn slack_source(
    binding: &super::super::agent_channels::Binding,
) -> Option<SourceDescriptor> {
    if binding.provider != "slack" || !binding.enabled || !binding.receive_enabled {
        return None;
    }
    Some(descriptor(
        format!("channel-{}", digest(&binding.id)),
        digest(serde_json::to_vec(binding).ok()?),
        SourceKind::Slack,
        binding.name.clone(),
    ))
}

/// Build the union of selected project folders and enabled receiving bindings.
/// Test: `catalogue_never_promotes_registration_or_disabled_channels_to_sources`.
pub(super) fn sources(
    projects_by_chat: &BTreeMap<String, Vec<String>>,
    projects: &BTreeMap<String, String>,
    agent: &AgentConfig,
    listeners: &[ListenerConfig],
    channels: &[super::super::agent_channels::Binding],
) -> Result<Vec<SourceDescriptor>, Error> {
    let mut sources = BTreeMap::new();
    for path in projects_by_chat.values().flatten().collect::<BTreeSet<_>>() {
        if let Some(name) = projects.get(path) {
            let source = descriptor(
                format!("project-{}", digest(path)),
                digest(format!("project-modified-v1:{path}")),
                SourceKind::Project,
                name.clone(),
            );
            sources.insert(source.id.clone(), source);
        }
    }
    for binding in &agent.listeners {
        if let Some(listener) = listeners.iter().find(|c| c.name == binding.name)
            && let Some(source) = listener_source(listener, binding)
        {
            sources.insert(source.id.clone(), source);
        }
    }
    for binding in channels {
        if let Some(source) = slack_source(binding) {
            sources.insert(source.id.clone(), source);
        }
    }
    if sources.len() > 128 {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "At most 128 knowledge sources are supported",
        ));
    }
    Ok(sources.into_values().collect())
}
