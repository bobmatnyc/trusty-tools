//! Unit tests for `session start` protected-path routing (#1916).
//!
//! Why: the #1916 fix has three behaviors to prove — (1) a recognized
//! GitHub-backed git repo routes through the managed-spawn code path and
//! never touches the source directory; (2) a directory that is NOT a
//! recognized git repo preserves the pre-#1916 in-place behavior; (3) the
//! `repo_url`/`ref`/`task` fields `session start` sends match the ones the
//! bare `tm` guided default's "launch new" sends (item 3 of the issue), so
//! the two entry points can never silently diverge on WHERE/WHAT gets
//! spawned. All three must be hermetic (no real daemon, no writes into the
//! developer's/CI's real `~/.claude`/`~/.trusty-mpm`).
//! What: `session_start_dispatches_managed_new_for_github_repo` drives
//! [`super::start_session`] end-to-end against a real temp git repo with a
//! `github.com` origin remote and an intentionally-unreachable daemon URL,
//! then asserts no tm artifacts were written into the repo.
//! `session_start_in_place_writes_stash_and_hard_fails_on_daemon_unreachable`
//! drives [`super::start_session_in_place`] directly with a hermetic
//! `FrameworkPaths::under(tempdir)` and asserts the pre-#1916 in-place
//! behavior (writes the instructions stash, hard-fails when the daemon is
//! unreachable) is preserved unchanged.
//! `session_start_posts_the_same_wire_shape_bare_tm_guided_default_sends`
//! captures the real outgoing `POST /api/v1/sessions/managed` body against a
//! minimal in-process HTTP server and asserts it matches the literal JSON
//! shape `commands::guided_launch::launch_new_session_and_attach` sends for
//! bare `tm`'s "launch new" picker choice.
//! Test: this file IS the test.

use super::{start_session, start_session_in_place};

/// Run `git <args>` in `dir`, panicking with full context on failure.
///
/// Why: every test below needs a real (but disposable) git repo; a tiny
/// helper keeps the setup boilerplate out of the test bodies.
/// What: shells out to `git -C <dir> <args>` and panics if the exit status is
/// non-zero.
/// Test: exercised by every test in this file (setup step).
fn git(dir: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .unwrap_or_else(|e| panic!("failed to run git {args:?}: {e}"));
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

/// An address nothing listens on, so any HTTP call fails fast (connection
/// refused) instead of hanging or hitting a real daemon.
///
/// Why: both tests need a daemon-unreachable URL; centralising the constant
/// avoids typo drift between them.
/// What: `http://127.0.0.1:1` — port 1 is a reserved/never-listened-on port
/// on every platform these tests run on.
const UNREACHABLE_URL: &str = "http://127.0.0.1:1";

/// #1916: `session start` inside a recognized GitHub-backed git repo must
/// route through the SAME protected-path mechanism `session new` uses, and
/// must NEVER write tm artifacts (statusline config, `.trusty-mpm/`) into the
/// source directory itself — the exact regression the issue reported.
///
/// Why: this is the acceptance criterion from issue #1916 item (a) — proving
/// the fix without requiring a live daemon. `managed_route::run` soft-fails
/// (prints a `CommandResult::Error`, returns `Ok(())`) when the daemon is
/// unreachable, so `start_session` returning `Ok(())` here confirms the
/// PROTECTED branch was taken (the in-place fallback hard-fails instead — see
/// the sibling test below) — and the crucial assertion is that the source
/// directory was never touched.
/// What: creates a real temp git repo with a `github.com` origin remote (so
/// `derive_project` recognizes it), calls `start_session` against it with an
/// unreachable daemon URL, and asserts `.trusty-mpm/` and
/// `.claude/settings.json` were never created inside the repo.
/// Test: this function IS the test.
#[tokio::test]
async fn session_start_dispatches_managed_new_for_github_repo() {
    let tmp = tempfile::TempDir::new().expect("tmp repo");
    let repo = tmp.path();
    git(repo, &["init", "-q"]);
    git(
        repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/example-owner/example-repo.git",
        ],
    );

    let client = reqwest::Client::new();
    let result = start_session(
        &client,
        UNREACHABLE_URL,
        Some(repo.to_string_lossy().to_string()),
    )
    .await;

    // The managed path never hard-errors on daemon-unreachable (it renders a
    // `CommandResult::Error` and returns, matching `session new`'s existing
    // soft-fail behavior via `managed_route::run`) — so `Ok` here proves the
    // PROTECTED branch was taken, not the in-place fallback (which hard-fails
    // — see the sibling test).
    assert!(
        result.is_ok(),
        "expected soft-fail Ok from the managed route, got {result:?}"
    );

    // The #1916 regression this fix closes: no tm artifacts written into the
    // live source checkout.
    assert!(
        !repo.join(".trusty-mpm").exists(),
        "session start must not write .trusty-mpm/ into a protected GitHub repo"
    );
    assert!(
        !repo.join(".claude").join("settings.json").exists(),
        "session start must not write .claude/settings.json into a protected GitHub repo"
    );
}

