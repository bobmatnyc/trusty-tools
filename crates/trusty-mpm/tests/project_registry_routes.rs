//! End-to-end HTTP tests for the registry-B project routes (#2114/#2115) and the
//! Deliverable/Milestone client methods (#2381), driven through the REAL
//! `DaemonClient` against the REAL `api::router` on a loopback port.
//!
//! Why: the in-crate unit tests cover the pure pieces (wire-shape serde in
//! `client::http_client::{projects,deliverables}::tests`, the daemon handlers'
//! request DTOs). This file is the literal proof that the CLI's client methods
//! and the daemon's new routes agree over the wire — `registry_register_project`
//! → `registry_get_project` → `registry_list_projects` → `project_status`
//! round-trips, the §10.3 illegal-transition 409 surfaces as
//! `SetStatusError::Rejected` carrying the legal next states, and the
//! Deliverable/Milestone CRUD path works through the client. Mirrors the harness
//! in `tests/project_status_route.rs` (axum::serve on an ephemeral port +
//! reqwest), so no external daemon is needed in CI.
//! What: binds the router once per test, points a `DaemonClient` at it, and drives
//! the full verb surface.
//! Test: this file IS the test; run with
//! `cargo test -p trusty-mpm --test project_registry_routes`.

use std::future::IntoFuture;
use std::sync::Arc;

use trusty_mpm::client::DaemonClient;
use trusty_mpm::client::http_client::deliverables::{
    CreateDeliverableArgs, CreateMilestoneArgs, SetStatusError,
};
use trusty_mpm::client::http_client::projects::{PatchProjectArgs, RegisterProjectArgs};
use trusty_mpm::core::trusty_tools_config::GithubConfig;
use trusty_mpm::daemon::{api, state::DaemonState};
use trusty_mpm::deliverable::{DeliverableKind, DeliverableStatus, EstimationTier};
use trusty_mpm::project::Project;

/// Bind the real router on an ephemeral loopback port; return a client plus the
/// backing [`DaemonState`] (some tests need to seed the store directly, bypassing
/// HTTP, to set up state the HTTP request body cannot express — e.g. the
/// per-project `github`/commit identity binding, #2184).
async fn serve_with_state() -> (DaemonClient, Arc<DaemonState>) {
    let root = tempfile::tempdir().unwrap().keep();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root).await);
    let router = api::router(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, router).into_future());
    (DaemonClient::new(format!("http://{addr}")), state)
}

/// Bind the real router on an ephemeral loopback port; return a client for it.
async fn serve() -> DaemonClient {
    serve_with_state().await.0
}

/// Bind the real router on an ephemeral loopback port; return a client for it
/// plus the plain `http://host:port` base URL (needed by the one test that
/// must send a raw wire body the typed [`DaemonClient`] methods cannot
/// express — `DaemonClient`'s `base` field is crate-private).
async fn serve_with_base() -> (DaemonClient, String) {
    let root = tempfile::tempdir().unwrap().keep();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root).await);
    let router = api::router(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, router).into_future());
    let base = format!("http://{addr}");
    (DaemonClient::new(base.clone()), base)
}

/// register → get → list → status round-trips over real HTTP.
#[tokio::test]
async fn register_get_list_status_round_trip() {
    let client = serve().await;

    let args = RegisterProjectArgs {
        name: "widget".into(),
        repo_url: "https://github.com/acme/widget".into(),
        default_branch: Some("develop".into()),
        description: Some("the widget".into()),
        tags: Some(vec!["backend".into()]),
        stack_hint: Some("rust".into()),
        gh_user: Some("acme-bot".into()),
        gh_account: None,
    };
    let registered = client.registry_register_project(&args).await.unwrap();
    assert_eq!(registered.name, "widget");
    assert_eq!(registered.default_branch, "develop");
    assert_eq!(registered.gh_user.as_deref(), Some("acme-bot"));

    let got = client.registry_get_project("widget").await.unwrap();
    assert_eq!(got, registered);

    let all = client.registry_list_projects(None).await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].name, "widget");

    // Tag filter keeps the match and drops non-matches.
    assert_eq!(
        client
            .registry_list_projects(Some("backend"))
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        client
            .registry_list_projects(Some("nope"))
            .await
            .unwrap()
            .len(),
        0
    );

    // Status rollup: no sessions yet, gh_user flag set.
    let status = client.project_status("widget").await.unwrap();
    assert_eq!(status.project_name, "widget");
    assert_eq!(status.sessions.total, 0);
    assert!(status.config.gh_user_set);
    assert!(status.last_activity_at.is_none());
}

