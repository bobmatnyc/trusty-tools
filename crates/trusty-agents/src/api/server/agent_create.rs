//! `POST /api/agents` — create a new agent from a template (#3819, epic
//! #3052).
//!
//! Why: Bob's spec correction — trusty-agents has NO SUBAGENTS, only AGENT
//! TEMPLATES. The add-agent flow (reachable from `ChatHeader`'s "+ Add
//! agent") creates a new agent FROM a template, starting with (and, for this
//! slice, limited to) the `assistant` template — name + optional description
//! only. This mirrors `PersonalityPanel.svelte`'s pre-existing "+ New agent"
//! form conceptually (name → slug, base to extend) but targets the REST API
//! (works in both Tauri and plain-browser mode) rather than the Tauri-only
//! `write_personalization_overlay` IPC command that form used, and creates a
//! real directory-PACKAGE agent (`agents/<name>/{agent.toml,persona.md}`)
//! rather than a flat `.md` overlay.
//! What: [`create_agent_route`] validates the request body, resolves the
//! `assistant` template package (`agents_dir()/assistant/{agent.toml,persona.md}`
//! — the same template every bundled install ships), and writes a new
//! package directory with `name`/`display_name`/`description` substituted
//! into the copied `agent.toml`, an empty starter `persona.md`. Rejects a
//! name that already exists (directory or flat-file collision) or isn't a
//! valid slug.
//! Test: `super::tests::agent_create::*`.

use std::path::Path;

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use toml_edit::{DocumentMut, value};

use super::handlers::agents_dir;
use super::projects::parse_agent_toml;
use super::state::AppState;

/// The only template offered in this slice — Bob: "starting with just the
/// assistant template."
const ASSISTANT_TEMPLATE: &str = "assistant";

/// Names no new agent may take — collides with the fixed Concierge/`ctrl`
/// agent or the template itself.
const RESERVED_NAMES: [&str; 2] = ["ctrl", "assistant"];

#[derive(Debug, Clone, Deserialize)]
pub(super) struct CreateAgentRequest {
    pub(super) name: String,
    #[serde(default)]
    pub(super) description: Option<String>,
    /// Optional; when present must be `"assistant"` — the only template
    /// this slice supports. Omitted defaults to `"assistant"` too.
    #[serde(default)]
    pub(super) template: Option<String>,
}

fn bad_request(msg: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": msg.into() })),
    )
        .into_response()
}

