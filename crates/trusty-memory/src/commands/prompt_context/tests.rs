//! Integration tests for `commands::prompt_context`.
//!
//! Why: Separated from `mod.rs` to keep the production module under the
//! 500-SLOC cap while retaining full test coverage.
//! What: exercises `build_injection_body`, filter helpers, format helpers,
//! and the full HTTP-daemon path (gated on `axum-server`).
//! Test: this file is the test suite.

use super::*;
// Why (issue #226): `serde_json::json!` is only used by the daemon-based
//      tests, which are themselves gated behind `axum-server`. Mirror the
//      gate here so `--no-default-features` builds stay warning-free.
#[cfg(feature = "axum-server")]
use serde_json::json;

/// Why (issue #134): the recall query needs the actual prompt text the
/// user typed; the stdin payload carries it under `"prompt"`.
/// What: parses three shapes — full JSON with `prompt`, JSON without,
/// and raw text — and asserts each returns the expected string.
/// Test: itself.
#[test]
fn parse_user_prompt_prefers_prompt_field() {
    let json_with_prompt = serde_json::json!({
        "prompt": "what is rust?",
        "cwd": "/tmp/example",
    })
    .to_string();
    assert_eq!(parse_user_prompt(&json_with_prompt), "what is rust?");

    let json_without_prompt = serde_json::json!({"cwd": "/tmp/example"}).to_string();
    assert_eq!(parse_user_prompt(&json_without_prompt), json_without_prompt);

    assert_eq!(parse_user_prompt("plain text query"), "plain text query");
    assert_eq!(parse_user_prompt(""), "");
}

/// Why (issue #139): the deny-tag filter is the load-bearing piece of
/// the recall-quality fix; unit-test the boundary conditions in
/// isolation so a refactor cannot silently regress them.
/// What: case-insensitive matching, empty deny list = passthrough,
/// drawers with no tags = kept (no excluded tag can match).
/// Test: itself.
#[test]
fn filter_drawers_by_deny_tags_handles_edge_cases() {
    use filter::{filter_drawers_by_deny_tags, RecalledDrawer};
    let make = |tags: &[&str]| RecalledDrawer {
        content: "irrelevant".into(),
        tags: tags.iter().map(|s| s.to_string()).collect(),
        layer: Some(2),
    };

    // Empty deny list → passthrough.
    let drawers = vec![make(&["claude-session"]), make(&["rust"])];
    let out = filter_drawers_by_deny_tags(drawers.clone(), &[]);
    assert_eq!(out.len(), 2, "empty deny list must pass everything");

    // Case-insensitive match (deny "claude-session" vs tag "Claude-Session").
    let drawers = vec![make(&["Claude-Session"]), make(&["rust"])];
    let out = filter_drawers_by_deny_tags(drawers, &["claude-session".to_string()]);
    assert_eq!(out.len(), 1);
    assert!(out[0].tags.iter().any(|t| t == "rust"));

    // Drawer with no tags is always kept.
    let drawers = vec![make(&[]), make(&["user-prompt"])];
    let out = filter_drawers_by_deny_tags(drawers, &["user-prompt".to_string()]);
    assert_eq!(out.len(), 1, "tagless drawers must survive the filter");
    assert!(out[0].tags.is_empty());

    // Multiple deny entries — any match excludes.
    let drawers = vec![
        make(&["claude-session"]),
        make(&["user-prompt"]),
        make(&["signal"]),
    ];
    let out = filter_drawers_by_deny_tags(
        drawers,
        &["claude-session".to_string(), "user-prompt".to_string()],
    );
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].tags, vec!["signal".to_string()]);
}

/// Why (issue #134): KG triples should only surface when one of their
/// endpoints actually appears in the user's prompt; otherwise the
/// injection just dumps random graph noise.
/// What: build a small set of triples; query a prompt that mentions
/// only one subject; assert exactly the matching triple comes back.
/// Test: itself.
#[test]
fn select_relevant_triples_filters_by_prompt_overlap() {
    use filter::{select_relevant_triples, RawTriple};
    let triples = vec![
        RawTriple {
            subject: "tga".into(),
            predicate: "is_alias_for".into(),
            object: "trusty-git-analytics".into(),
        },
        RawTriple {
            subject: "python".into(),
            predicate: "is-a".into(),
            object: "language".into(),
        },
        RawTriple {
            subject: "rust".into(),
            predicate: "is-a".into(),
            object: "language".into(),
        },
    ];
    let chosen = select_relevant_triples(&triples, "tell me about rust integration", 5);
    assert_eq!(chosen.len(), 1, "only the rust triple should match");
    assert_eq!(chosen[0].subject, "rust");

    // Empty / no-overlap prompt → no triples.
    let none = select_relevant_triples(&triples, "weather forecast next week", 5);
    assert!(none.is_empty());
}

