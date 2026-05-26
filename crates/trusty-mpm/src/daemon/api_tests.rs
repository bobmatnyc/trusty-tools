use super::*;
use crate::core::session::{ControlModel, Session, SessionStatus};
use axum::http::StatusCode;

fn state_with_session() -> (Arc<DaemonState>, SessionId) {
    let state = DaemonState::shared();
    let id = SessionId::new();
    let mut session = Session::new(id, "/tmp/p", ControlModel::Tmux, None);
    session.status = SessionStatus::Active;
    state.register_session(session);
    (state, id)
}

#[tokio::test]
async fn health_endpoint_responds() {
    assert_eq!(health().await, "ok");
}

#[tokio::test]
async fn current_project_found_and_missing() {
    // `GET /projects/current` returns the project for a registered path
    // and `404` for an unregistered one.
    let state = DaemonState::shared();
    let _ = register_project(
        State(Arc::clone(&state)),
        Json(RegisterProject {
            path: "/work/demo".into(),
        }),
    )
    .await;

    let ok = current_project(
        State(Arc::clone(&state)),
        Query(CurrentProjectQuery {
            path: "/work/demo".into(),
        }),
    )
    .await;
    assert!(ok.is_ok());

    let err = current_project(
        State(state),
        Query(CurrentProjectQuery {
            path: "/work/missing".into(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn discover_projects_returns_array() {
    // `GET /projects/discover` always answers with a (possibly empty) array;
    // it must never error even when `~/.claude/projects/` is absent.
    let state = DaemonState::shared();
    let resp = discover_projects(State(state)).await;
    // The discovered list is well-formed; on CI it is typically empty.
    for project in &resp.0.projects {
        assert!(!project.path.is_empty());
    }
}

#[tokio::test]
async fn register_session_associates_project() {
    // A `POST /sessions` body carrying `project_path` must associate the
    // new session with that project.
    let state = DaemonState::shared();
    let Json(body) = register_session(
        State(Arc::clone(&state)),
        Json(RegisterSession {
            project: "/work/demo".into(),
            project_path: Some("/work/demo".into()),
            name: None,
            workdir: None,
        }),
    )
    .await
    .expect("registration-only path succeeds");
    let id = body.id.0.to_string();
    let listed = state.list_sessions();
    let session = listed
        .iter()
        .find(|s| s.id.0.to_string() == id)
        .expect("session registered");
    assert_eq!(session.project_path, Some(PathBuf::from("/work/demo")));
}

#[tokio::test]
async fn list_sessions_filters_by_project() {
    // `GET /sessions?project=<path>` returns only sessions of that project.
    let state = DaemonState::shared();
    let _ = register_session(
        State(Arc::clone(&state)),
        Json(RegisterSession {
            project: "/work/demo".into(),
            project_path: Some("/work/demo".into()),
            name: None,
            workdir: None,
        }),
    )
    .await;
    let _ = register_session(
        State(Arc::clone(&state)),
        Json(RegisterSession {
            project: "/work/other".into(),
            project_path: Some("/work/other".into()),
            name: None,
            workdir: None,
        }),
    )
    .await;

    let Json(all) = list_sessions(State(Arc::clone(&state)), Query(SessionQuery::default())).await;
    assert_eq!(all.sessions.len(), 2);

    let Json(scoped) = list_sessions(
        State(state),
        Query(SessionQuery {
            project: Some("/work/demo".into()),
        }),
    )
    .await;
    assert_eq!(scoped.sessions.len(), 1);
}

#[tokio::test]
async fn hook_relay_ingests_known_event() {
    let (state, id) = state_with_session();
    let post = HookPost {
        session_id: id.0.to_string(),
        event: HookEvent::PostToolUse,
        payload: serde_json::json!({"tool": "Edit"}),
    };
    let result = ingest_hook(State(state.clone()), Json(post)).await;
    assert!(result.is_ok());
    assert_eq!(state.recent_hook_events().len(), 1);
}

#[tokio::test]
async fn register_and_remove_session() {
    let state = DaemonState::shared();
    let Json(body) = register_session(
        State(state.clone()),
        Json(RegisterSession {
            project: "/tmp/new".into(),
            project_path: None,
            name: None,
            workdir: None,
        }),
    )
    .await
    .expect("registration-only path succeeds");
    let id = body.id.0.to_string();
    assert_eq!(state.list_sessions().len(), 1);
    // Removing it succeeds; removing again is a 404.
    assert!(
        remove_session(State(state.clone()), Path(id.clone()))
            .await
            .is_ok()
    );
    let err = remove_session(State(state), Path(id)).await.unwrap_err();
    assert_eq!(err.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_session_returns_session() {
    // `GET /sessions/{id}` resolves a single session by id and returns its
    // current snapshot so callers don't have to page through `GET /sessions`.
    let (state, id) = state_with_session();
    let Json(session) = get_session(State(state), Path(id.0.to_string()))
        .await
        .expect("known id resolves");
    assert_eq!(session.id, id);
    assert_eq!(session.workdir, "/tmp/p");
}

#[tokio::test]
async fn get_session_unknown_is_404() {
    // An unknown UUID is a 404, matching the rest of the sessions surface.
    let state = DaemonState::shared();
    let err = get_session(State(state), Path(SessionId::new().0.to_string()))
        .await
        .unwrap_err();
    assert_eq!(err.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_session_malformed_id_is_400() {
    // A non-UUID id is a 400 before the lookup runs, mirroring `parse_id`.
    let state = DaemonState::shared();
    let err = get_session(State(state), Path("not-a-uuid".to_string()))
        .await
        .unwrap_err();
    assert_eq!(err.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn connect_session_registers_without_deploy() {
    // `POST /api/v1/sessions/connect` performs the same daemon-side
    // bookkeeping as `POST /sessions` — it registers the session and returns
    // its id and friendly name. The daemon never deploys framework artifacts
    // in either path; the connect/launch distinction lives in the client.
    let state = DaemonState::shared();
    let Json(body) = connect_session(
        State(Arc::clone(&state)),
        Json(RegisterSession {
            project: "/tmp/connect".into(),
            project_path: Some("/tmp/connect".into()),
            name: Some("tmpm-connect".into()),
            workdir: None,
        }),
    )
    .await
    .expect("connect registration succeeds");
    assert_eq!(body.name, "tmpm-connect");
    let listed = state.list_sessions();
    let session = listed
        .iter()
        .find(|s| s.id == body.id)
        .expect("session registered via connect");
    assert_eq!(session.workdir, "/tmp/connect");
}

#[tokio::test]
async fn registered_session_has_friendly_tmux_name() {
    // A registered session must carry a `tmpm-<adj>-<noun>` tmux name
    // derived from its UUID, not the legacy `trusty-mpm-<uuid>` form.
    let state = DaemonState::shared();
    let Json(body) = register_session(
        State(Arc::clone(&state)),
        Json(RegisterSession {
            project: "/tmp/friendly".into(),
            project_path: None,
            name: None,
            workdir: None,
        }),
    )
    .await
    .expect("registration-only path succeeds");
    let id = body.id.0.to_string();
    let listed = state.list_sessions();
    let session = listed
        .iter()
        .find(|s| s.id.0.to_string() == id)
        .expect("session registered");
    assert!(
        session.tmux_name.starts_with("tmpm-"),
        "friendly name: {}",
        session.tmux_name
    );
    assert!(session.tmux_name.len() <= 25);
}

#[tokio::test]
async fn reap_sessions_returns_removed_count() {
    // `DELETE /sessions/dead` always returns a well-formed `{ "removed": N }`
    // body. The exact count depends on whether tmux is installed: with tmux
    // the lone test session (no live tmux session named `tmpm-*`) is reaped
    // (1); without tmux nothing is reaped (0). Either way the registry must
    // not contain a session that is missing from tmux afterwards.
    let (state, _) = state_with_session();
    let Json(body) = reap_sessions(State(Arc::clone(&state))).await;
    let removed = body.removed;
    assert!(removed <= 1, "at most the one test session is reaped");
    assert_eq!(state.list_sessions().len(), 1 - removed);
}

#[tokio::test]
async fn spawn_session_without_claude_returns_422() {
    // `POST /sessions` with a `workdir` opts into spawn mode. When the
    // `claude` binary is unavailable, the handler must return HTTP 422 (and
    // leave the session registry empty — no half-created bookkeeping).
    let _claude = crate::daemon::services::tmux_service::set_claude_lookup_override(Some(None));
    let state = DaemonState::shared();
    let err = register_session(
        State(Arc::clone(&state)),
        Json(RegisterSession {
            project: "/tmp/spawn-no-claude".into(),
            project_path: Some("/tmp/spawn-no-claude".into()),
            name: None,
            workdir: Some("/tmp/spawn-no-claude".into()),
        }),
    )
    .await
    .expect_err("spawn mode without claude must error");
    assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        state.list_sessions().is_empty(),
        "no session should be registered on 422"
    );
}

#[tokio::test]
async fn spawn_session_without_tmux_returns_422_on_no_tmux_host() {
    // Force the `claude` lookup positive so the spawn proceeds past the
    // binary check, then assert that the daemon degrades gracefully when
    // tmux is unavailable. On CI tmux is generally absent, in which case the
    // documented 422 applies. On a developer host that *does* have tmux
    // installed the spawn will either succeed or surface an internal tmux
    // error — both are acceptable shapes; the contract this test enforces
    // is "never panic, and 422 when tmux missing".
    let _claude = crate::daemon::services::tmux_service::set_claude_lookup_override(Some(Some(
        "/fake/claude".into(),
    )));
    let state = DaemonState::shared();
    let outcome = register_session(
        State(Arc::clone(&state)),
        Json(RegisterSession {
            project: "/tmp/spawn-no-tmux".into(),
            project_path: Some("/tmp/spawn-no-tmux".into()),
            name: Some("tmpm-spawn-test-no-tmux".into()),
            workdir: Some("/tmp".into()),
        }),
    )
    .await;
    if crate::daemon::tmux::TmuxDriver::is_available() {
        // On a tmux-equipped host the spawn either succeeds or errors with an
        // internal error from the bogus `claude` path; clean up if it created
        // a real session, then return.
        if let Ok(driver) = crate::daemon::tmux::TmuxDriver::discover() {
            let _ = driver.kill_session("tmpm-spawn-test-no-tmux");
        }
        // Either way the registry must not contain a session for a failed
        // spawn — successful spawns leave one entry; remove it for hygiene.
        for s in state.list_sessions() {
            state.remove_session(s.id);
        }
        let _ = outcome;
    } else {
        let err = outcome.expect_err("spawn mode without tmux must error");
        assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            state.list_sessions().is_empty(),
            "no session should be registered on 422"
        );
    }
}

#[tokio::test]
async fn registration_only_path_ignores_missing_claude() {
    // The bookkeeping (registration-only) path must NOT consult `claude` —
    // an absent binary is irrelevant when no spawn was requested. Forcing
    // the lookup negative proves the field is the sole trigger.
    let _claude = crate::daemon::services::tmux_service::set_claude_lookup_override(Some(None));
    let state = DaemonState::shared();
    let Json(body) = register_session(
        State(Arc::clone(&state)),
        Json(RegisterSession {
            project: "/tmp/no-spawn".into(),
            project_path: None,
            name: None,
            workdir: None,
        }),
    )
    .await
    .expect("registration-only path must succeed regardless of claude availability");
    assert_eq!(state.list_sessions().len(), 1);
    assert_eq!(state.list_sessions()[0].id, body.id);
}

#[tokio::test]
async fn register_session_returns_id_even_without_tmux() {
    // Graceful-degradation invariant: tmux is unavailable in CI, yet
    // `POST /sessions` must still return a JSON body carrying an `id`, and
    // that id must be visible in the subsequent `GET /sessions` snapshot.
    let state = DaemonState::shared();
    let Json(body) = register_session(
        State(Arc::clone(&state)),
        Json(RegisterSession {
            project: "/tmp/no-tmux".into(),
            project_path: None,
            name: None,
            workdir: None,
        }),
    )
    .await
    .expect("registration-only path succeeds");
    let id_str = body.id.0.to_string();
    let listed = state.list_sessions();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id.0.to_string(), id_str);
}

#[tokio::test]
async fn hook_relay_rejects_bad_session_id() {
    let (state, _) = state_with_session();
    let post = HookPost {
        session_id: "not-a-uuid".into(),
        event: HookEvent::Stop,
        payload: serde_json::Value::Null,
    };
    let err = ingest_hook(State(state), Json(post)).await.unwrap_err();
    assert_eq!(err.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn hook_relay_runs_with_disabled_overseer() {
    // With the overseer disabled (the default), a PreToolUse event must
    // still be ingested normally — the overseer fast-path allows it.
    let (state, id) = state_with_session();
    let post = HookPost {
        session_id: id.0.to_string(),
        event: HookEvent::PreToolUse,
        payload: serde_json::json!({"tool": "Bash", "input": {"command": "ls"}}),
    };
    let result = ingest_hook(State(state.clone()), Json(post)).await;
    assert!(result.is_ok());
    assert_eq!(state.recent_hook_events().len(), 1);
}

#[tokio::test]
async fn session_start_auto_registers_unknown_session() {
    // A `SessionStart` hook for a session the daemon has never seen must
    // auto-register it (connection-driven registration), using the incoming
    // UUID so the session carries the right id.
    let state = DaemonState::shared();
    let new_id = crate::core::session::SessionId::new();
    assert!(state.session(new_id).is_none());

    let post = HookPost {
        session_id: new_id.0.to_string(),
        event: HookEvent::SessionStart,
        payload: serde_json::Value::Null,
    };
    let result = ingest_hook(State(state.clone()), Json(post)).await;
    assert!(result.is_ok());

    let registered = state.session(new_id).expect("session auto-registered");
    assert_eq!(registered.id, new_id);
    assert_eq!(
        registered.status,
        crate::core::session::SessionStatus::Active
    );
}

#[tokio::test]
async fn non_session_start_event_does_not_auto_register() {
    // Only `SessionStart` auto-registers. A non-start event for an unknown
    // session must not create a session record.
    let state = DaemonState::shared();
    let unknown = crate::core::session::SessionId::new();
    let post = HookPost {
        session_id: unknown.0.to_string(),
        event: HookEvent::PreToolUse,
        payload: serde_json::json!({"tool": "Bash"}),
    };
    let _ = ingest_hook(State(state.clone()), Json(post)).await;
    assert!(state.session(unknown).is_none());
}

#[tokio::test]
async fn session_start_for_known_session_does_not_duplicate() {
    // A `SessionStart` for an already-registered session must not create a
    // second record.
    let (state, id) = state_with_session();
    let before = state.list_sessions().len();
    let post = HookPost {
        session_id: id.0.to_string(),
        event: HookEvent::SessionStart,
        payload: serde_json::Value::Null,
    };
    let result = ingest_hook(State(state.clone()), Json(post)).await;
    assert!(result.is_ok());
    assert_eq!(state.list_sessions().len(), before);
}

#[tokio::test]
async fn llm_chat_without_overseer_is_503() {
    // A default daemon has no OpenRouter key, so `POST /llm/chat` reports the
    // capability as unavailable rather than attempting a network call.
    let state = DaemonState::shared();
    let err = llm_chat(
        State(state),
        Json(LlmChatRequest {
            message: "hello".into(),
            history: Vec::new(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn coordinator_context_returns_snapshot() {
    // `GET /api/v1/coordinator/context` always returns a snapshot; with a
    // registered session it appears in the `sessions` array.
    let (state, _id) = state_with_session();
    let snapshot = coordinator_context(State(state)).await;
    assert_eq!(snapshot.sessions.len(), 1);
}

#[tokio::test]
async fn coordinator_chat_without_overseer_is_503() {
    // A non-prefixed coordinator message needs the LLM; a default daemon has
    // no key, so the chat endpoint reports the capability unavailable.
    let state = DaemonState::shared();
    let err = coordinator_chat(
        State(state),
        Json(CoordinatorChatRequest {
            message: "what is happening?".into(),
            history: Vec::new(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn coordinator_chat_routes_prefixed_message() {
    // A `@prefix:` message routes directly to the session's tmux pane and
    // never touches the LLM, so it succeeds even with no API key configured.
    let state = DaemonState::shared();
    let id = SessionId::new();
    let mut session = Session::new(id, "/tmp/p", ControlModel::Tmux, None);
    session.status = SessionStatus::Active;
    session.tmux_name = "tmpm-coordtest".to_string();
    state.register_session(session);

    let resp = coordinator_chat(
        State(state),
        Json(CoordinatorChatRequest {
            message: "@coordtest: echo hi".into(),
            history: Vec::new(),
        }),
    )
    .await
    .expect("prefixed routing must not require an LLM");
    assert_eq!(resp.routed_to_session.as_deref(), Some("tmpm-coordtest"));
    assert!(resp.command_output.is_some());
}

#[tokio::test]
async fn openapi_spec_is_valid() {
    // `GET /api-docs/openapi.json` must return 200 with a document that
    // carries the `openapi` version key and the daemon's title.
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    let app = router(DaemonState::shared());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api-docs/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let spec: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        spec.get("openapi").is_some(),
        "spec must have an openapi key"
    );
    assert!(
        spec["info"]["title"]
            .as_str()
            .unwrap_or_default()
            .contains("trusty-mpm"),
        "spec title must mention trusty-mpm"
    );
}

#[tokio::test]
async fn pause_then_resume_round_trips() {
    // Pausing flips a session to `Paused`; resuming flips it back to
    // `Active` and clears the pause metadata.
    let (state, id) = state_with_session();
    let Json(body) = pause_session(
        State(Arc::clone(&state)),
        Path(id.0.to_string()),
        Json(PauseRequest {
            summary: Some("mid-task".into()),
        }),
    )
    .await
    .expect("pause succeeds");
    assert!(body.paused);
    assert_eq!(body.summary, "mid-task");

    let paused = state.session(id).expect("session exists");
    assert_eq!(paused.status, SessionStatus::Paused);
    assert_eq!(paused.pause_summary.as_deref(), Some("mid-task"));
    assert!(paused.paused_at.is_some());

    let Json(resumed) = resume_session(State(Arc::clone(&state)), Path(id.0.to_string()))
        .await
        .expect("resume succeeds");
    assert!(resumed.resumed);

    let active = state.session(id).expect("session exists");
    assert_eq!(active.status, SessionStatus::Active);
    assert_eq!(active.paused_at, None);
    assert_eq!(active.pause_summary, None);
}

#[tokio::test]
async fn pause_unknown_session_is_404() {
    let state = DaemonState::shared();
    let err = pause_session(
        State(state),
        Path(SessionId::new().0.to_string()),
        Json(PauseRequest::default()),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn resume_unpaused_session_is_409() {
    // A session that was never paused cannot be resumed.
    let (state, id) = state_with_session();
    let err = resume_session(State(state), Path(id.0.to_string()))
        .await
        .unwrap_err();
    assert_eq!(err.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn command_to_stopped_session_is_409() {
    let state = DaemonState::shared();
    let id = SessionId::new();
    let mut session = Session::new(id, "/tmp/p", ControlModel::Tmux, None);
    session.status = SessionStatus::Stopped;
    state.register_session(session);

    let err = send_command(
        State(state),
        Path(id.0.to_string()),
        Query(CommandQuery::default()),
        Json(CommandRequest {
            command: "help".into(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn output_unknown_session_is_404() {
    let state = DaemonState::shared();
    let err = get_output(
        State(state),
        Path(SessionId::new().0.to_string()),
        Query(OutputQuery::default()),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn pause_resolves_session_by_friendly_name() {
    // The pause endpoint accepts a friendly tmux name, not just a UUID.
    let (state, id) = state_with_session();
    let name = state.session(id).expect("session").tmux_name;
    let Json(body) = pause_session(
        State(Arc::clone(&state)),
        Path(name),
        Json(PauseRequest::default()),
    )
    .await
    .expect("pause by name succeeds");
    assert!(body.paused);
}

#[test]
fn send_command_compress_query_defaults_off() {
    // A `CommandQuery` with no `compress` field deserializes to `None`, so
    // omitting `?compress=` defaults to no compression.
    let query: CommandQuery = serde_json::from_str("{}").expect("empty query deserializes");
    assert_eq!(query.compress, None);
}

#[test]
fn output_query_defaults() {
    // An `OutputQuery` with no fields set has neither a line count nor a
    // compression level.
    let query: OutputQuery = serde_json::from_str("{}").expect("empty query deserializes");
    assert_eq!(query.lines, None);
    assert_eq!(query.compress, None);
}

#[test]
fn compress_level_roundtrips_serde() {
    // `CompressionLevel::Summarise` serializes to the lowercase wire name
    // `"summarise"` and deserializes back to the same variant.
    let json = serde_json::to_string(&CompressionLevel::Summarise).expect("serialize");
    assert_eq!(json, "\"summarise\"");
    let parsed: CompressionLevel = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed, CompressionLevel::Summarise);
}

#[test]
fn compress_level_label_matches_serde() {
    // The lowercase label helper agrees with serde's wire representation.
    assert_eq!(compression_level_label(CompressionLevel::Off), "off");
    assert_eq!(compression_level_label(CompressionLevel::Trim), "trim");
    assert_eq!(
        compression_level_label(CompressionLevel::Summarise),
        "summarise"
    );
    assert_eq!(
        compression_level_label(CompressionLevel::Caveman),
        "caveman"
    );
}

#[test]
fn apply_compression_off_is_passthrough() {
    // With no level, the text is returned unchanged and there is no label.
    let result = apply_compression(None, "raw pane text");
    assert_eq!(result.text, "raw pane text");
    assert_eq!(result.level_label, None);
}

#[test]
fn apply_compression_summarise() {
    // With a level set, the label is recorded and stats reflect the input.
    let raw = "x".repeat(100);
    let result = apply_compression(Some(CompressionLevel::Summarise), &raw);
    assert_eq!(result.level_label.as_deref(), Some("summarise"));
    assert_eq!(result.stats.original_bytes, 100);
}

#[tokio::test]
async fn adopt_tmux_session_handles_missing() {
    // Adopting a session that does not exist (or with tmux absent) is 404.
    let state = DaemonState::shared();
    let result = adopt_tmux_session(
        State(state),
        Json(AdoptRequest {
            session: "trusty-mpm-no-such-session-xyz".into(),
        }),
    )
    .await;
    assert_eq!(result.unwrap_err().status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn tmux_snapshot_unknown_session_is_404() {
    let state = DaemonState::shared();
    let result = tmux_snapshot(State(state), Path("no-such-session-xyz".into())).await;
    assert_eq!(result.unwrap_err().status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_checkpoint_returns_id() {
    // `POST /claude-config/checkpoints` returns an `id` and the checkpoint
    // is then visible via the list endpoint.
    let dir = tempfile::tempdir().unwrap();
    let state = DaemonState::shared();
    let Json(body) = create_checkpoint(
        State(Arc::clone(&state)),
        Json(CreateCheckpointRequest {
            project: dir.path().to_path_buf(),
            label: Some("manual".into()),
        }),
    )
    .await
    .expect("create succeeds");
    assert!(!body.id.is_empty());

    let Json(listed) = list_checkpoints(
        State(state),
        Query(CheckpointQuery {
            project: dir.path().to_path_buf(),
        }),
    )
    .await;
    assert_eq!(listed.checkpoints.len(), 1);
}

#[tokio::test]
async fn restore_unknown_checkpoint_is_500() {
    let dir = tempfile::tempdir().unwrap();
    let state = DaemonState::shared();
    let err = restore_checkpoint(
        State(state),
        Json(RestoreRequest {
            project: dir.path().to_path_buf(),
            checkpoint_id: "no-such-checkpoint".into(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn delete_unknown_checkpoint_is_404() {
    let dir = tempfile::tempdir().unwrap();
    let state = DaemonState::shared();
    let err = delete_checkpoint(
        State(state),
        Path("no-such-checkpoint".into()),
        Query(CheckpointQuery {
            project: dir.path().to_path_buf(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn deploy_profile_returns_checkpoint_id() {
    // `POST /claude-config/deploy` deploys a built-in profile and returns a
    // checkpoint id for undo.
    let dir = tempfile::tempdir().unwrap();
    let state = DaemonState::shared();
    let Json(body) = deploy_profile(
        State(state),
        Json(DeployProfileRequest {
            project: dir.path().to_path_buf(),
            profile_name: "minimal".into(),
            target: None,
        }),
    )
    .await
    .expect("deploy succeeds");
    assert_eq!(body.deployed, "minimal");
    assert!(!body.checkpoint_id.is_empty());
}

#[tokio::test]
async fn deploy_unknown_profile_is_404() {
    let dir = tempfile::tempdir().unwrap();
    let state = DaemonState::shared();
    let err = deploy_profile(
        State(state),
        Json(DeployProfileRequest {
            project: dir.path().to_path_buf(),
            profile_name: "no-such-profile".into(),
            target: None,
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn pair_confirm_rejects_bad_code() {
    // A code that was never issued must not pair the daemon. The state is
    // rooted at a temp dir so it ignores any real persisted pairing on disk.
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(DaemonState::with_root(dir.path().to_path_buf()));
    let _ = pair_request(State(Arc::clone(&state))).await;
    let Json(confirm) = pair_confirm(
        State(Arc::clone(&state)),
        Json(PairConfirmRequest {
            code: "ZZZZZZ".into(),
            chat_id: 777,
        }),
    )
    .await;
    assert!(!confirm.success);
    assert!(confirm.error.as_deref().unwrap().contains("invalid"));

    let Json(status) = pair_status(State(state)).await;
    assert!(!status.paired);
    assert!(status.chat_id.is_none());
}

#[tokio::test]
async fn discover_sessions_returns_count() {
    // `POST /sessions/discover` returns a well-formed count; with tmux absent
    // (or no Claude panes) on CI it is zero, but the shape must be correct.
    let state = DaemonState::shared();
    let Json(resp) = discover_sessions(State(state)).await;
    assert_eq!(resp.discovered, resp.sessions.len());
}

#[tokio::test]
async fn pair_reset_clears_pairing() {
    // `POST /pair/reset` always reports `reset: true` and leaves the daemon
    // unpaired. The state is rooted at a temp dir so no disk write touches HOME.
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(DaemonState::with_root(dir.path().to_path_buf()));
    let Json(resp) = pair_reset(State(Arc::clone(&state))).await;
    assert!(resp.reset);
    let Json(status) = pair_status(State(state)).await;
    assert!(!status.paired);
}

#[tokio::test]
async fn doctor_endpoint_returns_report() {
    // `GET /api/v1/doctor` always returns a five-check report; the per-check
    // statuses carry the diagnosis, not the HTTP status.
    let state = DaemonState::shared();
    let Json(report) = doctor(State(state), Query(DoctorQuery::default())).await;
    assert_eq!(report.checks.len(), 5);
    let names: Vec<&str> = report.checks.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        ["instructions", "agents", "skills", "memory", "search"]
    );
}

#[tokio::test]
async fn apply_claude_config_unknown_rec_is_404() {
    let dir = tempfile::tempdir().unwrap();
    let state = DaemonState::shared();
    let result = apply_claude_config(
        State(state),
        Json(ApplyConfigRequest {
            project: dir.path().to_path_buf(),
            recommendation_id: "no-such-recommendation".into(),
        }),
    )
    .await;
    assert_eq!(result.unwrap_err(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn ingest_hook_broadcasts_to_subscribers() {
    // `POST /hooks` must publish the event onto the broadcast channel so the
    // SSE handlers can stream it to live subscribers. Subscribing first
    // guarantees the receiver sees the publish.
    let (state, id) = state_with_session();
    let mut rx = state.event_subscribe();

    let post = HookPost {
        session_id: id.0.to_string(),
        event: HookEvent::PostToolUse,
        payload: serde_json::json!({"tool": "Edit"}),
    };
    let result = ingest_hook(State(state.clone()), Json(post)).await;
    assert!(result.is_ok());

    let received = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("broadcast arrived within 1s")
        .expect("broadcast value is Ok");
    assert_eq!(received["event"], serde_json::json!("PostToolUse"));
    assert_eq!(received["session"], serde_json::json!(id.0.to_string()));
}

#[tokio::test]
async fn events_sse_streams_one_frame() {
    // The new `GET /events` SSE handler subscribes to the broadcast channel
    // and writes each event as one `data:` line. Driving the live router via
    // `tower::oneshot` and reading the response body confirms the wire
    // contract.
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let (state, id) = state_with_session();
    let app = router(Arc::clone(&state));

    // Kick off the SSE request first so the handler subscribes *before* we
    // publish; otherwise the broadcast value is dropped on the floor.
    let response = app
        .oneshot(
            Request::builder()
                .uri("/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        content_type.starts_with("text/event-stream"),
        "expected SSE content-type, got {content_type:?}"
    );

    // Now publish a hook event after the handler is connected.
    state
        .clone()
        .push_hook_event(crate::core::hook::HookEventRecord::now(
            id,
            HookEvent::PostToolUse,
            serde_json::json!({"tool": "Edit"}),
        ));

    // Read one frame of body bytes. The frame must contain the JSON-encoded
    // event on a `data:` line. A 2-second timeout keeps a regression from
    // hanging the test runner.
    let mut body = response.into_body();
    let bytes = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let frame = body.frame().await.expect("body has a frame")?;
            if let Ok(data) = frame.into_data()
                && !data.is_empty()
            {
                return Ok::<_, axum::Error>(data);
            }
        }
    })
    .await
    .expect("SSE frame arrived within 2s")
    .expect("frame read ok");

    let text = std::str::from_utf8(&bytes).expect("utf8");
    assert!(
        text.contains("data:"),
        "expected an SSE `data:` line, got {text:?}"
    );
    assert!(
        text.contains("PostToolUse"),
        "expected event payload in frame, got {text:?}"
    );
    assert!(
        text.contains(&id.0.to_string()),
        "expected session id in frame, got {text:?}"
    );
}

#[tokio::test]
async fn session_events_sse_filters_by_session() {
    // `GET /sessions/{id}/events` must only forward events for that session,
    // dropping events for unrelated sessions. Publishing one event for the
    // subscribed session and one for an unrelated session and confirming only
    // the first arrives proves the filter is in effect.
    let (state, id) = state_with_session();
    let other = SessionId::new();
    let mut other_session =
        crate::core::session::Session::new(other, "/tmp/other", ControlModel::Tmux, None);
    other_session.status = SessionStatus::Active;
    state.register_session(other_session);

    let stream_response =
        stream_session_events(Path(id.0.to_string()), State(Arc::clone(&state))).await;
    // Consume the `Sse<...>` to a real HTTP response so we can read frames.
    use axum::response::IntoResponse;
    let response = stream_response.into_response();
    assert_eq!(response.status(), StatusCode::OK);

    // Publish: one for the other session (must be filtered out), then one for
    // the subscribed session.
    state.push_hook_event(crate::core::hook::HookEventRecord::now(
        other,
        HookEvent::PostToolUse,
        serde_json::json!({"tool": "Read"}),
    ));
    state.push_hook_event(crate::core::hook::HookEventRecord::now(
        id,
        HookEvent::PostToolUse,
        serde_json::json!({"tool": "Edit"}),
    ));

    use http_body_util::BodyExt;
    let mut body = response.into_body();
    let bytes = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let frame = body.frame().await.expect("body has a frame")?;
            if let Ok(data) = frame.into_data()
                && !data.is_empty()
            {
                return Ok::<_, axum::Error>(data);
            }
        }
    })
    .await
    .expect("SSE frame arrived within 2s")
    .expect("frame read ok");

    let text = std::str::from_utf8(&bytes).expect("utf8");
    // The first non-empty data frame must be the *subscribed* session's event,
    // not the unrelated one.
    assert!(
        text.contains(&id.0.to_string()),
        "expected subscribed session id in frame, got {text:?}"
    );
    assert!(
        !text.contains(&other.0.to_string()),
        "unrelated session id leaked into stream: {text:?}"
    );
}