/// A re-register (upsert) preserves an existing per-project identity binding that
/// the register body cannot express (parity with `project_register`).
#[tokio::test]
async fn register_is_idempotent_upsert() {
    let client = serve().await;
    let base = RegisterProjectArgs {
        name: "widget".into(),
        repo_url: "https://github.com/acme/widget".into(),
        default_branch: None,
        description: None,
        tags: None,
        stack_hint: None,
        gh_user: None,
        gh_account: None,
    };
    client.registry_register_project(&base).await.unwrap();
    // Re-register with a different branch — upsert on name, not a duplicate.
    let mut updated = base.clone();
    updated.default_branch = Some("release".into());
    let out = client.registry_register_project(&updated).await.unwrap();
    assert_eq!(out.default_branch, "release");
    assert_eq!(client.registry_list_projects(None).await.unwrap().len(), 1);
}

/// #3025 review follow-up item 4 (MEDIUM — "no safe update path"): before this
/// fix, re-registering an existing project with ONLY `gh_account` set (the
/// only way to pin an account without knowing the PATCH API) silently wiped
/// `stack_hint`/`tags`/`description`/`gh_user` back to absent because the
/// route replaced the whole record from the body. Every field the second
/// request OMITS must now survive.
#[tokio::test]
async fn register_preserves_unspecified_optional_fields_on_existing_project() {
    let client = serve().await;
    client
        .registry_register_project(&RegisterProjectArgs {
            name: "widget".into(),
            repo_url: "https://github.com/acme/widget".into(),
            default_branch: None,
            description: Some("the widget project".into()),
            tags: Some(vec!["backend".into(), "oss".into()]),
            stack_hint: Some("rust".into()),
            gh_user: Some("bobmatnyc".into()),
            gh_account: None,
        })
        .await
        .expect("initial register");

    // Second register carries ONLY `gh_account` — every other optional field
    // is omitted and must be preserved, not wiped.
    let updated = client
        .registry_register_project(&RegisterProjectArgs {
            name: "widget".into(),
            repo_url: "https://github.com/acme/widget".into(),
            default_branch: None,
            description: None,
            tags: None,
            stack_hint: None,
            gh_user: None,
            gh_account: Some("bob-work".into()),
        })
        .await
        .expect("gh_account-only re-register");

    assert_eq!(updated.gh_account.as_deref(), Some("bob-work"));
    assert_eq!(
        updated.description.as_deref(),
        Some("the widget project"),
        "description must survive an omitting re-register"
    );
    assert_eq!(
        updated.tags,
        vec!["backend".to_string(), "oss".to_string()],
        "tags must survive an omitting re-register"
    );
    assert_eq!(
        updated.stack_hint.as_deref(),
        Some("rust"),
        "stack_hint must survive an omitting re-register"
    );
    assert_eq!(
        updated.gh_user.as_deref(),
        Some("bobmatnyc"),
        "gh_user must survive an omitting re-register"
    );
}

/// The flip side of the merge fix: a field the request DOES carry must still
/// override the existing value — merge-with-existing must never become
/// merge-that-ignores-new-values.
#[tokio::test]
async fn register_explicit_fields_still_override_existing() {
    let client = serve().await;
    client
        .registry_register_project(&RegisterProjectArgs {
            name: "widget".into(),
            repo_url: "https://github.com/acme/widget".into(),
            default_branch: None,
            description: Some("old description".into()),
            tags: Some(vec!["backend".into()]),
            stack_hint: Some("rust".into()),
            gh_user: Some("bobmatnyc".into()),
            gh_account: Some("bobmatnyc".into()),
        })
        .await
        .expect("initial register");

    let updated = client
        .registry_register_project(&RegisterProjectArgs {
            name: "widget".into(),
            repo_url: "https://github.com/acme/widget".into(),
            default_branch: None,
            description: Some("new description".into()),
            tags: Some(vec!["frontend".into()]),
            stack_hint: Some("python".into()),
            gh_user: Some("bob-work".into()),
            gh_account: Some("bob-work".into()),
        })
        .await
        .expect("overriding re-register");

    assert_eq!(updated.description.as_deref(), Some("new description"));
    assert_eq!(updated.tags, vec!["frontend".to_string()]);
    assert_eq!(updated.stack_hint.as_deref(), Some("python"));
    assert_eq!(updated.gh_user.as_deref(), Some("bob-work"));
    assert_eq!(updated.gh_account.as_deref(), Some("bob-work"));
}