/// Why: the injection has a hard 4 KB byte ceiling so a runaway palace
/// can't drown the model's prompt; truncation must end with `…` and
/// stay valid UTF-8.
/// What: synthesises drawers whose previews exceed the cap, calls
/// `compose_injection`, asserts the result is `<= INJECTION_BYTE_CAP`
/// and ends with `…`.
/// Test: itself.
#[test]
fn compose_injection_truncates_at_cap() {
    use filter::{RawTriple, RecalledDrawer};
    use format::compose_injection;
    // Stuff a giant global-facts block to push the composition past the
    // 4 KB byte cap. Drawer previews are already capped at
    // DRAWER_PREVIEW_CHARS so the cap-trigger has to come from the
    // global section.
    let big_global = "## Big block\n".to_string() + &"- fact line\n".repeat(500);
    let drawers: Vec<RecalledDrawer> = (0..5)
        .map(|i| RecalledDrawer {
            content: format!("drawer {i} content"),
            tags: vec!["tag1".into()],
            layer: Some(2),
        })
        .collect();
    let triples: Vec<RawTriple> = (0..5)
        .map(|i| RawTriple {
            subject: format!("subject{i}"),
            predicate: "p".into(),
            object: "object".into(),
        })
        .collect();
    let out = compose_injection(Some(&big_global), &drawers, &triples, Some("alpha"));
    assert!(
        out.len() <= INJECTION_BYTE_CAP,
        "expected len <= cap; got {}",
        out.len()
    );
    // Truncation marker survives.
    assert!(
        out.ends_with('…'),
        "expected `…` truncation marker; got tail: {}",
        &out[out.len().saturating_sub(20)..]
    );
}

/// Why: an empty composition (no global facts, no drawers, no triples)
/// must return an empty string so the caller can substitute the
/// legacy placeholder. Section headers should never appear without
/// content beneath them.
/// What: call `compose_injection` with empty inputs and assert the
/// result is empty.
/// Test: itself.
#[test]
fn compose_injection_empty_inputs_yields_empty() {
    use format::compose_injection;
    let out = compose_injection(None, &[], &[], Some("alpha"));
    assert!(out.is_empty(), "got: {out:?}");
}

/// Why (issue #125): when Claude Code invokes the UserPromptSubmit hook,
/// the stdin JSON carries a `cwd` field that reflects the user's actual
/// working directory at prompt time. The hook process cwd may be where
/// the hook was registered (typically a fixed install root), not where
/// the user actually is. The log palace must follow the stdin `cwd`.
/// What: build a stdin JSON payload pointing at a tempdir, derive the
/// expected slug for that tempdir via the *_at variant, and assert
/// `resolve_palace_for_log` returns the same slug — even though the
/// process cwd is unchanged and would resolve to a different slug.
/// Test: itself.
#[test]
fn resolve_palace_for_log_prefers_stdin_cwd() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path().join("stdin-driven-project");
    std::fs::create_dir_all(&project).expect("create project dir");
    let payload = serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "cwd": project.to_string_lossy(),
        "prompt": "hello"
    })
    .to_string();

    let expected =
        crate::messaging::cwd_palace_slug_at(&project).expect("derive slug from stdin cwd");
    let got = resolve_palace_for_log(&payload);
    assert_eq!(
        got, expected,
        "stdin `cwd` must override the process cwd for the log palace slug"
    );
    assert!(
        got.contains("stdin-driven-project"),
        "expected slug derived from stdin path, got {got:?}"
    );
}

/// Why (issue #125): when stdin is empty or non-JSON, the helper must
/// fall through to the process-cwd resolution path so manual `trusty-
/// memory prompt-context` invocations from a TTY still get a useful
/// palace identifier.
/// What: pass an empty string and a non-JSON string; assert the result
/// is *not* the legacy `"<unknown>"` sentinel (the process cwd here is a
/// real git repo, so cwd_palace_slug succeeds).
/// Test: itself.
#[test]
fn resolve_palace_for_log_falls_back_to_process_cwd() {
    let from_empty = resolve_palace_for_log("");
    let from_garbage = resolve_palace_for_log("not json at all");
    assert_eq!(from_empty, from_garbage);
    assert_ne!(from_empty, "<unknown>");
}

