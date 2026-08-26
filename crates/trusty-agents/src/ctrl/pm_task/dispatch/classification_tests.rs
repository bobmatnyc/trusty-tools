//! Tests for `classification` — split out following the `persona.rs`/
//! `persona_tests.rs` pattern already established in this directory (keeps
//! `classification.rs` well clear of the 500-SLOC production cap).
//! What: pure-function tests for `parse_marker`/`is_valid_label`/
//! `classification_block`/`bleed_nudge`/`should_refresh_summary`, hermetic
//! tests for `build_turn_context`/`finish_turn` against a no-project-root
//! tempdir (no network, no LLM credentials required — every code path that
//! would need them short-circuits before reaching them), plus a
//! mock-daemon integration test proving the detached-persistence gate
//! (#3840 critic HIGH-2/HIGH-4).
//! Test: This module IS the test coverage.

use super::*;
use async_openai::{Client, config::OpenAIConfig};
use std::time::Duration;

/// A socket path nothing can be serving, for hermetic tests that must never
/// reach a daemon (paired with a project root that resolves to `None`, so
/// nothing in the call path attempts a connection at all).
///
/// #6286: this was a reserved-port URL; a path under a directory that cannot
/// exist is the socket equivalent, and unlike a port it cannot be taken by
/// something else on the machine.
fn dead_socket() -> &'static std::path::Path {
    std::path::Path::new("/nonexistent/trusty-memory/trusty-memory.sock")
}

fn test_client() -> Client<OpenAIConfig> {
    Client::with_config(OpenAIConfig::new())
}

/// A client pointed at a local mock daemon's `/chat/completions` route
/// (#3867) — `with_api_base` deliberately omits `/v1` so
/// `create_chat_completion_lenient`'s `config.url("/chat/completions")`
/// lands exactly on `mock_daemon`'s route, matching the raw-stub pattern in
/// `workflow::resolver_tests::serve_once`.
fn test_client_with_base(base: &str) -> Client<OpenAIConfig> {
    let cfg = OpenAIConfig::new()
        .with_api_key("test-key")
        .with_api_base(base);
    Client::with_config(cfg)
}

fn test_persona_cfg() -> AgentConfig {
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
    toml::from_str(toml_str).expect("parses")
}

fn test_persona_cfg_disabled() -> AgentConfig {
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"

[workstreams]
enabled = false
"#;
    toml::from_str(toml_str).expect("parses")
}

// ---------------------------------------------------------------------
// is_valid_label
// ---------------------------------------------------------------------

#[test]
fn is_valid_label_accepts_kebab_case() {
    for label in ["feat-x", "a", "pr-review-tips", "abc123-def"] {
        assert!(is_valid_label(label), "expected valid: {label}");
    }
}

#[test]
fn is_valid_label_rejects_placeholder_and_unsafe_text() {
    for label in [
        "",
        "<label>",
        "Feat-X",               // uppercase
        "feat_x",               // underscore, not hyphen
        "feat x",               // space
        "the format is [[task", // stray syntax fragment
        &"x".repeat(65),        // over LABEL_MAX_LEN
    ] {
        assert!(!is_valid_label(label), "expected invalid: {label}");
    }
}

// ---------------------------------------------------------------------
// parse_marker
// ---------------------------------------------------------------------

#[test]
fn parse_marker_extracts_existing_label() {
    let raw = "Here's your answer.\n\n[[task: feat-x]]";
    let (display, c) = parse_marker(raw);
    assert_eq!(display, "Here's your answer.");
    let c = c.expect("classification present");
    assert_eq!(c.label, "feat-x");
    assert!(!c.is_new);
}

#[test]
fn parse_marker_extracts_new_label() {
    let raw = "Sure, I'll help with that.\n\n[[task: new: onboarding-flow]]";
    let (display, c) = parse_marker(raw);
    assert_eq!(display, "Sure, I'll help with that.");
    let c = c.expect("classification present");
    assert_eq!(c.label, "onboarding-flow");
    assert!(c.is_new);
}

