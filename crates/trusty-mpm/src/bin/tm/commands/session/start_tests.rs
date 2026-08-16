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

// #5544: there is no `$HOME` guard here any more. The two tests below used to
// repoint the process's `$HOME` at a tempdir behind `#[serial]`, because
// `prepare_session` reached the real `~/.claude.json` and `~/.claude/settings.json`
// through `dirs::home_dir()` — a resolution path the `fw` parameter they already
// isolated with `FrameworkPaths::under(tempdir)` had no say over.
//
// The home is now a PARAMETER: `prepare_session_with_home` takes it, and
// `start_session_in_place` threads it through. Production passes
// `dirs::home_dir()`, byte-identical to before under every framework root.
//
// 🔴 Do NOT "simplify" this by resolving the home from `fw` instead.
// `FrameworkPaths::claude_home_dir()` still exists and looks like the obvious
// answer; it is not. `for_managed_project` rewrites `claude_agents`, which that
// accessor derives from, so for every managed session it returns the WORKSPACE —
// which drops an untracked `.claude.json` into the operator's repo and points the
// global hook cleanup at a file that does not exist. A missing file is success by
// contract, so nothing reports it. That defect shipped twice in this PR's review
// and cost two rounds; the parameter is what makes it unreachable.
//
// The env write had to go rather than be serialised: `cargo test` runs a
// target's tests as threads in ONE process, so `$HOME` was repointed for every
// sibling for the duration, and `#[serial]` excludes only other `#[serial]`
// tests. `prepare_session_does_not_seed_the_workspace_on_the_managed_path` and
// `global_hook_cleanup_reaches_the_real_home_under_an_overridden_root`
// (`core::session_launch::tests`) are the regression tests for the escape.

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
/// the fix without requiring a live daemon. Before #2457, `managed_route::run`
/// soft-failed (printed a `CommandResult::Error`, returned `Ok(())`) on a
/// daemon-unreachable spawn — a `tm session start` against a down daemon
/// silently reported success. #2457 fixed `run` to propagate that
/// `CommandResult::Error` as `Err`, so this test now asserts `Err` (matching
/// the in-place fallback's existing hard-fail — see the sibling test) while
/// still proving the PROTECTED branch was taken via the crucial assertion:
/// the source directory was never touched.
/// What: creates a real temp git repo with a `github.com` origin remote (so
/// `derive_project` recognizes it), calls `start_session` against it with an
/// unreachable daemon URL, and asserts (1) it returns `Err` and (2)
/// `.trusty-mpm/` and `.claude/settings.json` were never created inside the
/// repo.
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

    // #2457: a daemon-unreachable spawn is a genuine failure and must
    // propagate as `Err` (not the pre-#2457 soft-fail `Ok`) — proving the
    // PROTECTED branch was taken (the in-place fallback also hard-fails, but
    // via a different code path — see the sibling test).
    assert!(
        result.is_err(),
        "expected a hard Err from the managed route on daemon-unreachable, got {result:?}"
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
    // #5544: the home is INJECTED. Production passes `dirs::home_dir()`; this
    // passes the same tempdir `fw` is rooted at, which is what lets the test
    // stop repointing the process's `$HOME`.
    let result = start_session_in_place(
        &client,
        UNREACHABLE_URL,
        target.path(),
        &fw,
        Some(tmp_home.path()),
    )
    .await;

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
/// in by the handler once a request lands. `answer` selects what the handler
/// replies with AFTER capturing (#4965): [`StatusCode::OK`] plus the ack for a
/// caller that must continue past the POST, or any error status for a caller
/// whose only subject is the request body — `launch_new_session_and_attach`
/// answered `200` would go on to run a REAL `tmux attach`, so its wire tests
/// use `500` to make the function return the moment the body is captured.
/// Test: `session_start_posts_the_same_wire_shape_bare_tm_guided_default_sends`,
/// `launch_new_session_and_attach_sends_the_name_hint`,
/// `launch_new_session_and_attach_omits_name_hint_when_unnamed`.
async fn spawn_capturing_managed_spawn_server_answering(
    answer: axum::http::StatusCode,
) -> (
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
            (
                answer,
                Json(serde_json::json!({
                    "id": "11111111-1111-1111-1111-111111111111",
                    "name": "tmpm-test-session",
                    "state": "Active",
                    "runtime": "claude-code",
                    "attach_cmd": "tmux attach -t tmpm-test-session",
                })),
            )
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

/// [`spawn_capturing_managed_spawn_server_answering`] with the success answer
/// — the shape every caller wanted before #4965 added the reject variant.
async fn spawn_capturing_managed_spawn_server() -> (
    std::sync::Arc<std::sync::Mutex<Option<serde_json::Value>>>,
    String,
) {
    spawn_capturing_managed_spawn_server_answering(axum::http::StatusCode::OK).await
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

// ── #4832: a launch outside any git repository is refused ──────────────────

/// A directory that is not inside any git working tree must be refused, not
/// deployed into.
///
/// Why (#4832): `resolve_dir(None)` returns the process cwd, a non-git
/// directory routed to `start_session_in_place`, and `prepare_session` then
/// wrote `.trusty-mpm/`, a `CLAUDE.md` stub and a `.claude/` tier wherever the
/// operator's shell happened to be standing. Harness state belongs to a
/// project; outside a repository there is no project to attach it to.
///
/// FAILS BEFORE THIS CHANGE: `start_session` fell through to the in-place path
/// and the directory grew `.trusty-mpm/`.
/// What: runs `start_session` against a plain temp directory with an
/// unreachable daemon URL, and asserts (1) an error naming the directory and
/// (2) that nothing was written into it.
#[tokio::test]
async fn session_start_refuses_a_non_git_directory() {
    let tmp = tempfile::TempDir::new().expect("tmp dir");
    let plain = tmp.path();

    let client = reqwest::Client::new();
    let err = start_session(
        &client,
        UNREACHABLE_URL,
        Some(plain.to_string_lossy().to_string()),
    )
    .await
    .expect_err("a non-git directory must be refused");

    let msg = err.to_string();
    assert!(
        msg.contains("not inside a git repository"),
        "must say why: {msg}"
    );
    assert!(msg.contains("git init"), "must point at a remedy: {msg}");
    assert!(
        !plain.join(".trusty-mpm").exists(),
        "no harness state may be scattered into a non-project directory"
    );
    assert!(
        !plain.join("CLAUDE.md").exists(),
        "no CLAUDE.md stub may be seeded into a non-project directory"
    );
}

/// The guard must not fire for a real repository — including one with no
/// remote, which still routes to the in-place path.
///
/// Why: the refusal is about "no project", not "no GitHub remote". A guard
/// that also rejected a remote-less repo would break the documented in-place
/// case `start_session` preserves.
/// What: asserts [`super::refuse_outside_a_git_project`] accepts a repo root,
/// a subdirectory of it, and rejects a plain directory.
#[test]
fn session_start_accepts_a_git_directory() {
    let tmp = tempfile::TempDir::new().expect("tmp");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    let sub = repo.join("src");
    std::fs::create_dir_all(&sub).unwrap();

    super::refuse_outside_a_git_project(&repo).expect("a repo root is accepted");
    super::refuse_outside_a_git_project(&sub).expect("a subdirectory is accepted");

    let plain = tmp.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    assert!(
        super::refuse_outside_a_git_project(&plain).is_err(),
        "a non-git directory is refused"
    );
}

// ── #4965: the picker's `n <name>` name_hint on the wire ───────────────────

/// Drive [`crate::commands::guided_launch::launch_new_session_and_attach`]
/// against the capturing server and return the request body it POSTed.
///
/// Why: the `name_hint` plumbing had no test of its own — `session start`'s
/// wire-shape test only pins a hand-copied MIRROR of this body, so a wrong key
/// name, a wrong JSON type, or a key leaking on the `None` path would all have
/// gone unnoticed. This drives the REAL function.
/// What: answers `500` after capturing so the function returns immediately
/// after the POST instead of continuing into `tmux_attach`/`provision-status`
/// (the returned `Err` is expected and ignored — the request body is the
/// entire subject). `repo_url` is a non-directory string, so
/// `needs_first_run_clone` short-circuits without touching the filesystem.
/// Test: `launch_new_session_and_attach_sends_the_name_hint`,
/// `launch_new_session_and_attach_omits_name_hint_when_unnamed`,
/// `launch_new_session_and_attach_requests_a_worktree_when_asked`.
async fn capture_guided_launch_body(
    name_hint: Option<&str>,
    isolation: crate::commands::picker_launch_new::LaunchIsolation,
) -> serde_json::Value {
    let (captured, url) = spawn_capturing_managed_spawn_server_answering(
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
    )
    .await;
    let client = reqwest::Client::new();
    let result = crate::commands::guided_launch::launch_new_session_and_attach(
        &client,
        &url,
        "https://example.invalid/owner/repo.git",
        name_hint,
        isolation,
    )
    .await;
    assert!(
        result.is_err(),
        "the harness answers 500 so the call stops at the POST; got {result:?}"
    );

    captured
        .lock()
        .expect("captured mutex poisoned")
        .clone()
        .expect("launch_new_session_and_attach must have POSTed to /api/v1/sessions/managed")
}

/// #4965: a named launch must put the operator's leaf on the wire under the
/// key the daemon reads (`name_hint`), as a JSON string.
///
/// FAILS BEFORE THIS CHANGE: nothing exercised this function's body at all —
/// renaming the key to `nameHint`, or sending it as a number, kept every test
/// green.
/// What: captures the real POST body for `Some("auth")`.
#[tokio::test]
async fn launch_new_session_and_attach_sends_the_name_hint() {
    let body = capture_guided_launch_body(
        Some("auth"),
        crate::commands::picker_launch_new::LaunchIsolation::SessionCheckout,
    )
    .await;
    assert_eq!(
        body.get("name_hint"),
        Some(&serde_json::Value::String("auth".to_string())),
        "the named launch must send name_hint as a JSON string: {body}"
    );
}

/// #4965: the unnamed launch must OMIT the key entirely — not send `null`.
///
/// Why: the daemon's `Option<String>` deserialization accepts a missing key
/// and a `null` alike, but `session start`'s wire-shape test asserts the two
/// surfaces' bodies are EQUAL, so a leaked `null` here would be a silent
/// divergence between the two spawn entry points (#1916 item 3).
/// What: captures the real POST body for `None` and asserts the key is absent,
/// then that the rest of the body is unchanged.
#[tokio::test]
async fn launch_new_session_and_attach_omits_name_hint_when_unnamed() {
    let body = capture_guided_launch_body(
        None,
        crate::commands::picker_launch_new::LaunchIsolation::SessionCheckout,
    )
    .await;
    assert!(
        body.get("name_hint").is_none(),
        "the unnamed launch must omit name_hint entirely, not send null: {body}"
    );
    assert_eq!(
        body,
        serde_json::json!({
            "repo_url": "https://example.invalid/owner/repo.git",
            "ref": "HEAD",
            "task": "",
            "force_new": true,
            "background": true,
        }),
        "the unnamed request's wire shape must be unchanged by #4965 or #5773"
    );
}

/// #5773: `n <name> --worktree` must put the isolation request on the wire
/// under the key the daemon reads (`worktree`), as a JSON `true`.
///
/// Why: the picker's launch-new path built a body with no `worktree` key at
/// all, so `SpawnRequest`'s `#[serde(default)]` decoded `false` and
/// `spawn_managed_routed` took the main-checkout branch for every picker
/// launch — the operator had no way to reach the worktree branch that
/// `tm launch --worktree` reaches. Sending it under a different spelling would
/// be silently ignored the same way.
///
/// FAILS BEFORE THIS CHANGE: the body carried no `worktree` key at all.
/// What: captures the real POST body for an isolation-requesting launch. The
/// default path's absence of the key is pinned by
/// `launch_new_session_and_attach_omits_name_hint_when_unnamed`'s equality
/// assertion above.
#[tokio::test]
async fn launch_new_session_and_attach_requests_a_worktree_when_asked() {
    let body = capture_guided_launch_body(
        Some("auth"),
        crate::commands::picker_launch_new::LaunchIsolation::OwnWorktree,
    )
    .await;
    assert_eq!(
        body.get("worktree"),
        Some(&serde_json::Value::Bool(true)),
        "an isolation request must send worktree: true: {body}"
    );
}