/// Why: the hook is wired into every Claude Code prompt the user types;
/// failing it would block the prompt. The contract is that a missing
/// daemon-address lockfile (the canonical "daemon not running" signal)
/// must produce `Ok(())` with no stdout, not an error.
/// What: redirects `trusty_common::resolve_data_dir` at a fresh tempdir
/// via `TRUSTY_DATA_DIR_OVERRIDE` so `read_daemon_addr("trusty-memory")`
/// observes a missing lockfile, then runs the handler and asserts it
/// returns `Ok(())`. Calls `handle_prompt_context_with_payload` directly
/// (issue #2079) with a fixed empty payload instead of
/// `handle_prompt_context()` so the test never spawns a real blocking
/// stdin read that could outlive the test's `Runtime` teardown.
#[tokio::test]
async fn prompt_context_returns_ok_without_daemon() {
    let _guard = crate::commands::env_test_lock().lock().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    // SAFETY: tests serialise on `TRUSTY_DATA_DIR_OVERRIDE` by convention
    // across the trusty-* workspace (see trusty-common's lib.rs notes).
    // This test only mutates the env var inside its own scope.
    unsafe {
        std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, tmp.path());
    }
    let res = handle_prompt_context_with_payload(String::new()).await;
    unsafe {
        std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
    }
    assert!(
        res.is_ok(),
        "missing daemon lockfile must degrade to Ok(()), got {res:?}"
    );
}

/// Why (issue #134): the hook's whole value-prop is surfacing relevant
/// drawers from the palace; previously it only returned the workspace-
/// level hot facts. Confirm that with a live daemon, a populated palace,
/// and a prompt that mentions a known keyword, the rendered injection
/// contains real drawer content — not the legacy `EMPTY_PLACEHOLDER`.
/// What: spin up a real HTTP daemon under a tempdir-pinned data root,
/// create a palace whose slug matches a project tempdir basename,
/// populate it with three keyworded drawers via the MCP dispatch
/// (which loads the real embedder), then call `build_injection_body`
/// with a stdin payload carrying `cwd = <project tempdir>` and
/// `prompt = "how does rust integration work?"`. Assert the body
/// contains the rust drawer's content and the relevant-memories
/// section header.
/// Test: itself.
/// Note (issue #226): gated on `axum-server` because it spins up the
/// real HTTP daemon via `run_http_on`.
#[cfg(feature = "axum-server")]
#[tokio::test]
async fn prompt_context_recalls_palace_drawers() {
    let _guard = crate::commands::env_test_lock().lock().await;
    let (state, _data_dir_tmp, _project_dir_tmp, project_dir, slug, addr_handle) =
        spin_up_test_daemon_with_palace("prompt-ctx-recall-pop").await;

    // Populate the palace with three diverged drawers via MCP dispatch.
    // Using the MCP path here exercises the real embedder + KG hook.
    for (text, tags) in [
        (
            "Rust integration uses tokio for async tasks and serde for JSON",
            vec!["rust", "tokio"],
        ),
        (
            "Python bindings ship via PyO3 with custom ABI shims",
            vec!["python", "pyo3"],
        ),
        (
            "Knowledge graph stores triples in redb with valid_from intervals",
            vec!["kg", "redb"],
        ),
    ] {
        let tags_json: Vec<serde_json::Value> = tags.iter().map(|t| json!(t)).collect();
        let _ = crate::tools::dispatch_tool(
            &state,
            "memory_remember",
            json!({
                "palace": slug,
                "text": text,
                "room": "General",
                "tags": tags_json,
            }),
        )
        .await
        .expect("memory_remember");
    }

    // Build the stdin payload Claude Code would send: a JSON object with
    // `cwd` (the project dir) and `prompt` (mentioning "rust").
    let payload = json!({
        "hook_event_name": "UserPromptSubmit",
        "cwd": project_dir.to_string_lossy(),
        "prompt": "how does rust integration work?"
    })
    .to_string();

    let start = std::time::Instant::now();
    let body = build_injection_body(&payload).await;
    let elapsed_ms = start.elapsed().as_millis();
    eprintln!("prompt_context_recalls_palace_drawers latency: {elapsed_ms}ms");

    assert_ne!(
        body, EMPTY_PLACEHOLDER,
        "populated palace must return real content, not the placeholder"
    );
    // The injection must mention the rust drawer's content (proves
    // recall actually targeted the resolved palace and surfaced
    // prompt-relevant memories).
    assert!(
        body.to_lowercase().contains("rust") && body.to_lowercase().contains("integration"),
        "expected rust integration drawer in injection; got:\n{body}"
    );
    // Section header should be present (proves the multi-section
    // composition is wired through).
    assert!(
        body.contains("Relevant memories") || body.contains("memories from palace"),
        "expected a `Relevant memories` section; got:\n{body}"
    );

    // Performance guardrail (issue #134 target: <200 ms p95). On
    // CI/dev machines this comfortably stays under the budget.
    assert!(
        elapsed_ms < 5_000,
        "prompt-context too slow ({elapsed_ms}ms) — investigate"
    );

    addr_handle.shutdown().await;
}