#[test]
fn parse_marker_missing_is_none() {
    let raw = "Just a plain response with no marker.";
    let (display, c) = parse_marker(raw);
    assert_eq!(display, raw);
    assert!(c.is_none());
}

#[test]
fn parse_marker_strips_surrounding_whitespace() {
    let raw = "Answer text.\n\n[[task:   feat-y   ]]  \n";
    let (display, c) = parse_marker(raw);
    assert_eq!(display, "Answer text.");
    assert_eq!(c.expect("classification present").label, "feat-y");
}

/// #3840 critic HIGH-1: the marker is anchored to the response's own LAST
/// LINE, not "last occurrence anywhere" — a marker-shaped fragment embedded
/// mid-sentence (not itself the trailing line) must never be mistaken for
/// the real decision, and must NOT be stripped from the displayed text.
#[test]
fn parse_marker_ignores_earlier_lookalike_text() {
    let raw = "I'll end with `[[task: <label>]]` as instructed.\n\n[[task: feat-z]]";
    let (display, c) = parse_marker(raw);
    assert_eq!(display, "I'll end with `[[task: <label>]]` as instructed.");
    assert_eq!(c.expect("classification present").label, "feat-z");
}

#[test]
fn parse_marker_empty_label_is_none() {
    let raw = "Answer.\n\n[[task: ]]";
    let (_, c) = parse_marker(raw);
    assert!(c.is_none());
}

/// #3840 critic HIGH-1, case 2: a persona answering "how does classification
/// work?" whose LAST line happens to be exactly the marker syntax echoing
/// the literal `<label>` placeholder must NOT have `<label>` persisted as a
/// real tag — `is_valid_label` rejects it, so `finish_turn` never calls
/// `create_tagged_drawer_at` for this turn. The line is still stripped from
/// display (it structurally matches the marker the model was instructed to
/// emit — see `parse_marker`'s doc comment for the documented tradeoff).
#[test]
fn parse_marker_trailing_syntax_echo_is_stripped_but_unclassified() {
    let raw = "The exact format to end with is:\n[[task: <label>]]";
    let (display, c) = parse_marker(raw);
    assert_eq!(display, "The exact format to end with is:");
    assert!(
        c.is_none(),
        "a literal placeholder label must never classify as real"
    );
}

/// A marker-shaped fragment that is NOT anchored to the response's own last
/// line (mid-sentence, more prose follows on the same/later line) must be
/// left untouched — this is `parse_marker_ignores_earlier_lookalike_text`'s
/// single-line sibling: the whole response is one line that merely CONTAINS
/// marker syntax without being the trailing marker.
#[test]
fn parse_marker_ignores_trailing_syntax_echo_mid_sentence() {
    let raw = "You should end your response with something like [[task: <label>]] to classify it.";
    let (display, c) = parse_marker(raw);
    assert_eq!(display, raw);
    assert!(c.is_none());
}

/// #3840 critic HIGH-1, case 1: a persona that emits the marker TWICE (e.g.
/// a stray duplicate) must not leave the FIRST occurrence visible in the
/// displayed text. Documented choice: BOTH trailing marker lines are
/// stripped from display; the LAST one (closest to the true end of the
/// response) is authoritative for classification — not the first.
#[test]
fn parse_marker_double_marker_strips_both_last_wins() {
    let raw = "Answer text.\n\n[[task: feat-x]]\n[[task: feat-y]]";
    let (display, c) = parse_marker(raw);
    assert_eq!(
        display, "Answer text.",
        "both trailing marker lines must be stripped from the display"
    );
    assert_eq!(
        c.expect("classification present").label,
        "feat-y",
        "the LAST marker line wins, not the first"
    );
}

// ---------------------------------------------------------------------
// classification_block
// ---------------------------------------------------------------------