/// #1916: a directory that is NOT a recognized git repo has no live source
/// tree to protect, so `session start` must preserve its pre-#1916 in-place
/// behavior exactly — deploy `prepare_session` into the directory, then
/// hard-fail (propagate `Err`) when the daemon is unreachable, matching the
/// original `?`-propagating `POST /sessions` call.
///
/// Why: proves the "just run claude here, no segregation" case ([`start_session`]'s
/// doc explains why it remains supported — it mirrors what `tm connect`
/// already documents) was not silently broken by the #1916 routing change.
/// Calls [`start_session_in_place`] directly with a hermetic
/// `FrameworkPaths::under(tempdir)` (never `FrameworkPaths::default()`) so the
/// test cannot write into the developer's/CI's real `~/.claude`/`~/.trusty-mpm`.
/// What: creates a plain (non-git) temp directory, calls
/// `start_session_in_place` with an unreachable daemon URL, and asserts (1) it
/// returns `Err` (the original hard-fail-on-unreachable-daemon behavior) and
/// (2) `<dir>/.trusty-mpm/last-instructions.md` was written BEFORE the failed
/// POST — proving `prepare_session` still ran in place.
/// Test: this function IS the test.
#[tokio::test]
async fn session_start_in_place_writes_stash_and_hard_fails_on_daemon_unreachable() {
    let tmp_home = tempfile::TempDir::new().expect("tmp home");
    let target = tempfile::TempDir::new().expect("tmp target dir");
    let fw = trusty_mpm::core::paths::FrameworkPaths::under(tmp_home.path());

    let client = reqwest::Client::new();
    let result = start_session_in_place(&client, UNREACHABLE_URL, target.path(), &fw).await;

    // Preserves the original in-place `Start` behavior: a daemon-unreachable
    // `POST /sessions` propagates as a hard `Err` via `?`.
    assert!(
        result.is_err(),
        "in-place start must hard-fail when the daemon is unreachable (pre-#1916 behavior)"
    );

    // `prepare_session` ran (and wrote its stash) BEFORE the failed POST,
    // proving the in-place deploy behavior is preserved.
    assert!(
        target
            .path()
            .join(".trusty-mpm")
            .join("last-instructions.md")
            .exists(),
        "in-place start must still run prepare_session and stash instructions locally"
    );
}

/// Start a minimal in-process HTTP server that captures the JSON body of the
/// first `POST /api/v1/sessions/managed` it receives, then answers with a
/// well-formed `ManagedSpawnResponse` so the client-side deserialization in
/// [`super::start_session`]'s protected branch succeeds.
///
/// Why: proving the #1916 item-3 "bare `tm` shortcuts to the same protected
/// flow" claim needs to compare the ACTUAL bytes `session start` puts on the
/// wire against the literal JSON `guided_launch::launch_new_session_and_attach`
/// sends — a real (but tiny, hermetic) HTTP listener is the only way to
/// capture that without depending on an external mocking crate (none is a
/// dev-dependency of this crate).
/// What: binds an ephemeral loopback port, serves one `axum` route via a
/// background task, and returns `(captured, url)` where `captured` is filled
/// in by the handler once a request lands.
/// Test: `session_start_posts_the_same_wire_shape_bare_tm_guided_default_sends`.
async fn spawn_capturing_managed_spawn_server() -> (
    std::sync::Arc<std::sync::Mutex<Option<serde_json::Value>>>,
    String,
) {
    use axum::{Json, Router, routing::post};

    let captured: std::sync::Arc<std::sync::Mutex<Option<serde_json::Value>>> =
        std::sync::Arc::default();
    let captured_for_handler = std::sync::Arc::clone(&captured);

    let handler = move |Json(body): Json<serde_json::Value>| {
        let captured = std::sync::Arc::clone(&captured_for_handler);
        async move {
            *captured.lock().expect("captured mutex poisoned") = Some(body);
            Json(serde_json::json!({
                "id": "11111111-1111-1111-1111-111111111111",
                "name": "tmpm-test-session",
                "state": "Active",
                "runtime": "claude-code",
                "attach_cmd": "tmux attach -t tmpm-test-session",
            }))
        }
    };
    let router = Router::new().route("/api/v1/sessions/managed", post(handler));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        axum::serve(listener, router).await.ok();
    });

    (captured, format!("http://{addr}"))
}