/// The highest-risk clobber path: a project already carries a per-project
/// `github`/commit identity binding (#2184, set out-of-band — e.g. via
/// `seed_from_config` or a direct `registry.register` call, since the HTTP
/// register body has no fields for them). Re-registering that SAME project
/// through `POST /api/v1/projects` — whose body cannot express `github`/
/// `commit_name`/`commit_email` — must NOT wipe the binding; the route
/// preserves it by reading the existing record before building the replacement
/// (see `register_project_registry_route`'s `existing.as_ref().and_then(...)`
/// carry-forward).
#[tokio::test]
async fn register_preserves_identity_binding_not_expressible_in_body() {
    let (client, state) = serve_with_state().await;

    // Seed directly on the store (bypassing HTTP) with a full identity binding —
    // this is state the register HTTP body has no way to set.
    state
        .project_registry()
        .await
        .register(Project {
            name: "widget".into(),
            repo_url: "https://github.com/acme/widget".into(),
            default_branch: "main".into(),
            stack_hint: None,
            tags: vec![],
            description: None,
            gh_user: None,
            gh_account: None,
            github: Some(GithubConfig {
                config_dir: Some("/home/bob/.config/gh-work".into()),
                token_env: None,
                account: None,
                host: None,
            }),
            commit_name: Some("Bob".into()),
            commit_email: Some("bob@example.com".into()),
        })
        .await
        .expect("seed project with identity binding");

    // Re-register the SAME project over HTTP, changing only the branch — the
    // body cannot carry `github`/`commit_name`/`commit_email` at all.
    let updated = client
        .registry_register_project(&RegisterProjectArgs {
            name: "widget".into(),
            repo_url: "https://github.com/acme/widget".into(),
            default_branch: Some("develop".into()),
            description: None,
            tags: None,
            stack_hint: None,
            gh_user: None,
            gh_account: None,
        })
        .await
        .unwrap();

    assert_eq!(
        updated.default_branch, "develop",
        "the field the request DID carry must still apply"
    );
    assert_eq!(
        updated.commit_name.as_deref(),
        Some("Bob"),
        "commit_name must survive a re-register the body cannot express it in"
    );
    assert_eq!(
        updated.commit_email.as_deref(),
        Some("bob@example.com"),
        "commit_email must survive a re-register the body cannot express it in"
    );
    let github = updated
        .github
        .as_ref()
        .expect("github binding must survive the re-register");
    assert_eq!(
        github.config_dir.as_deref(),
        Some(std::path::Path::new("/home/bob/.config/gh-work")),
        "github.config_dir must survive unchanged"
    );

    // The persisted record (read back independently of the register response)
    // must show the same preserved binding — proving it is durable, not just an
    // artifact of the immediate response body.
    let refetched = client.registry_get_project("widget").await.unwrap();
    assert_eq!(refetched.commit_name.as_deref(), Some("Bob"));
    assert_eq!(refetched.commit_email.as_deref(), Some("bob@example.com"));
    assert!(refetched.github.is_some());
}

/// Fetching an unregistered project surfaces an error (the daemon 404s).
#[tokio::test]
async fn get_unknown_project_errors() {
    let client = serve().await;
    assert!(client.registry_get_project("nope").await.is_err());
}

// ───────────────────────── PATCH /api/v1/projects/{name} (#2114) ──────────