#[test]
fn classification_block_lists_existing_labels() {
    let block = classification_block(&["feat-x".to_string(), "feat-y".to_string()]);
    assert!(block.contains("feat-x, feat-y"));
    assert!(block.contains("[[task: <label>]]"));
    assert!(block.contains("[[task: new: <label>]]"));
}

#[test]
fn classification_block_empty_labels_still_offers_new() {
    let block = classification_block(&[]);
    assert!(block.contains("(none yet)"));
    assert!(block.contains("new:"));
}

// ---------------------------------------------------------------------
// bleed_nudge
// ---------------------------------------------------------------------

#[test]
fn bleed_nudge_none_when_unfocused() {
    assert_eq!(bleed_nudge(None, "feat-x"), None);
}

#[test]
fn bleed_nudge_none_when_matching() {
    assert_eq!(bleed_nudge(Some("feat-x"), "feat-x"), None);
}

#[test]
fn bleed_nudge_some_when_different() {
    let nudge = bleed_nudge(Some("feat-x"), "feat-y").expect("nudge present");
    assert!(nudge.contains("feat-y"));
    assert!(nudge.contains("feat-x"));
}

// ---------------------------------------------------------------------
// build_turn_context / finish_turn — hermetic (no project root -> every
// trusty-memory call short-circuits to empty before any network attempt).
// ---------------------------------------------------------------------

#[tokio::test]
async fn build_turn_context_unfocused_has_no_context_block() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ctx = build_turn_context(tmp.path(), None, 12, true, dead_socket()).await;
    assert!(ctx.focused_context_block.is_none());
    assert!(ctx.focused_label.is_none());
    assert!(ctx.classification_block.contains("(none yet)"));
}

#[tokio::test]
async fn build_turn_context_focused_assembles_stable_order() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ctx = build_turn_context(tmp.path(), Some("feat-x"), 12, true, dead_socket()).await;
    assert_eq!(ctx.focused_label.as_deref(), Some("feat-x"));
    let block = ctx.focused_context_block.expect("focused block present");
    let global_pos = block
        .find("Global summary")
        .expect("global summary section");
    let ws_pos = block.find("Task summary").expect("task summary section");
    let recent_pos = block
        .find("Recent turns on this task")
        .expect("recent turns section");
    assert!(
        global_pos < ws_pos && ws_pos < recent_pos,
        "DOC-54 §9.6.3 stable order: global -> per-workstream summary -> recent turns"
    );
}

/// #3840 critic HIGH-2: `[workstreams].enabled = false` must be a REAL
/// master switch on `build_turn_context` — no vocabulary fetch, no
/// classification block, and focused-mode context is skipped entirely (even
/// though `focused = Some(...)` is passed, mirroring what a stale user
/// focus setting would look like on a disabled agent).
#[tokio::test]
async fn build_turn_context_disabled_is_a_real_no_op() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ctx = build_turn_context(tmp.path(), Some("feat-x"), 12, false, dead_socket()).await;
    assert_eq!(
        ctx.classification_block, "",
        "no classification block when disabled"
    );
    assert!(
        ctx.focused_label.is_none(),
        "disabled treats every turn as unfocused"
    );
    assert!(
        ctx.focused_context_block.is_none(),
        "no focused-mode assembly when disabled, even with focused=Some"
    );
}

#[tokio::test]
async fn finish_turn_no_marker_returns_unchanged() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let client = test_client();
    let cfg = test_persona_cfg();
    let ctx = build_turn_context(tmp.path(), None, 12, true, dead_socket()).await;
    let raw = "Plain response, no classification marker.".to_string();
    let out = finish_turn(
        tmp.path(),
        "izzie",
        &client,
        &cfg,
        "hi",
        raw.clone(),
        &ctx,
        dead_socket(),
    )
    .await
    .expect("finish_turn must not error even without a marker");
    assert_eq!(out, raw);
}