/// Why (issue #134, negative case): when the resolved palace has no
/// drawers AND no global hot facts have been asserted, the hook must
/// still emit a safe placeholder so downstream consumers see byte-
/// identical behaviour to the pre-fix daemon. Don't regress the empty
/// case while fixing the populated one.
/// What: spin up the same daemon shape but skip the drawer-population
/// step; assert the body equals [`EMPTY_PLACEHOLDER`].
/// Test: itself.
/// Note (issue #226): gated on `axum-server`; spawns the HTTP daemon.
#[cfg(feature = "axum-server")]
#[tokio::test]
async fn prompt_context_empty_palace_falls_back_to_global() {
    let _guard = crate::commands::env_test_lock().lock().await;
    let (_state, _data_dir_tmp, _project_dir_tmp, project_dir, _slug, addr_handle) =
        spin_up_test_daemon_with_palace("prompt-ctx-recall-empty").await;

    let payload = json!({
        "hook_event_name": "UserPromptSubmit",
        "cwd": project_dir.to_string_lossy(),
        "prompt": "no drawers exist here"
    })
    .to_string();
    let body = build_injection_body(&payload).await;
    assert_eq!(
        body, EMPTY_PLACEHOLDER,
        "empty palace + empty prompt-facts must fall back to the placeholder"
    );

    addr_handle.shutdown().await;
}

/// Test fixture: spin up a real HTTP daemon under a tempdir-pinned
/// data root, create a palace with the given slug under a project
/// tempdir whose basename matches the slug, and return everything
/// the test needs to interact with the daemon.
///
/// Why: the prompt-context hook talks HTTP to a live daemon; unit
/// tests with the router alone can't exercise the `read_daemon_addr`
/// → HTTP round trip. This helper wires it all together in one place.
/// What: creates two tempdirs (one for the data root, one for the
/// project cwd whose basename equals `palace_slug`), pins
/// `TRUSTY_DATA_DIR_OVERRIDE`, builds the `AppState`, creates the
/// palace, spawns the HTTP server on `127.0.0.1:0`, and waits for
/// the daemon addr file to land. Returns `(state, data_dir_tmp,
/// project_dir_tmp, project_dir_path, palace_slug, addr_handle)`.
/// Test: indirectly via `prompt_context_recalls_palace_drawers` and
/// `prompt_context_empty_palace_falls_back_to_global`.
/// Note (issue #226): gated on `axum-server` because `run_http_on` is
/// only available when the HTTP-serving surface is compiled in.
#[cfg(feature = "axum-server")]
async fn spin_up_test_daemon_with_palace(
    palace_slug: &str,
) -> (
    crate::AppState,
    tempfile::TempDir,
    tempfile::TempDir,
    std::path::PathBuf,
    String,
    DaemonHandle,
) {
    let data_tmp = tempfile::tempdir().expect("data tempdir");
    let project_tmp = tempfile::tempdir().expect("project tempdir");
    // Build a project directory whose basename equals the palace slug.
    // This is what `cwd_palace_slug_at` will derive from the stdin `cwd`.
    let project_dir = project_tmp.path().join(palace_slug);
    std::fs::create_dir_all(&project_dir).expect("project dir");

    // SAFETY: env_test_lock serialises this section.
    unsafe {
        std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, data_tmp.path());
        std::env::remove_var(crate::prompt_log::ENV_ENABLED);
        std::env::remove_var(crate::prompt_log::ENV_DIR);
        std::env::remove_var(crate::prompt_log::ENV_HASH_PROMPTS);
        // Issue #88: bypass palace-slug enforcement so test palaces with
        // arbitrary names can be created without a matching project root.
        std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
    }

    // Issue #1217: the default palace ID is now derived from project identity
    // (git owner/repo, else parent/dir slug), so a bare tempdir whose basename
    // equals `palace_slug` no longer resolves to that slug (a non-git dir
    // derives `<parent>-<leaf>`). Write a committed pin file in `project_dir`
    // so the hook's `cwd_palace_slug_at` resolves deterministically to the
    // palace this fixture created. This is fully hermetic (per-tempdir, no env
    // or global state to leak into sibling tests) and exercises the #1217
    // pin-file-primacy anchor that keeps existing palaces from being orphaned.
    crate::project_root::write_project_pin(
        &project_dir,
        &crate::project_root::ProjectPin {
            schema_version: crate::project_root::PIN_SCHEMA_VERSION,
            palace: palace_slug.to_string(),
            note: None,
        },
    )
    .expect("write project pin for fixture");

    let data_root =
        trusty_common::resolve_data_dir("trusty-memory").expect("resolve data dir under override");
    let state = crate::AppState::new(data_root.clone());
    // Flip to Ready so the issue #911 warming preflight does not reject
    // the `memory_remember` calls that seed fixture data below.
    state.set_ready();

    // Create the palace via MCP dispatch so the on-disk metadata
    // matches what a real client would have produced. The `TRUSTY_MEMORY_PALACE`
    // override pinned above makes `cwd_palace_slug_at` (and thus the hook)
    // resolve to exactly this slug (issue #1217).
    let _ = crate::tools::dispatch_tool(&state, "palace_create", json!({"name": palace_slug}))
        .await
        .expect("palace_create");

    // Bind a random local port and start the HTTP server. `run_http_on`
    // writes the addr file as part of startup; we poll for it briefly
    // so the subsequent `read_daemon_addr` call succeeds.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind 127.0.0.1:0");
    let addr = listener.local_addr().expect("local_addr");
    let state_for_server = state.clone();
    let handle = tokio::spawn(async move {
        let _ = crate::run_http_on(state_for_server, listener).await;
    });

    // Poll for the addr file (run_http_on writes it after binding).
    // Generous deadline so a contended CI machine doesn't flake — the
    // disk_size_ticker spawned inside `run_http_on` does some setup
    // work before the addr write lands. Bumped from 250 to 500
    // attempts (5 s → 10 s) under issue #139: the recall-quality fix
    // doubled the number of fixtures spinning a daemon (5 tests now
    // share this helper), so a heavily loaded host needs more headroom
    // before the first `http_addr` write lands.
    let addr_file = data_root.join("http_addr");
    let mut attempts = 0;
    while !addr_file.exists() && attempts < 500 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        attempts += 1;
    }
    assert!(
        addr_file.exists(),
        "daemon never wrote http_addr at {} (attempts={attempts})",
        addr_file.display()
    );

    (
        state,
        data_tmp,
        project_tmp,
        project_dir,
        palace_slug.to_string(),
        DaemonHandle {
            addr,
            join: Some(handle),
        },
    )
}

