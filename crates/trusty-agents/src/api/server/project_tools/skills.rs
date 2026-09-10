//! Registered-project skill settings; revisions prevent stale UI overwrites.
use super::*;
use crate::skills::manage::WRITE_LOCK;
use crate::skills::project::{self, SourceUpdate};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::api::server) struct SkillUpdate {
    path: String,
    revision: String,
    sources: Vec<SourceUpdate>,
}
pub(in crate::api::server) async fn get_project_skills(
    Query(query): Query<ProjectPath>,
) -> Result<Json<Value>, ApiError> {
    let root = registered_path(&ProjectRegistry::new().map_err(internal)?, &query.path).await?;
    project::snapshot(&root)
        .and_then(|s| Ok(serde_json::to_value(s)?))
        .map(Json)
        .map_err(internal)
}
pub(in crate::api::server) async fn patch_project_skills(
    Json(req): Json<SkillUpdate>,
) -> Result<Json<Value>, ApiError> {
    let _lock = WRITE_LOCK.lock().await;
    let _process_lock = crate::skills::manage::mutation_lock()
        .await
        .map_err(internal)?;
    let root = registered_path(&ProjectRegistry::new().map_err(internal)?, &req.path).await?;
    let result = project::save(&root, &req.revision, req.sources).map_err(|e| {
        failure(
            if e.to_string().starts_with("Revision conflict") {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_REQUEST
            },
            e,
        )
    })?;
    Ok(Json(serde_json::to_value(result).map_err(internal)?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::api::server) struct UserSkillUpdate {
    revision: String,
    sources: Vec<SourceUpdate>,
}
pub(in crate::api::server) async fn get_user_skills() -> Result<Json<Value>, ApiError> {
    project::user_snapshot()
        .and_then(|s| Ok(serde_json::to_value(s)?))
        .map(Json)
        .map_err(internal)
}
pub(in crate::api::server) async fn patch_user_skills(
    Json(req): Json<UserSkillUpdate>,
) -> Result<Json<Value>, ApiError> {
    let _lock = WRITE_LOCK.lock().await;
    let _process_lock = crate::skills::manage::mutation_lock()
        .await
        .map_err(internal)?;
    let root = project::user_root().map_err(internal)?;
    let value = project::save_scoped(&root, true, &req.revision, req.sources).map_err(|e| {
        failure(
            if e.to_string().starts_with("Revision conflict") {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_REQUEST
            },
            e,
        )
    })?;
    Ok(Json(serde_json::to_value(value).map_err(internal)?))
}