#[tokio::test]
async fn finish_turn_with_marker_and_no_project_root_still_returns_display_text() {
    // No project root -> the detached persistence task's `create_tagged_drawer_at`
    // fails (logged, not propagated) and `maybe_summarize_workstream` sees
    // zero turns and never attempts an LLM call — the whole thing stays
    // hermetic. `finish_turn` itself returns before that background task
    // even runs (#3840 critic HIGH-4: persistence is detached).
    let tmp = tempfile::tempdir().expect("tempdir");
    let client = test_client();
    let cfg = test_persona_cfg();
    let ctx = build_turn_context(tmp.path(), None, 12, true, dead_socket()).await;
    let raw = "Here you go.\n\n[[task: feat-x]]".to_string();
    let out = finish_turn(
        tmp.path(),
        "izzie",
        &client,
        &cfg,
        "hi",
        raw,
        &ctx,
        dead_socket(),
    )
    .await
    .expect("finish_turn must fail open on a persistence error");
    assert_eq!(out, "Here you go.");
}

#[tokio::test]
async fn finish_turn_bleed_nudge_appended_when_focused_mismatch() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let client = test_client();
    let cfg = test_persona_cfg();
    let ctx = build_turn_context(tmp.path(), Some("feat-a"), 12, true, dead_socket()).await;
    let raw = "Doing something else.\n\n[[task: feat-b]]".to_string();
    let out = finish_turn(
        tmp.path(),
        "izzie",
        &client,
        &cfg,
        "hi",
        raw,
        &ctx,
        dead_socket(),
    )
    .await
    .expect("finish_turn must not error");
    assert!(out.starts_with("Doing something else."));
    assert!(out.contains("feat-b"));
    assert!(out.contains("feat-a"));
}

/// #3840 critic HIGH-2: `[workstreams].enabled = false` gates
/// `finish_turn`'s persistence too, not just the summarizer — even with a
/// syntactically valid marker AND a real project root (so persistence would
/// otherwise be attempted), the disabled config must short-circuit before
/// ever spawning the background write. Proven against the mock daemon
/// below: zero drawers land, in contrast to the enabled case.
#[tokio::test]
async fn finish_turn_disabled_does_not_persist() {
    let (_addr, memory, state) = mock_daemon::spawn().await;
    let socket = memory.socket();
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(tmp.path().join(".git")).expect("mkdir .git");

    let client = test_client();
    let cfg = test_persona_cfg_disabled();
    let ctx = build_turn_context(tmp.path(), None, 12, cfg.workstreams.enabled, socket).await;
    let raw = "Here you go.\n\n[[task: feat-x]]".to_string();
    let out = finish_turn(
        tmp.path(),
        "izzie",
        &client,
        &cfg,
        "hi",
        raw,
        &ctx,
        socket,
    )
    .await
    .expect("finish_turn must not error");
    assert_eq!(out, "Here you go.", "marker still stripped from display");

    // Give a would-be background task every chance to run; there must be
    // none to run since the gate returns before ever spawning it.
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        state.drawer_count(),
        0,
        "disabled config must never persist a drawer"
    );
}