/// Test-only handle to a spawned daemon — aborts the server task on
/// drop or explicit `shutdown` so the tempdir cleanup doesn't race
/// with in-flight requests.
/// Note (issue #226): gated on `axum-server` because the only callers
/// are HTTP-daemon-dependent tests.
#[cfg(feature = "axum-server")]
struct DaemonHandle {
    #[allow(dead_code)]
    addr: std::net::SocketAddr,
    join: Option<tokio::task::JoinHandle<()>>,
}

#[cfg(feature = "axum-server")]
impl DaemonHandle {
    async fn shutdown(mut self) {
        if let Some(h) = self.join.take() {
            h.abort();
            let _ = h.await;
        }
        // Release the pinned data dir override for sibling tests.
        // SAFETY: protected by env_test_lock in the caller.
        unsafe {
            std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
        }
    }
}

/// Why (issue #139): live evidence from the user's `trusty-tools`
/// session showed the prompt-context hook injecting raw past user
/// prompts (drawers tagged `claude-session` / `user-prompt` from an
/// upstream auto-capture hook) on every UserPromptSubmit, dominating
/// real palace knowledge. Filtering by deny-listed tags is the
/// cheapest in-tree fix. Verify the default deny list drops both
/// auto-capture tags AND keeps a signal drawer untouched.
/// What: populate a palace with three drawers — one tagged
/// `claude-session`, one tagged `user-prompt`, one tagged with only
/// signal tags. The prompt mentions a keyword shared by all three so
/// recall returns all three. Assert the injection contains only the
/// signal drawer's content and neither of the deny-listed drawers'
/// content surfaces.
/// Test: itself.
/// Note (issue #226): gated on `axum-server`; spins up the HTTP daemon.
#[cfg(feature = "axum-server")]
#[tokio::test]
async fn prompt_context_recall_filters_deny_tags() {
    let _guard = crate::commands::env_test_lock().lock().await;
    // Defensive: scrub the env override in case a sibling test set it
    // and panicked before its cleanup ran. Both vars are pinned to a
    // known state for this test.
    unsafe {
        std::env::remove_var(ENV_RECALL_DENY_TAGS);
    }
    let (state, _data_dir_tmp, _project_dir_tmp, project_dir, slug, addr_handle) =
        spin_up_test_daemon_with_palace("prompt-ctx-deny-tags").await;

    // Three drawers, all mentioning "rust" so recall returns all three.
    // The first two carry the default deny tags and must be filtered;
    // their content is sized above the signal-filter threshold so the
    // remember path accepts them (the deny filter operates on tags,
    // not content length, so the body must be realistic).
    // The third has only signal tags and must survive.
    for (text, tags) in [
        (
            "user: how do I use rust async tokio runtime and serde derive macros in this project to glue an http handler to a kafka producer",
            vec!["claude-session", "user-prompt", "rust"],
        ),
        (
            "user: yes please go ahead and refactor the rust async producer module, this captured prompt fragment should never be surfaced",
            vec!["user-prompt", "rust"],
        ),
        (
            "Rust integration uses tokio for async tasks and serde for JSON",
            vec!["rust", "tokio"],
        ),
    ] {
        let tags_json: Vec<serde_json::Value> = tags.iter().map(|t| json!(t)).collect();
        let _ = crate::tools::dispatch_tool(
            &state,
            "memory_remember",
            json!({
                "palace": slug,
                "text": text,
                "room": "General",
                "tags": tags_json,
            }),
        )
        .await
        .expect("memory_remember");
    }

    let payload = json!({
        "hook_event_name": "UserPromptSubmit",
        "cwd": project_dir.to_string_lossy(),
        "prompt": "how does rust integration work?"
    })
    .to_string();
    let body = build_injection_body(&payload).await;

    assert!(
        body.contains("tokio") && body.contains("serde"),
        "signal drawer must survive deny filter; got:\n{body}"
    );
    assert!(
        !body.contains("kafka producer"),
        "claude-session-tagged drawer must be filtered out; got:\n{body}"
    );
    assert!(
        !body.contains("captured prompt fragment"),
        "user-prompt-tagged drawer must be filtered out; got:\n{body}"
    );

    addr_handle.shutdown().await;
}