/// register → patch → get round-trips: every mutable field type (required
/// string, clearable optional, tag add/remove) is reflected by a follow-up
/// `GET`, not just the PATCH response.
#[tokio::test]
async fn patch_round_trip_updates_fields() {
    let client = serve().await;
    client
        .registry_register_project(&RegisterProjectArgs {
            name: "widget".into(),
            repo_url: "https://github.com/acme/widget".into(),
            default_branch: Some("main".into()),
            description: Some("original".into()),
            tags: Some(vec!["backend".into(), "keep-me".into()]),
            stack_hint: Some("rust".into()),
            gh_user: Some("acme-bot".into()),
            gh_account: None,
        })
        .await
        .unwrap();

    let patched = client
        .registry_patch_project(
            "widget",
            &PatchProjectArgs {
                default_branch: Some("develop".into()),
                description: Some(Some("updated".into())),
                stack_hint: Some(None), // clear
                tags_add: Some(vec!["ml".into()]),
                tags_remove: Some(vec!["backend".into()]),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(patched.default_branch, "develop");
    assert_eq!(patched.description.as_deref(), Some("updated"));
    assert_eq!(patched.stack_hint, None, "explicit null must clear");
    assert!(patched.tags.contains(&"ml".to_string()));
    assert!(patched.tags.contains(&"keep-me".to_string()));
    assert!(!patched.tags.contains(&"backend".to_string()));
    // gh_user was not touched by this PATCH — must survive unchanged.
    assert_eq!(patched.gh_user.as_deref(), Some("acme-bot"));

    // A follow-up GET, independent of the PATCH response, shows the same state.
    let refetched = client.registry_get_project("widget").await.unwrap();
    assert_eq!(refetched, patched);
}

/// Fields absent from the PATCH body are left completely untouched, including
/// fields that could look ambiguous (an empty `tags_add`/`tags_remove` array
/// vs. the key being absent entirely).
#[tokio::test]
async fn patch_absent_fields_untouched() {
    let client = serve().await;
    client
        .registry_register_project(&RegisterProjectArgs {
            name: "widget".into(),
            repo_url: "https://github.com/acme/widget".into(),
            default_branch: Some("main".into()),
            description: Some("keep this".into()),
            tags: Some(vec!["backend".into()]),
            stack_hint: Some("rust".into()),
            gh_user: Some("acme-bot".into()),
            gh_account: None,
        })
        .await
        .unwrap();

    // A PATCH that only touches repo_url must leave every other field alone.
    let patched = client
        .registry_patch_project(
            "widget",
            &PatchProjectArgs {
                repo_url: Some("https://github.com/acme/widget2".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(patched.repo_url, "https://github.com/acme/widget2");
    assert_eq!(patched.default_branch, "main");
    assert_eq!(patched.description.as_deref(), Some("keep this"));
    assert_eq!(patched.stack_hint.as_deref(), Some("rust"));
    assert_eq!(patched.gh_user.as_deref(), Some("acme-bot"));
    assert_eq!(patched.tags, vec!["backend".to_string()]);
}

/// PATCHing an unregistered project 404s (same shape as `GET .../{name}`).
#[tokio::test]
async fn patch_unknown_project_is_404() {
    let (client, base) = serve_with_base().await;
    // The typed client only surfaces "is this an error", so assert the exact
    // status via a raw request to confirm it is a 404, not some other 4xx/5xx.
    let resp = reqwest::Client::new()
        .patch(format!("{base}/api/v1/projects/nope"))
        .json(&serde_json::json!({ "description": "x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

    let err = client
        .registry_patch_project(
            "nope",
            &PatchProjectArgs {
                description: Some(Some("x".into())),
                ..Default::default()
            },
        )
        .await
        .expect_err("unknown project must error via the typed client too");
    // #2485: the typed client's error must carry the daemon's actual body
    // text, not just a bare status line.
    let msg = err.to_string();
    assert!(msg.contains("project nope not found"), "{msg}");
}

/// #2485: a PATCH rejected for a validation reason (blank `repo_url`) must
/// surface the daemon's actual rejection message through the typed client,
/// not a bare "400 Bad Request" — this is the exact regression the fast-follow
/// from PR #2484's review fixes for `tm projects config` / the TUI form.
#[tokio::test]
async fn patch_rejects_blank_repo_url_surfaces_server_message() {
    let client = serve().await;
    client
        .registry_register_project(&RegisterProjectArgs {
            name: "widget".into(),
            repo_url: "https://github.com/acme/widget".into(),
            default_branch: None,
            description: None,
            tags: None,
            stack_hint: None,
            gh_user: None,
            gh_account: None,
        })
        .await
        .unwrap();

    let err = client
        .registry_patch_project(
            "widget",
            &PatchProjectArgs {
                repo_url: Some("   ".into()),
                ..Default::default()
            },
        )
        .await
        .expect_err("blank repo_url must be rejected");
    let msg = err.to_string();
    assert!(msg.contains("repo_url must not be empty"), "{msg}");
    assert!(msg.contains("400"), "{msg}");
}

/// A body carrying a `name` different from the `{name}` path segment is
/// rejected — `name` is the identity key and PATCH cannot change it.
#[tokio::test]
async fn patch_rejects_name_change() {
    let (client, base) = serve_with_base().await;
    client
        .registry_register_project(&RegisterProjectArgs {
            name: "widget".into(),
            repo_url: "https://github.com/acme/widget".into(),
            default_branch: None,
            description: None,
            tags: None,
            stack_hint: None,
            gh_user: None,
            gh_account: None,
        })
        .await
        .unwrap();

    // The typed client DTO cannot express a `name` field at all (by design —
    // see `PatchProjectArgs`'s doc comment); drive the raw wire body directly
    // to prove the daemon itself rejects the attempt.
    let url = format!("{base}/api/v1/projects/widget");
    let resp = reqwest::Client::new()
        .patch(&url)
        .json(&serde_json::json!({ "name": "renamed" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // The record must be unchanged after the rejected attempt.
    let unchanged = client.registry_get_project("widget").await.unwrap();
    assert_eq!(unchanged.name, "widget");
}

/// Re-issuing an identical PATCH is idempotent: the second call produces the
/// same resulting record as the first (no duplicate tag entries, no drift).
#[tokio::test]
async fn patch_is_idempotent() {
    let client = serve().await;
    client
        .registry_register_project(&RegisterProjectArgs {
            name: "widget".into(),
            repo_url: "https://github.com/acme/widget".into(),
            default_branch: None,
            description: None,
            tags: Some(vec!["backend".into()]),
            stack_hint: None,
            gh_user: None,
            gh_account: None,
        })
        .await
        .unwrap();

    let args = PatchProjectArgs {
        description: Some(Some("stable".into())),
        tags_add: Some(vec!["ml".into()]),
        ..Default::default()
    };
    let first = client
        .registry_patch_project("widget", &args)
        .await
        .unwrap();
    let second = client
        .registry_patch_project("widget", &args)
        .await
        .unwrap();

    assert_eq!(first.description, second.description);
    assert_eq!(first.tags, second.tags);
    assert_eq!(
        second.tags.iter().filter(|t| *t == "ml").count(),
        1,
        "re-applying tags_add must not duplicate the tag"
    );
}

/// A blank/whitespace-only entry in `tags_add` is rejected with 400 rather
/// than silently persisted — this endpoint is the server-side validation
/// surface #2120's CLI/TUI depend on, so a client-side mistake (e.g. a
/// trailing comma in `--add a,b,`) must surface immediately.
#[tokio::test]
async fn patch_rejects_blank_tags_add() {
    let (client, base) = serve_with_base().await;
    client
        .registry_register_project(&RegisterProjectArgs {
            name: "widget".into(),
            repo_url: "https://github.com/acme/widget".into(),
            default_branch: None,
            description: None,
            tags: None,
            stack_hint: None,
            gh_user: None,
            gh_account: None,
        })
        .await
        .unwrap();

    let resp = reqwest::Client::new()
        .patch(format!("{base}/api/v1/projects/widget"))
        .json(&serde_json::json!({ "tags_add": ["backend", "   "] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // Rejected request must not have partially applied "backend".
    let unchanged = client.registry_get_project("widget").await.unwrap();
    assert!(unchanged.tags.is_empty(), "{:?}", unchanged.tags);
}

/// Same rejection rule applies to `tags_remove`.
#[tokio::test]
async fn patch_rejects_blank_tags_remove() {
    let (client, base) = serve_with_base().await;
    client
        .registry_register_project(&RegisterProjectArgs {
            name: "widget".into(),
            repo_url: "https://github.com/acme/widget".into(),
            default_branch: None,
            description: None,
            tags: Some(vec!["backend".into()]),
            stack_hint: None,
            gh_user: None,
            gh_account: None,
        })
        .await
        .unwrap();

    let resp = reqwest::Client::new()
        .patch(format!("{base}/api/v1/projects/widget"))
        .json(&serde_json::json!({ "tags_remove": [""] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // Rejected request must not have removed "backend".
    let unchanged = client.registry_get_project("widget").await.unwrap();
    assert_eq!(unchanged.tags, vec!["backend".to_string()]);
}

/// A tag with leading/trailing whitespace is trimmed before being persisted
/// (not rejected — only fully-blank entries are rejected).
#[tokio::test]
async fn patch_trims_tags_add_whitespace() {
    let client = serve().await;
    client
        .registry_register_project(&RegisterProjectArgs {
            name: "widget".into(),
            repo_url: "https://github.com/acme/widget".into(),
            default_branch: None,
            description: None,
            tags: None,
            stack_hint: None,
            gh_user: None,
            gh_account: None,
        })
        .await
        .unwrap();

    let patched = client
        .registry_patch_project(
            "widget",
            &PatchProjectArgs {
                tags_add: Some(vec!["  rust  ".into()]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(patched.tags, vec!["rust".to_string()]);
}

/// Deliverable create → list → set-status legal → illegal-409 round-trips, and
/// the illegal transition surfaces as `SetStatusError::Rejected` with the legal
/// next states (#2380).
#[tokio::test]
async fn deliverable_crud_and_illegal_transition_409() {
    let client = serve().await;
    // No project registration is required — deliverables defer referential
    // integrity by design (daemon `CreateDeliverable` doc).
    let created = client
        .create_deliverable(
            "widget",
            &CreateDeliverableArgs {
                name: "OAuth2 flow".into(),
                description: None,
                kind: DeliverableKind::Feature,
                estimated_effort: EstimationTier::L,
                ticket_ref: Some("#2117".into()),
                spec_ref: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(created.status, DeliverableStatus::Proposed);

    let id = created.id.to_string();
    let listed = client.list_deliverables("widget", None).await.unwrap();
    assert_eq!(listed.len(), 1);

    // Status filter: proposed matches, in-progress does not (yet).
    assert_eq!(
        client
            .list_deliverables("widget", Some(DeliverableStatus::Proposed))
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        client
            .list_deliverables("widget", Some(DeliverableStatus::InProgress))
            .await
            .unwrap()
            .len(),
        0
    );

    // Legal transition proposed → in-progress.
    let moved = client
        .set_deliverable_status("widget", &id, DeliverableStatus::InProgress)
        .await
        .unwrap();
    assert_eq!(moved.status, DeliverableStatus::InProgress);

    // Illegal transition in-progress → delivered → structured 409.
    let err = client
        .set_deliverable_status("widget", &id, DeliverableStatus::Delivered)
        .await
        .expect_err("must be rejected");
    match err {
        SetStatusError::Rejected {
            from,
            to,
            allowed_next,
        } => {
            assert_eq!(from, "in-progress");
            assert_eq!(to, "delivered");
            // in-progress → {blocked, complete} are the legal successors.
            assert!(
                allowed_next.contains(&"complete".to_string()),
                "{allowed_next:?}"
            );
            assert!(
                allowed_next.contains(&"blocked".to_string()),
                "{allowed_next:?}"
            );
        }
        SetStatusError::Other(e) => panic!("expected Rejected, got {e}"),
    }

    // get round-trips the updated record.
    let fetched = client.get_deliverable("widget", &id).await.unwrap();
    assert_eq!(fetched.status, DeliverableStatus::InProgress);
}

/// Milestone create → list → get round-trips over real HTTP.
#[tokio::test]
async fn milestone_crud_round_trip() {
    let client = serve().await;
    let created = client
        .create_milestone(
            "widget",
            &CreateMilestoneArgs {
                name: "v1.0 Alpha".into(),
                description: Some("first alpha".into()),
                target_date: chrono::DateTime::parse_from_rfc3339("2026-09-01T00:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            },
        )
        .await
        .unwrap();
    assert_eq!(created.name, "v1.0 Alpha");

    let id = created.id.to_string();
    let listed = client.list_milestones("widget").await.unwrap();
    assert_eq!(listed.len(), 1);

    let got = client.get_milestone("widget", &id).await.unwrap();
    assert_eq!(got, created);
}