/// #3840 critic HIGH-4: persistence (the turn drawer write) happens off the
/// response path — `finish_turn` returns the display text without awaiting
/// the write, and the write lands a beat later via the detached task. This
/// proves the END STATE (the drawer eventually exists) and the GATE
/// (enabled=true persists; the sibling `finish_turn_disabled_does_not_persist`
/// proves enabled=false does not) — see that test's doc comment for why
/// asserting synchronous-vs-async ORDERING directly would be flaky; the
/// functional gating is what matters for the demo's data-integrity concern.
#[tokio::test]
async fn finish_turn_persists_via_detached_task() {
    let (_addr, memory, state) = mock_daemon::spawn().await;
    let socket = memory.socket();
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(tmp.path().join(".git")).expect("mkdir .git");

    let client = test_client();
    let cfg = test_persona_cfg();
    let ctx = build_turn_context(tmp.path(), None, 12, true, socket).await;
    let raw = "Here you go.\n\n[[task: feat-x]]".to_string();
    let out = finish_turn(
        tmp.path(),
        "izzie",
        &client,
        &cfg,
        "hi",
        raw,
        &ctx,
        socket,
    )
    .await
    .expect("finish_turn must not error");
    assert_eq!(out, "Here you go.");

    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        state.drawer_count(),
        1,
        "enabled config must persist exactly one drawer via the detached task"
    );
    assert!(state.drawer_tags_contain("ws:feat-x"));
}

// ---------------------------------------------------------------------
// #3867 (epic #3866 Slice A): agents-ws-summary compression telemetry
// ---------------------------------------------------------------------

/// Why: issue #3867's acceptance criteria requires a
/// `summarize_every`-cadence-boundary turn to produce exactly one
/// `compression.jsonl` line under `<project_dir>/.trusty-agents/state/`,
/// with `surface_detail` equal to the workstream label — proving the new
/// instrumentation in `maybe_summarize_workstream` (not just the pure
/// `ws_summary_compression_record` builder tested in `compression.rs`) is
/// actually wired at the real call site. Drives `maybe_summarize_workstream`
/// directly (the exact seam classification.rs offers — see this module's
/// doc comment) against a mock daemon serving both the drawers-listing HTTP
/// route and a stubbed `/chat/completions` endpoint.
/// What: seeds exactly one drawer under `ws:feat-x` with
/// `summarize_every = 1` so `should_refresh_summary` fires on the very
/// first call (`count == 1`, `1 % 1 == 0`) — i.e. "driving past the cadence
/// boundary" in the smallest possible step. Asserts the resulting
/// `compression.jsonl` has exactly one line, `surface ==
/// "agents-ws-summary"`, `surface_detail == "feat-x"`, and
/// `tokens_before > tokens_after` (the common-case shrinkage the issue
/// requires be asserted, not just hoped for).
/// Test: this IS the test.
#[tokio::test]
async fn maybe_summarize_workstream_emits_exactly_one_compression_event() {
    let (addr, memory, state) = mock_daemon::spawn().await;
    let chat_base = format!("http://{addr}");
    let socket = memory.socket();
    state.seed_drawers(
        "ws:feat-x",
        &["Did substantial work on feat-x: wired the new endpoint, added tests, updated docs."],
    );
    state.set_chat_reply("Summary: feat-x endpoint shipped.");

    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(tmp.path().join(".git")).expect("mkdir .git");

    let client = test_client_with_base(&chat_base);
    let mut cfg = test_persona_cfg();
    cfg.workstreams.enabled = true;
    cfg.workstreams.summarize_every = 1;

    maybe_summarize_workstream(tmp.path(), socket, "feat-x", &client, &cfg)
        .await
        .expect("summary refresh should succeed against the mock daemon");

    let jsonl_path = tmp.path().join(".trusty-agents/state/compression.jsonl");
    let contents = tokio::fs::read_to_string(&jsonl_path)
        .await
        .expect("compression.jsonl should exist after one cadence-boundary refresh");
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "exactly one compression event for one cadence-boundary refresh"
    );
    let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(parsed["surface"], "agents-ws-summary");
    assert_eq!(parsed["surface_detail"], "feat-x");
    assert!(parsed["compression_path"].is_null());
    let tokens_before = parsed["tokens_before"].as_u64().unwrap();
    let tokens_after = parsed["tokens_after"].as_u64().unwrap();
    assert!(
        tokens_before > tokens_after,
        "expected the summary to be shorter than the source window: before={tokens_before} after={tokens_after}"
    );
}