/// Why (issue #139): operators need to widen the deny list at runtime
/// without rebuilding the binary — e.g. when a palace accumulates a
/// project-specific synthetic tag. The env override
/// [`ENV_RECALL_DENY_TAGS`] supplies the comma-separated list.
/// What: set the env override to a custom tag, populate a palace
/// where the only recallable drawer carries that custom tag plus a
/// keyword shared with the prompt, assert the drawer is filtered out
/// and the body falls back to the global / empty placeholder path
/// (no `Relevant memories` section).
/// Test: itself.
/// Note (issue #226): gated on `axum-server`; spins up the HTTP daemon.
#[cfg(feature = "axum-server")]
#[tokio::test]
async fn prompt_context_recall_env_override_extends_deny_list() {
    let _guard = crate::commands::env_test_lock().lock().await;
    // SAFETY: env_test_lock serialises this section.
    unsafe {
        std::env::set_var(ENV_RECALL_DENY_TAGS, "noise-tag");
    }
    let (state, _data_dir_tmp, _project_dir_tmp, project_dir, slug, addr_handle) =
        spin_up_test_daemon_with_palace("prompt-ctx-env-deny").await;

    let _ = crate::tools::dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": slug,
            "text": "Rust integration uses tokio and serde for the async layer",
            "room": "General",
            "tags": ["noise-tag", "rust"],
        }),
    )
    .await
    .expect("memory_remember");

    let payload = json!({
        "hook_event_name": "UserPromptSubmit",
        "cwd": project_dir.to_string_lossy(),
        "prompt": "how does rust integration work?"
    })
    .to_string();
    let body = build_injection_body(&payload).await;

    // The single drawer is filtered, so the body should NOT carry its
    // content. The empty-palace fallback (or the global facts path)
    // takes over.
    assert!(
        !body.contains("tokio and serde"),
        "noise-tag drawer must be filtered when env override targets it; got:\n{body}"
    );

    // Clean up the env override so it does not leak to sibling tests.
    // SAFETY: env_test_lock still held until DaemonHandle::shutdown.
    unsafe {
        std::env::remove_var(ENV_RECALL_DENY_TAGS);
    }
    addr_handle.shutdown().await;
}