/// Filesystem-safe slug validator — lowercase alnum + `_`/`-`, must start
/// with an alnum char, capped at 64 chars. Mirrors the shape the UI's own
/// `slugify()` (`lib/roster.ts`) produces, so a name that round-trips
/// through the create form's live slug preview is always accepted here.
fn is_valid_slug(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 {
        return false;
    }
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// `POST /api/agents` — HTTP entry point.
pub(super) async fn create_agent_route(
    State(_state): State<AppState>,
    Json(req): Json<CreateAgentRequest>,
) -> Response {
    create_agent_at(&agents_dir(), req).await
}

/// Core creation logic against an explicit agents directory.
///
/// Why: Same testability rationale as `patch_agent_at`/`list_workstreams_at`.
/// What: See module doc for the full validation + write sequence.
/// Test: `super::tests::agent_create::*`.
pub(super) async fn create_agent_at(dir: &Path, req: CreateAgentRequest) -> Response {
    let name = req.name.trim().to_string();
    if !is_valid_slug(&name) {
        return bad_request(
            "invalid agent name: must be lowercase alphanumeric with '-'/'_' only, starting \
             with a letter or digit, at most 64 characters",
        );
    }
    if RESERVED_NAMES.contains(&name.as_str()) {
        return bad_request(format!("'{name}' is a reserved agent name"));
    }
    if let Some(template) = &req.template
        && template != ASSISTANT_TEMPLATE
    {
        return bad_request(format!(
            "unsupported template '{template}': only '{ASSISTANT_TEMPLATE}' is available"
        ));
    }

    let new_pkg = dir.join(&name);
    let flat_shadow = dir.join(format!("{name}.toml"));
    if new_pkg.exists() || flat_shadow.exists() {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": format!("agent '{name}' already exists") })),
        )
            .into_response();
    }

    let template_toml_path = dir.join(ASSISTANT_TEMPLATE).join("agent.toml");
    let template_raw = match tokio::fs::read_to_string(&template_toml_path).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                ?e, path = %template_toml_path.display(),
                "create_agent: assistant template unreadable"
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "the 'assistant' template is not available on this install"
                })),
            )
                .into_response();
        }
    };

    let mut doc: DocumentMut = match template_raw.parse() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(?e, "create_agent: assistant template TOML failed to parse");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "the 'assistant' template is not valid TOML" })),
            )
                .into_response();
        }
    };

    let Some(agent_table) = doc
        .get_mut("agent")
        .and_then(|item| item.as_table_like_mut())
    else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "the 'assistant' template is missing its [agent] table" })),
        )
            .into_response();
    };
    agent_table.insert("name", value(name.clone()));
    agent_table.insert("display_name", value(name.clone()));
    if let Some(desc) = &req.description {
        agent_table.insert("description", value(desc.clone()));
    }

    if let Err(e) = tokio::fs::create_dir_all(&new_pkg).await {
        tracing::warn!(?e, path = %new_pkg.display(), "create_agent: mkdir failed");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "failed to create agent directory" })),
        )
            .into_response();
    }
    if let Err(e) = tokio::fs::write(new_pkg.join("agent.toml"), doc.to_string()).await {
        tracing::warn!(?e, "create_agent: agent.toml write failed");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "failed to write agent.toml" })),
        )
            .into_response();
    }
    let starter_persona = format!(
        "# {name}\n\nReplace this paragraph with {name}'s instructions — how it should talk, \
         what it should prioritize, and any context it should always remember. Starts from the \
         `assistant` template; everything here is this agent's own delta.\n"
    );
    if let Err(e) = tokio::fs::write(new_pkg.join("persona.md"), starter_persona).await {
        tracing::warn!(?e, "create_agent: persona.md write failed");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "failed to write persona.md" })),
        )
            .into_response();
    }

    match parse_agent_toml(&doc.to_string(), &name) {
        Some(v) => (StatusCode::CREATED, Json(v)).into_response(),
        None => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "failed to reload created agent config" })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode as SC;

    fn write_template(dir: &Path) {
        let pkg = dir.join(ASSISTANT_TEMPLATE);
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("agent.toml"),
            "[agent]\nname = \"assistant\"\nrole = \"assistant\"\nmodel = \"anthropic/claude-sonnet-4-6\"\n",
        )
        .unwrap();
        std::fs::write(pkg.join("persona.md"), "Base assistant persona.\n").unwrap();
    }

    #[tokio::test]
    async fn create_agent_writes_package_from_template() {
        let tmp = tempfile::tempdir().unwrap();
        write_template(tmp.path());

        let resp = create_agent_at(
            tmp.path(),
            CreateAgentRequest {
                name: "my-helper".to_string(),
                description: Some("A test helper".to_string()),
                template: None,
            },
        )
        .await;
        assert_eq!(resp.status(), SC::CREATED);

        let toml = std::fs::read_to_string(tmp.path().join("my-helper/agent.toml")).unwrap();
        assert!(toml.contains("name = \"my-helper\""));
        assert!(toml.contains("A test helper"));
        assert!(tmp.path().join("my-helper/persona.md").exists());
    }

    #[tokio::test]
    async fn create_agent_rejects_invalid_slug() {
        let tmp = tempfile::tempdir().unwrap();
        write_template(tmp.path());
        let resp = create_agent_at(
            tmp.path(),
            CreateAgentRequest {
                name: "Not A Slug!".to_string(),
                description: None,
                template: None,
            },
        )
        .await;
        assert_eq!(resp.status(), SC::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_agent_rejects_reserved_name() {
        let tmp = tempfile::tempdir().unwrap();
        write_template(tmp.path());
        let resp = create_agent_at(
            tmp.path(),
            CreateAgentRequest {
                name: "ctrl".to_string(),
                description: None,
                template: None,
            },
        )
        .await;
        assert_eq!(resp.status(), SC::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_agent_rejects_duplicate_name() {
        let tmp = tempfile::tempdir().unwrap();
        write_template(tmp.path());
        std::fs::create_dir_all(tmp.path().join("izzie")).unwrap();

        let resp = create_agent_at(
            tmp.path(),
            CreateAgentRequest {
                name: "izzie".to_string(),
                description: None,
                template: None,
            },
        )
        .await;
        assert_eq!(resp.status(), SC::CONFLICT);
    }

    #[tokio::test]
    async fn create_agent_rejects_unsupported_template() {
        let tmp = tempfile::tempdir().unwrap();
        write_template(tmp.path());
        let resp = create_agent_at(
            tmp.path(),
            CreateAgentRequest {
                name: "my-helper".to_string(),
                description: None,
                template: Some("engineer".to_string()),
            },
        )
        .await;
        assert_eq!(resp.status(), SC::BAD_REQUEST);
    }
}