/// #3885 code-critic MEDIUM (mirrors #3867's acceptance criteria and PR
/// #3880's `unwritable_data_dir_does_not_fail_the_loop` pattern): a broken
/// compression-telemetry sink must never fail the summary refresh itself —
/// the drawer write and the `tracing::info!` still succeed, only the
/// best-effort JSONL append silently fails. Simulates "unwritable" the same
/// way the sibling `rtk` test does: `.trusty-agents` pre-created as a plain
/// FILE so `append_compression`'s `create_dir_all`/`OpenOptions::open` both
/// fail.
#[tokio::test]
async fn maybe_summarize_workstream_survives_unwritable_sink() {
    let (addr, memory, state) = mock_daemon::spawn().await;
    let chat_base = format!("http://{addr}");
    let socket = memory.socket();
    state.seed_drawers(
        "ws:feat-y",
        &["Substantial progress on feat-y: schema migration landed, backfill running."],
    );
    state.set_chat_reply("Summary: feat-y migration in progress.");

    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(tmp.path().join(".git")).expect("mkdir .git");
    std::fs::write(tmp.path().join(".trusty-agents"), b"blocked").expect("block the sink dir");

    let client = test_client_with_base(&chat_base);
    let mut cfg = test_persona_cfg();
    cfg.workstreams.enabled = true;
    cfg.workstreams.summarize_every = 1;

    maybe_summarize_workstream(tmp.path(), socket, "feat-y", &client, &cfg)
        .await
        .expect("summary refresh must still succeed despite an unwritable telemetry sink");

    assert!(
        state.drawer_tags_contain("ws-summary:feat-y"),
        "the summary drawer itself must still be persisted"
    );
    assert!(
        !tmp.path().join(".trusty-agents/state").exists(),
        "the blocked .trusty-agents file must not have been silently replaced with a dir"
    );
}

// ---------------------------------------------------------------------
// should_refresh_summary — pure cadence decision (DOC-54 §9.6.2)
// ---------------------------------------------------------------------

#[test]
fn should_refresh_summary_skips_when_disabled() {
    assert!(!should_refresh_summary(false, 5, 5));
    assert!(!should_refresh_summary(false, 5, 10));
}

#[test]
fn should_refresh_summary_skips_zero_cadence() {
    assert!(!should_refresh_summary(true, 0, 5));
}

#[test]
fn should_refresh_summary_skips_zero_turns() {
    assert!(!should_refresh_summary(true, 5, 0));
}

#[test]
fn should_refresh_summary_skips_off_cadence() {
    assert!(!should_refresh_summary(true, 5, 4));
    assert!(!should_refresh_summary(true, 5, 6));
    assert!(!should_refresh_summary(true, 5, 11));
}

#[test]
fn should_refresh_summary_true_on_cadence_boundary() {
    assert!(should_refresh_summary(true, 5, 5));
    assert!(should_refresh_summary(true, 5, 10));
    assert!(should_refresh_summary(true, 1, 3));
}