/// Why (issue #139, regression): a palace consisting entirely of
/// deny-listed drawers must NOT crash the hook and must NOT inject a
/// `Relevant memories` section. The empty-palace fallback path (the
/// existing global hot-facts route from #136) must kick in instead.
/// What: populate a palace where every drawer carries a deny tag,
/// run the hook, assert the body is either the legacy placeholder OR
/// global-facts-only — crucially, it must not contain any of the
/// drawer content nor a `Relevant memories` section header.
/// Test: itself.
/// Note (issue #226): gated on `axum-server`; spins up the HTTP daemon.
#[cfg(feature = "axum-server")]
#[tokio::test]
async fn prompt_context_recall_all_filtered_falls_back_to_global() {
    let _guard = crate::commands::env_test_lock().lock().await;
    unsafe {
        std::env::remove_var(ENV_RECALL_DENY_TAGS);
    }
    let (state, _data_dir_tmp, _project_dir_tmp, project_dir, slug, addr_handle) =
        spin_up_test_daemon_with_palace("prompt-ctx-all-filtered").await;

    // Every drawer is deny-listed. Bodies are sized above the signal
    // filter threshold so memory_remember accepts them — the deny-tag
    // filter operates downstream on tags, not content length.
    for (text, tags) in [
        (
            "user: status update on the rust async rewrite, the kafka consumer should not surface in any prompt-context injection",
            vec!["claude-session", "user-prompt", "rust"],
        ),
        (
            "user: yes please continue with the rust refactor on the producer side, this prompt fragment must be filtered out of recall",
            vec!["claude-session", "rust"],
        ),
    ] {
        let tags_json: Vec<serde_json::Value> = tags.iter().map(|t| json!(t)).collect();
        let _ = crate::tools::dispatch_tool(
            &state,
            "memory_remember",
            json!({
                "palace": slug,
                "text": text,
                "room": "General",
                "tags": tags_json,
            }),
        )
        .await
        .expect("memory_remember");
    }

    let payload = json!({
        "hook_event_name": "UserPromptSubmit",
        "cwd": project_dir.to_string_lossy(),
        "prompt": "tell me about rust"
    })
    .to_string();
    let body = build_injection_body(&payload).await;

    // No drawer content leaks through and no `Relevant memories`
    // section is rendered — either the global hot-facts section or
    // the empty-placeholder fallback wins.
    assert!(
        !body.contains("kafka consumer") && !body.contains("producer side"),
        "filtered drawer content must not leak; got:\n{body}"
    );
    assert!(
        !body.contains("Relevant memories"),
        "no `Relevant memories` section should render when every drawer is filtered; got:\n{body}"
    );

    addr_handle.shutdown().await;
}

/// Why (issue #105): even when the daemon is down, the hook must still
/// log an attempt entry so operators can see "prompt-context fired N
/// times but the daemon was unreachable" in the JSONL stream.
/// What: pin a tempdir as the data directory, run the handler with no
/// daemon, and assert exactly one log file landed under `<tmp>/logs/`
/// with a single JSONL line whose `injection_kind` is the prompt-context
/// kind. Calls `handle_prompt_context_with_payload` directly (issue
/// #2079) with a fixed empty payload so this test never spawns a real
/// blocking stdin read.
/// Test: itself.
#[tokio::test]
async fn prompt_context_logs_attempt_without_daemon() {
    let _guard = crate::commands::env_test_lock().lock().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    unsafe {
        std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, tmp.path());
        std::env::remove_var(crate::prompt_log::ENV_ENABLED);
        std::env::remove_var(crate::prompt_log::ENV_DIR);
        std::env::remove_var(crate::prompt_log::ENV_HASH_PROMPTS);
    }
    let res = handle_prompt_context_with_payload(String::new()).await;
    let logs_dir = trusty_common::resolve_data_dir("trusty-memory")
        .expect("resolve data dir")
        .join("logs");
    unsafe {
        std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
    }
    assert!(res.is_ok());
    let files: Vec<_> = std::fs::read_dir(&logs_dir)
        .expect("logs dir should be created")
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("enriched-prompts."))
        })
        .collect();
    assert_eq!(
        files.len(),
        1,
        "expected one enriched-prompts log file, got {files:?}"
    );
    let content = std::fs::read_to_string(&files[0]).expect("read log");
    let line = content.lines().next().expect("at least one line");
    let parsed: crate::prompt_log::PromptLogEntry =
        serde_json::from_str(line).expect("parse JSONL");
    assert_eq!(parsed.hook_type, "UserPromptSubmit");
    assert_eq!(parsed.injection_kind, "prompt-context-facts");
}

