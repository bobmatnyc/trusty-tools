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
//! `assistant` template package (`<dir>/assistant/{agent.toml,persona.md}`,
//! searched across [`crate::agents::agents_dir_candidates`]'s tiers —
//! project-local then `$HOME/.trusty-agents/agents`, the same precedence
//! `GET`/`PATCH /api/agents/:name` use, critical for the packaged desktop
//! app where the Tauri sidecar's cwd is `/` and every bundled template
//! lives in the `$HOME` tier, never a nonexistent project-local one — the
//! same template every bundled install ships), and writes a new package
//! directory INTO THAT SAME TIER (co-located with the template it copied)
//! with `name`/`display_name`/`description` substituted into the copied
//! `agent.toml`, plus an empty starter `persona.md`. Rejects a name that
//! already exists (directory or flat-file collision, checked across every
//! tier) or isn't a valid slug.
//! Test: `super::tests::agent_create::*`.

use std::path::PathBuf;

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use toml_edit::{DocumentMut, value};

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
    create_agent_at(&crate::agents::agents_dir_candidates(), req).await
}

/// Core creation logic against explicit candidate agent directories.
///
/// Why: Same testability rationale as `patch_agent_at`/`list_workstreams_at`.
/// What: See module doc for the full validation + write sequence.
/// Test: `super::tests::agent_create::*`.
pub(super) async fn create_agent_at(dirs: &[PathBuf], req: CreateAgentRequest) -> Response {
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

    if dirs
        .iter()
        .any(|dir| dir.join(&name).exists() || dir.join(format!("{name}.toml")).exists())
    {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": format!("agent '{name}' already exists") })),
        )
            .into_response();
    }

    // Find the FIRST tier that actually has the assistant template, and
    // write the new agent's package into that same tier (co-located with
    // what it was templated from) — see module doc for why a single
    // cwd-relative directory can't be assumed to exist.
    let Some((dir, template_raw)) = ({
        let mut found = None;
        for dir in dirs {
            let template_toml_path = dir.join(ASSISTANT_TEMPLATE).join("agent.toml");
            if let Ok(s) = tokio::fs::read_to_string(&template_toml_path).await {
                found = Some((dir.clone(), s));
                break;
            }
        }
        found
    }) else {
        tracing::warn!(
            dirs = ?dirs,
            "create_agent: assistant template unreadable in every candidate directory"
        );
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "the 'assistant' template is not available on this install"
            })),
        )
            .into_response();
    };
    let new_pkg = dir.join(&name);

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
    // #3819 demo-build fix: a newly-created agent must always start
    // VISIBLE, regardless of what `hidden` value the template it was
    // copied from happens to carry (the demo sets `hidden = true` on
    // `assistant` itself — copying that verbatim would make every agent
    // created from it invisible in the very picker the add-agent flow is
    // supposed to populate).
    agent_table.insert("hidden", value(false));

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
    use std::path::Path;

    fn write_template(dir: &Path) {
        write_template_with_toml(
            dir,
            "[agent]\nname = \"assistant\"\nrole = \"assistant\"\nmodel = \"anthropic/claude-sonnet-4-6\"\n",
        );
    }

    fn write_template_with_toml(dir: &Path, agent_toml: &str) {
        let pkg = dir.join(ASSISTANT_TEMPLATE);
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("agent.toml"), agent_toml).unwrap();
        std::fs::write(pkg.join("persona.md"), "Base assistant persona.\n").unwrap();
    }

    /// Regression guard (demo-build fix): a new agent must be VISIBLE even
    /// when copied from a template that has `hidden = true` (the demo sets
    /// this on `assistant` itself to trim the picker roster) — otherwise
    /// every agent the add-agent flow creates would vanish from the very
    /// picker it's meant to populate.
    #[tokio::test]
    async fn create_agent_is_visible_even_when_template_is_hidden() {
        let tmp = tempfile::tempdir().unwrap();
        write_template_with_toml(
            tmp.path(),
            "[agent]\nname = \"assistant\"\nrole = \"assistant\"\nhidden = true\n",
        );

        let resp = create_agent_at(
            &[tmp.path().to_path_buf()],
            CreateAgentRequest {
                name: "my-helper".to_string(),
                description: None,
                template: None,
            },
        )
        .await;
        assert_eq!(resp.status(), SC::CREATED);
        let bytes = axum::body::to_bytes(resp.into_body(), 8 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["hidden"], false);
    }

    #[tokio::test]
    async fn create_agent_writes_package_from_template() {
        let tmp = tempfile::tempdir().unwrap();
        write_template(tmp.path());

        let resp = create_agent_at(
            &[tmp.path().to_path_buf()],
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

    /// Regression guard (#3819 demo-build fix): the template must be found
    /// across MULTIPLE tiers, and the new agent must be written INTO the
    /// tier where the template was actually found — not the first
    /// (possibly nonexistent, e.g. cwd `/` in the packaged app) tier.
    #[tokio::test]
    async fn create_agent_finds_template_in_second_tier_and_writes_there() {
        let primary = tempfile::tempdir().unwrap(); // no template here
        let home = tempfile::tempdir().unwrap();
        write_template(home.path());

        let dirs = vec![primary.path().to_path_buf(), home.path().to_path_buf()];
        let resp = create_agent_at(
            &dirs,
            CreateAgentRequest {
                name: "my-helper".to_string(),
                description: None,
                template: None,
            },
        )
        .await;
        assert_eq!(resp.status(), SC::CREATED);
        assert!(home.path().join("my-helper/agent.toml").exists());
        assert!(
            !primary.path().join("my-helper").exists(),
            "must not create in a tier that had no template"
        );
    }

    #[tokio::test]
    async fn create_agent_rejects_invalid_slug() {
        let tmp = tempfile::tempdir().unwrap();
        write_template(tmp.path());
        let resp = create_agent_at(
            &[tmp.path().to_path_buf()],
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
            &[tmp.path().to_path_buf()],
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
            &[tmp.path().to_path_buf()],
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
            &[tmp.path().to_path_buf()],
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