// -----------------------------------------------------------------
// Minimal mock covering what `finish_turn`'s detached persistence task and
// `maybe_summarize_workstream` exercise. Two listeners, because the two things
// under test no longer share a transport (#6286): trusty-memory's
// `palace_create` / `memory.drawer_create` / `memory.drawers_list` over a Unix
// socket, and the OpenAI-compatible `/chat/completions` over HTTP, which is a
// third-party contract ADR-0032 does not touch. One `MockState` is shared by
// both so a test still inspects one place.
// -----------------------------------------------------------------
mod mock_daemon {
    use crate::uds_mock::{self, MockMemoryDaemon, RpcError};
    use axum::routing::post;
    use axum::{Json, Router};
    use axum::extract::State;
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex as StdMutex};

    #[derive(Default)]
    pub(super) struct MockState {
        tags: StdMutex<Vec<Vec<String>>>,
        // #3867: drawers returned by the `memory.drawers_list` listing, seeded
        // by `seed_drawers` before a `maybe_summarize_workstream` call —
        // separate from `tags` (which only records what got WRITTEN via
        // `memory.drawer_create`) since the summary test needs to seed what
        // gets READ back as the pre-summary turn window.
        seeded_drawers: StdMutex<Vec<serde_json::Value>>,
        // #3867: canned assistant content the mock `/chat/completions`
        // endpoint returns for `maybe_summarize_workstream`'s one-shot
        // summary call.
        chat_reply: StdMutex<String>,
    }

    impl MockState {
        pub(super) fn drawer_count(&self) -> usize {
            self.tags.lock().unwrap().len()
        }

        pub(super) fn drawer_tags_contain(&self, tag: &str) -> bool {
            self.tags
                .lock()
                .unwrap()
                .iter()
                .any(|tags| tags.iter().any(|t| t == tag))
        }

        /// Seed the drawers `drawers_by_tag_at` reads back — the pre-summary
        /// turn window `maybe_summarize_workstream` scans for its cadence
        /// decision and summary input.
        pub(super) fn seed_drawers(&self, tag: &str, contents: &[&str]) {
            let rows: Vec<serde_json::Value> = contents
                .iter()
                .map(|c| {
                    serde_json::json!({
                        "content": c,
                        "tags": [tag],
                        "created_at": "2026-07-24T00:00:00Z",
                    })
                })
                .collect();
            *self.seeded_drawers.lock().unwrap() = rows;
        }

        /// Set the canned assistant reply the mock chat-completions endpoint
        /// returns.
        pub(super) fn set_chat_reply(&self, reply: &str) {
            *self.chat_reply.lock().unwrap() = reply.to_string();
        }
    }

    /// POST `/chat/completions` — stands in for the OpenAI-compatible
    /// endpoint `llm::chat_adapter_aware`/`create_chat_completion_lenient`
    /// hits (`OpenAIConfig::url("/chat/completions")`, matching the
    /// `api_base` this module's `test_client_with_base` sets, which
    /// deliberately omits the `/v1` segment).
    async fn chat_completions(
        State(state): State<Arc<MockState>>,
        Json(_body): Json<serde_json::Value>,
    ) -> Json<serde_json::Value> {
        let content = state.chat_reply.lock().unwrap().clone();
        Json(serde_json::json!({
            "id": "chatcmpl-mock",
            "object": "chat.completion",
            "created": 0,
            "model": "x",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": content},
                "finish_reason": "stop",
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15},
        }))
    }

    /// The memory half, on a Unix socket.
    ///
    /// `memory.drawers_list` ignores its filters — tests seed exactly the rows
    /// a given call needs via `MockState::seed_drawers`, so no server-side
    /// filtering is required.
    async fn spawn_memory(state: Arc<MockState>) -> MockMemoryDaemon {
        uds_mock::spawn(move |method: &str, params: serde_json::Value| {
            let state = Arc::clone(&state);
            let method = method.to_string();
            Box::pin(async move {
                match method.as_str() {
                    "palace_create" => Ok(serde_json::json!({"ok": true})),
                    "memory.drawer_create" => {
                        let tags: Vec<String> = params["tags"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default();
                        state.tags.lock().unwrap().push(tags);
                        Ok(serde_json::json!({"ok": true}))
                    }
                    "memory.drawers_list" => Ok(serde_json::Value::Array(
                        state.seeded_drawers.lock().unwrap().clone(),
                    )),
                    other => Err(RpcError::method_not_found(other, &[])),
                }
            })
        })
        .await
    }

    pub(super) async fn spawn() -> (SocketAddr, MockMemoryDaemon, Arc<MockState>) {
        let state = Arc::new(MockState::default());
        let memory = spawn_memory(Arc::clone(&state)).await;

        let app = Router::new()
            .route("/chat/completions", post(chat_completions))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        (addr, memory, state)
    }
}