/// Why (issue #2043): [`bounded_blocking`] is the mechanism that keeps the
/// stdin read from ever hanging the hook. A real `std::io::Stdin` can't be
/// swapped in-process, so this test drives the exact same
/// `spawn_blocking` + `timeout` mechanism with a synthetic closure that
/// sleeps past the deadline, proving the caller gets control back on time
/// with a `None` (fail-open) result rather than waiting for the closure.
/// What: races a closure that sleeps 400 ms against a 100 ms deadline;
/// asserts `None` is returned and wall time stays close to the deadline,
/// not the closure's sleep duration. The 400 ms sleep (rather than
/// something longer) is deliberate: `tokio::test`'s per-test `Runtime`
/// still waits for this abandoned blocking thread to finish during its
/// own teardown (blocking-pool threads cannot be cancelled — this is the
/// same reason `main.rs` calls `std::process::exit` after the
/// `prompt-context` dispatch instead of returning normally), so the test
/// itself pays that thread's full sleep as wall-clock tax regardless of
/// the assertion below. Kept short to bound that tax.
/// Test: itself.
#[tokio::test]
async fn bounded_blocking_times_out_on_slow_closure() {
    let start = std::time::Instant::now();
    let result: Option<String> = bounded_blocking(
        || {
            std::thread::sleep(std::time::Duration::from_millis(400));
            "too-late".to_string()
        },
        std::time::Duration::from_millis(100),
    )
    .await;
    let elapsed = start.elapsed();
    assert_eq!(result, None, "slow closure must fail open to None");
    assert!(
        elapsed < std::time::Duration::from_millis(300),
        "bounded_blocking did not return promptly: elapsed={elapsed:?}"
    );
}

/// Why (issue #2043): the deadline mechanism must not clip a value that
/// finishes comfortably within budget — only genuinely slow closures
/// should fail open.
/// What: races an instantly-returning closure against a 300 ms deadline;
/// asserts the real value comes back.
/// Test: itself.
#[tokio::test]
async fn bounded_blocking_returns_value_when_fast_enough() {
    let result =
        bounded_blocking(|| "fast".to_string(), std::time::Duration::from_millis(300)).await;
    assert_eq!(result, Some("fast".to_string()));
}

/// Why (issue #2043): proves the end-to-end fix — before this issue, a
/// daemon that accepted a connection but never answered could block
/// `handle_prompt_context` for as long as the daemon took (bounded only by
/// Claude Code's own ~15 s hook ceiling, which is exactly the hang this
/// issue reports). This test stands up a real HTTP listener that accepts
/// every request and sleeps 5 s before responding — far longer than
/// [`BODY_DEADLINE`] + [`EMIT_DEADLINE`] — and asserts the hook still
/// returns `Ok(())` well inside its own deadline budget.
/// What: writes the slow listener's address as the discovered daemon addr
/// (via `write_daemon_addr`, matching what a real running daemon would
/// have written), runs `handle_prompt_context_with_payload` (issue #2079 —
/// direct call with a fixed empty payload so this test never spawns a
/// real blocking stdin read), and asserts both the `Ok(())` contract and
/// a bounded wall-clock time.
/// Test: itself.
/// Note (issue #226): gated on `axum-server` — it needs a real `axum`
/// listener to simulate the slow daemon.
#[cfg(feature = "axum-server")]
#[tokio::test]
async fn handle_prompt_context_fails_open_on_slow_daemon() {
    let _guard = crate::commands::env_test_lock().lock().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    unsafe {
        std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, tmp.path());
        std::env::remove_var(crate::prompt_log::ENV_ENABLED);
        std::env::remove_var(crate::prompt_log::ENV_DIR);
        std::env::remove_var(crate::prompt_log::ENV_HASH_PROMPTS);
    }

    async fn slow_handler() -> &'static str {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        "slow"
    }
    let app = axum::Router::new().fallback(axum::routing::any(slow_handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind 127.0.0.1:0");
    let addr = listener.local_addr().expect("local_addr");
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    trusty_common::write_daemon_addr("trusty-memory", &addr.to_string())
        .expect("write fake daemon addr");

    let start = std::time::Instant::now();
    let res = handle_prompt_context_with_payload(String::new()).await;
    let elapsed = start.elapsed();

    server.abort();
    unsafe {
        std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
    }

    assert!(
        res.is_ok(),
        "must fail open even with a stalled daemon, got {res:?}"
    );
    let budget = BODY_DEADLINE + EMIT_DEADLINE + std::time::Duration::from_millis(750);
    assert!(
        elapsed < budget,
        "handle_prompt_context took {elapsed:?}, expected under budget {budget:?} \
         (a stalled daemon must never make the hook wait out its own timeout)"
    );
}
