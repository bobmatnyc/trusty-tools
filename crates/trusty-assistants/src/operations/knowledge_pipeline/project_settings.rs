//! Persistent selection intent, independent of extraction readiness (#3931).
use super::*;
pub(super) async fn update_projects(
    context: Context,
    request: Projects,
) -> Result<Json<Value>, Error> {
    let _publication_guard = crate::knowledge::execution::mutation_guard(&context.manifest)
        .await
        .map_err(|e| bad(e.to_string()))?;
    let previous = context.state().await?;
    let retained: Vec<String> = previous
        .as_ref()
        .map(|s| s.project_selections().into_values().flatten().collect())
        .unwrap_or_default();
    let mut projects = Vec::new();
    if request.projects.len() > 64 {
        return Err(bad("At most 64 project folders may be selected"));
    }
    for path in &request.projects {
        if retained.contains(path) {
            projects.push(path.clone());
        } else {
            projects.extend(catalog::validate_projects(
                std::slice::from_ref(path),
                &context.projects,
            )?);
        }
    }
    projects.sort();
    projects.dedup();
    let assistant_scope = request.scope.as_deref() == Some("assistant");
    if (assistant_scope && !request.chat_id.is_empty())
        || (!assistant_scope
            && (request.chat_id.is_empty()
                || !matches!(request.scope.as_deref(), None | Some("chat"))))
    {
        return Err(bad(
            "Use assistant scope without chat_id, or chat scope with chat_id",
        ));
    }
    // #3931: project intent is independent of extraction/index availability.
    // Provision only private metadata; legacy bindings and data stay untouched.
    let state = match previous {
        Some(state) if state.revision == request.revision => state,
        Some(_) => return Err(core_error(KnowledgeError::Conflict)),
        None if request.revision.is_empty() => {
            let store = context.store();
            let selected = context.selected_store().ok().flatten();
            let initialized = disk(move || store.initialize(Utc::now(), selected)).await?;
            if !initialized.assistant_projects.is_empty()
                || !initialized.projects_by_chat.is_empty()
            {
                return Err(core_error(KnowledgeError::Conflict));
            }
            initialized
        }
        None => return Err(core_error(KnowledgeError::Conflict)),
    };
    let revision = state.revision.clone();
    let mut selected = state.project_selections();
    selected.insert(
        if assistant_scope {
            String::new()
        } else {
            request.chat_id.clone()
        },
        projects.clone(),
    );
    let sources = context.sources(&selected)?;
    let store = context.store();
    let state = disk(move || {
        if assistant_scope {
            return store.update_assistant_projects(&revision, &projects, &sources, Utc::now());
        }
        store.update_projects(&revision, &request.chat_id, &projects, &sources, Utc::now())
    })
    .await?;
    context.envelope(Some(state), false).await
}