/// #1916 item 3: `session start`'s protected-path branch must POST the SAME
/// `repo_url`/`ref`/`task` values to `POST /api/v1/sessions/managed` that the
/// bare `tm` guided default's "launch new" picker choice sends via
/// `commands::guided_launch::launch_new_session_and_attach` — see that
/// function's `client.post(...).json(&serde_json::json!({ "repo_url":
/// repo_url, "ref": "HEAD", "task": "" }))` call, which this test's expected
/// value mirrors literally for those three fields. Proving matching requests
/// (rather than just "both eventually call `spawn_managed`") is the strongest
/// guarantee the two entry points cannot silently diverge in the future.
///
/// Why: closes the loop on "bare `tm` in a git repo shortcuts to the
/// (now-fixed) `session start` flow" — both surfaces already funnel through
/// `POST /api/v1/sessions/managed`, so this test locks in that they do so
/// with the same `repo_url`/`ref`/`task`.
/// What: spins up [`spawn_capturing_managed_spawn_server`], calls
/// `start_session` against a temp GitHub-remote git repo, then asserts the
/// captured JSON body's `repo_url`/`ref`/`task` equal `<git root>`/`"HEAD"`/
/// `""`. `session start` additionally sends an explicit `runtime` key (via
/// `RuntimeKind::default()`, since [`crate::commands::managed_route::to_command`]
/// always wraps it `Some(...)`) where the guided default omits the key and
/// lets the daemon apply its own default — both resolve to `claude-code`, so
/// this is asserted as part of the expected body too, documented as the one
/// known (semantically inert) spelling difference between the two callers.
/// Test: this function IS the test.
#[tokio::test]
async fn session_start_posts_the_same_wire_shape_bare_tm_guided_default_sends() {
    let tmp = tempfile::TempDir::new().expect("tmp repo");
    let repo = tmp.path().canonicalize().expect("canonicalize tmp repo");
    git(&repo, &["init", "-q"]);
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/example-owner/example-repo.git",
        ],
    );

    let (captured, url) = spawn_capturing_managed_spawn_server().await;

    let client = reqwest::Client::new();
    let result = start_session(&client, &url, Some(repo.to_string_lossy().to_string())).await;
    assert!(
        result.is_ok(),
        "expected Ok from a successful spawn, got {result:?}"
    );

    let body = captured
        .lock()
        .expect("captured mutex poisoned")
        .clone()
        .expect("session start must have POSTed to /api/v1/sessions/managed");

    // The three fields that decide WHERE and WHAT gets spawned (`repo_url`,
    // `ref`, `task`) are byte-for-byte the same values
    // `launch_new_session_and_attach` sends for the equivalent bare-`tm`
    // "launch new" choice: repo_url = git root, ref = "HEAD", task = "" for
    // an interactive session. `session start`'s route additionally sends an
    // explicit `runtime` key (via `RuntimeKind::default()`) where the guided
    // default omits it and lets the daemon default — semantically identical
    // (both resolve to `claude-code`), just spelled differently on the wire,
    // so it is asserted separately below rather than folded into `expected`.
    // #2450: both `session start` (this request) and the picker's "launch new
    // session" (`launch_new_session_and_attach`) now send `force_new: true` —
    // an explicit launch verb must never adopt an existing live session for the
    // same project. The two surfaces stay identical on this field too (#1916).
    let expected = serde_json::json!({
        "repo_url": repo.to_string_lossy(),
        "ref": "HEAD",
        "task": "",
        "runtime": "claude-code",
        "force_new": true,
    });
    assert_eq!(
        body, expected,
        "session start's protected-path request must match bare tm's launch_new_session_and_attach wire shape"
    );
}
