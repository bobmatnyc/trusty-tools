//! M1 milestone acceptance suite (#2062, vision spec §9 Requirement 3 /
//! §13 Phase 1 Cut Line) — the merge gate for the whole control-plane
//! milestone.
//!
//! Why: §9 Requirement 3 requires each milestone to ship ONE cohesive
//! end-to-end suite driving the real CLI/API surface, over and above the
//! per-issue e2e slices (`tests/session_e2e.rs` #2054/#2055,
//! `tests/task_e2e.rs` #2056/#2058, `tests/cli_e2e.rs` #2059/#2060) that
//! each already validated their own slice. Those per-issue tests stay —
//! #2062 adds the milestone-level narrative that reads, top to bottom, as
//! "the cut line works": §13's verbatim scenario definition is `start
//! tcode serve --stdio, create a session via JSON-RPC, run an engineer
//! task, receive lifecycle/tool/session events in real-time, cancel the
//! session, inspect the full transcript, and replicate the same task
//! through the thin CLI` — every clause of that sentence maps to one test
//! (or one step within one test) below.
//!
//! What: [`m1_cutline_full_scenario_over_stdio`] is the PRIMARY cohesive
//! driver — session.create -> session.attach -> task.run (sessionful) ->
//! live tool/lifecycle events -> terminal `finished` -> session.get_transcript
//! — all over STDIO, with the REGRESSION BASELINE (the exact ordered event-kind
//! sequence, turn count/roles, tool-call set, and priced usage/cost for the
//! standard `TCODE_MOCK_LLM=echo` scripted task) pinned inline so a future
//! change to event ordering, turn count, or pricing fails this test loudly.
//! [`m1_cutline_full_scenario_over_http`] proves the SAME narrative works
//! over the HTTP + SSE transport ("cover both stdio and http" — §9
//! Requirement 2's own two-transport mandate, applied at the milestone
//! level). [`m1_cutline_cancel_path`] exercises `session.cancel`
//! (cooperative cancellation, #2056/#2059) as its own scenario — a session
//! cannot both finish AND be cancelled in one run, so this is necessarily a
//! separate test from the main narrative. [`m1_cutline_replay_via_thin_cli`]
//! is §13's closing clause: the SAME task, replayed through the real `tcode
//! run-task` CLI subprocess (#2060's thin client), proving the API and the
//! CLI are the same surface.
//!
//! No wall-clock assertions anywhere in this file — every wait is a bounded
//! read-until-terminal loop (mirroring every other e2e test in this crate),
//! never a fixed `sleep`. The regression baseline pins COUNTS, ORDER, and
//! STRUCTURE (event kinds, turn roles/tool-calls, seq contiguity, priced
//! cost — all deterministic functions of the fixed `TCODE_MOCK_LLM=echo`
//! script and the fixed pricing table), never timing.
//!
//! Test: this file IS the M1 acceptance suite (spec §9 R3); see `support`
//! for the shared process/protocol plumbing.

mod support;

use std::collections::HashSet;
use std::time::Duration;

use chrono::Utc;
use serde_json::{Value, json};
use support::{
    StdioSession, assert_envelopes_contiguous, find_session_event, open_sse, parse_sse_frames,
    project_with_agents, read_sse_until,
};
use trusty_code::events::{Event, SessionEventEnvelope};

/// The exact, deterministic ordered event-kind sequence a standard
/// `TCODE_MOCK_LLM=echo` `task.run` produces, from `session.create` through
/// the terminal `session_done` — THE regression baseline (#2062).
///
/// Why: pinned once, here, as the single source of truth both stdio and
/// HTTP scenario tests assert against, so the two transports can never
/// silently drift in what they report for the identical underlying run.
/// What: `session_started`/`session_status_changed` are `session.create`'s
/// own two events; the PM's `delegate_to_agent` call and the delegated
/// engineer's own `bash` call are then STRUCTURALLY ordered (not raced) by
/// the synchronous call chain — the PM's `tool_started` necessarily
/// precedes the engineer's, and the engineer's `tool_finished` necessarily
/// precedes the PM's own — followed by the terminal status transition and
/// `session_done`.
///
/// `context_budget` (epic #2343) is equally deterministic: the PM's loop
/// measures its working-context budget at EVERY turn boundary, before that
/// turn's LLM call, so one lands ahead of each of the PM's two turns. That
/// there are exactly TWO of them — and not one per engineer turn as well —
/// is this baseline's end-to-end proof of cadence's PM-only gating: the
/// delegated engineer's loop shares this very same event sink but keeps
/// `AgentLoopConfig.cadence: None`, so it never emits.
///
/// `index_readiness` is deliberately NOT in this sequence: it is produced by
/// the detached, fail-open index-warming thread, which races the run by
/// construction (that is the whole point — a slow trusty-search daemon must
/// never block a task). Its arrival point is therefore genuinely
/// nondeterministic, and pinning it into an ordered baseline would encode a
/// race as a contract. The readiness event's own content/state mapping is
/// pinned by `session::registry::registry_tests::record_index_readiness_*`
/// instead; see `filter_async_kinds`.
///
/// `log` is absent for the same reason (#5895): this suite runs on a tempdir
/// project root, so every turn's durable-memory write is dropped (#4638) and
/// the detached drain task reports the degradation out of band. Neither its
/// position nor its count is sequenced against the run — see
/// `filter_async_kinds` for the mechanism, and
/// `assert_memory_degradation_logs_well_formed` for what IS asserted about it.
///
/// (tcode streaming epic #3696, Gap A, Slice 1) Two `agent_message_delta`
/// entries were added by that slice: the delegated engineer's own final
/// (text, no-tool-call) turn emits one right after its `bash` `tool_finished`
/// and before that result unwinds back to the PM's `delegate_to_agent`
/// `tool_finished`; the PM's own final (text, no-tool-call) turn — the one
/// that ends the loop with no further tool call — emits the second, right
/// after the PM's turn-2 `context_budget` measurement and before the terminal
/// `session_status_changed`/`session_done`. Both are deterministic functions
/// of the fixed `TCODE_MOCK_LLM=echo` script (each loop's assistant turns are
/// scripted, not modeled), so they belong in the ordered baseline rather than
/// `filter_async_kinds`.
///
/// (Gap B — #4425) Each of those two became a PAIR. A text turn now publishes
/// its content incrementally with `done: false` and closes with one empty
/// `done: true` delta, so the stream carries two frames per turn rather than
/// one. The scripted `TCODE_MOCK_LLM=echo` transport has no native streaming,
/// so the shared adapter's buffered fallback replays each turn as exactly one
/// content delta — which is why the count is deterministic at two per text
/// turn. Against a real SSE provider the CONTENT frames multiply with the
/// token count; the terminal frame stays exactly one, which is the invariant
/// a consumer keys off.
const BASELINE_EVENT_KINDS: &[&str] = &[
    "session_started",
    "session_status_changed",
    "context_budget",      // PM turn 1 boundary (cadence measures before the call)
    "tool_started",        // PM: delegate_to_agent
    "tool_started",        // engineer: bash
    "tool_finished",       // engineer: bash
    "agent_message_delta", // engineer: final text turn — content (done: false)
    "agent_message_delta", // engineer: final text turn — terminal (done: true)
    "tool_finished",       // PM: delegate_to_agent
    "context_budget",      // PM turn 2 boundary
    "agent_message_delta", // PM: final text turn — content (done: false)
    "agent_message_delta", // PM: final text turn — terminal (done: true)
    "session_status_changed",
    "session_done",
];

/// Drop event kinds that are asynchronous BY CONSTRUCTION and therefore have
/// no deterministic position in the stream.
///
/// Why: both kinds below reach the stream from a detached task that nothing
/// sequences against the session lifecycle, so pinning either into an ordered
/// baseline would encode a race as a contract.
///
/// `index_readiness` comes from the detached index-warming thread, so it can
/// legitimately land anywhere relative to the run — including after
/// `session_done`.
///
/// `log` (#2425, emitted since #5895) carries the per-turn durable-memory
/// degradation warning. This suite's project root is a tempdir, so palace
/// auto-create is withheld (`PalaceCreation::Forbidden`, #4638) and every turn
/// takes the drop path in `session::memory_sink::drain` — a detached
/// `tokio::spawn`ed task. Its POSITION is unsequenced, and its COUNT varies
/// independently of that: `session::memory_outcome_reconciler` warns at
/// consecutive-failure thresholds `[1, 3]`, so one call can emit two warnings
/// when a run spans both, while a read loop that closes on `session_done`
/// before the drain reports sees none. Pinning it at any fixed position would
/// convert a hard failure into an intermittent one.
///
/// What: returns `kinds` minus every async kind, keeping the baseline a
/// statement about the run's DETERMINISTIC lifecycle spine. The dropped events
/// still exist and are still asserted — `index_readiness` content by
/// `session::registry::registry_tests::record_index_readiness_*`, and `log`
/// content unordered by [`assert_memory_degradation_logs_well_formed`].
fn filter_async_kinds<'a>(kinds: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    kinds
        .into_iter()
        .filter(|k| !matches!(*k, "index_readiness" | "log"))
        .collect()
}

/// Assert every `log` envelope in `events` is a well-formed durable-memory
/// degradation warning — unordered, and tolerant of any count including zero.
///
/// Why: [`filter_async_kinds`] drops `log` from the ordered baseline, which on
/// its own would discard #5895's coverage entirely. This keeps the assertion on
/// the part of the event that IS deterministic — its level and its message
/// vocabulary — while being structurally incapable of failing on the two things
/// that genuinely vary, how many warnings landed inside the read window and
/// where. Every count-sensitive or position-sensitive form of this check is a
/// latent flake; see [`filter_async_kinds`] for why.
/// What: zero `log` events passes — a reachable memory daemon keeps the session
/// durable, and the read loop can also close before the detached drain reports.
/// Any that DID land must be `level == "warn"` and open with the closed
/// `MemoryFailureCategory::PalaceEnsure` vocabulary, the only category a
/// tempdir-rooted run can produce (`session::registry_memory_sink`). A
/// regression that changed the level, the category, or the message shape fails
/// here.
///
/// `level` and `message` are fields of `Event::Log`, so they live under the
/// envelope's nested `event` object — only `kind`, `seq`, `at`, and
/// `session_id` sit at the top level (`events::SessionEventEnvelope`). Reading
/// them off the envelope yields `Null` for every real event (#5921).
/// Test: called by `m1_cutline_full_scenario_over_stdio` and
/// `m1_cutline_full_scenario_over_http`; the extraction itself is pinned
/// daemon-free by `degradation_log_events_never_move_the_ordered_baseline`.
fn assert_memory_degradation_logs_well_formed(events: &[Value]) {
    for envelope in events.iter().filter(|e| e["kind"] == "log") {
        let payload = &envelope["event"];
        assert_eq!(
            payload["level"], "warn",
            "the only log event this run can emit is the #2425 degradation \
             warning, which is warn-level: {envelope}"
        );
        let message = payload["message"].as_str().unwrap_or_default();
        assert!(
            message.starts_with("durable memory degraded: category=PalaceEnsure "),
            "a tempdir-rooted run's only log event is the PalaceEnsure \
             degradation warning (#4638): {envelope}"
        );
    }
}

/// The CI condition the live scenarios cannot be relied on to produce: a `log`
/// event landing inside the read window (#5895).
///
/// Why: the degradation warning only appears when the memory daemon cannot
/// serve the session's palace, which is CI's environment and not necessarily a
/// developer's — on a machine with a reachable daemon both scenario tests
/// observe zero `log` events, so a green run there says nothing about the case
/// that turned `main` red. This pins the two helpers' contract directly and
/// deterministically, with no daemon and no timing involved.
/// What: splices zero, one, and two warnings into the baseline stream at the
/// front, the middle, and past `session_done` — the counts the reconciler's
/// `[1, 3]` thresholds can emit and the positions an unsequenced detached task
/// can deliver them at — and asserts the filtered spine is the baseline every
/// time. Each warning is a REAL [`SessionEventEnvelope`] carrying a real
/// `Event::Log`, serialized through production serde rather than hand-written
/// as JSON: the envelope nests the payload under `event` while carrying `kind`
/// at the top level, and a hand-written fixture that flattened `level`/`message`
/// onto the envelope is exactly how the broken extraction in
/// [`assert_memory_degradation_logs_well_formed`] shipped green (#5921). The
/// `PalaceEnsure` token likewise comes from the production enum, so a rename
/// there fails here.
/// Test: this test.
#[test]
fn degradation_log_events_never_move_the_ordered_baseline() {
    let category = format!(
        "{:?}",
        trusty_code::session::MemoryFailureCategory::PalaceEnsure
    );
    let warning = |seq: u64, consecutive: u32| {
        let envelope = SessionEventEnvelope::new(
            "s1".to_string(),
            seq,
            Utc::now(),
            Event::Log {
                session_id: "s1".to_string(),
                level: "warn".to_string(),
                message: format!(
                    "durable memory degraded: category={category} total_failed_turns=2 \
                     consecutive_failed_turns={consecutive}"
                ),
            },
        );
        serde_json::to_value(&envelope).expect("envelope serializes")
    };
    let spine: Vec<Value> = BASELINE_EVENT_KINDS
        .iter()
        .map(|k| json!({"kind": k, "session_id": "s1"}))
        .collect();

    for warnings in [
        vec![],
        vec![warning(1, 1)],
        vec![warning(1, 1), warning(2, 3)],
    ] {
        for offset in [0, spine.len() / 2, spine.len()] {
            let mut events = spine.clone();
            for (i, w) in warnings.iter().enumerate() {
                events.insert(offset + i, w.clone());
            }
            let kinds: Vec<&str> = events.iter().map(|e| e["kind"].as_str().unwrap()).collect();
            assert_eq!(
                filter_async_kinds(kinds),
                BASELINE_EVENT_KINDS,
                "{} warning(s) spliced at offset {offset} must not move the baseline",
                warnings.len()
            );
            assert_memory_degradation_logs_well_formed(&events);
        }
    }
}

/// The exact transcript role sequence the standard mock script produces —
/// part of the regression baseline.
const BASELINE_TRANSCRIPT_ROLES: &[&str] = &["pm", "python-engineer", "python-engineer", "pm"];

/// §13 Phase 1 Cut Line, steps 1-6, driven end-to-end over STDIO — the
/// PRIMARY cohesive M1 acceptance test, with the regression baseline pinned
/// inline.
///
/// Why: this is the scenario definition itself, read top to bottom: start
/// the daemon, create a session over JSON-RPC, attach, run a task, observe
/// live events, reach a terminal state, inspect the transcript.
/// What: `session.create` (asserting its own replay-visible two-event
/// prefix), `session.attach` BEFORE running (so every subsequent event is
/// observed live, not just replayed), `task.run` targeting that EXISTING
/// session (the spec's "sessionful" mode — proving `task.run` and
/// `session.create` compose, not just `task.run`'s own auto-create path),
/// a bounded read-until-`session_done` loop, then `session.status` and
/// `session.get_transcript`. The REGRESSION BASELINE section asserts the
/// exact ordered event-kind sequence ([`BASELINE_EVENT_KINDS`]), the exact
/// transcript role sequence ([`BASELINE_TRANSCRIPT_ROLES`]), the tool-call
/// set, and the priced usage/cost (deterministic functions of the fixed
/// mock script + fixed pricing table — never wall-clock).
/// Test: this test.
#[tokio::test]
async fn m1_cutline_full_scenario_over_stdio() {
    // Step 1: start `tcode serve --stdio`.
    let project = project_with_agents();
    let mut daemon = StdioSession::spawn_with_mock_llm(project.path());

    // Step 2: create a session via JSON-RPC.
    let create_resp = daemon
        .call(
            1,
            "session.create",
            json!({"task": "M1 cut-line acceptance task"}),
        )
        .await;
    assert!(
        create_resp["error"].is_null(),
        "session.create failed: {create_resp}"
    );
    let session_id = create_resp["result"]["id"]
        .as_str()
        .expect("session.create must return an id")
        .to_string();
    assert_eq!(create_resp["result"]["status"], "running");

    // Attach BEFORE running, so every subsequent event arrives live.
    let attach_resp = daemon
        .call(2, "session.attach", json!({"session_id": session_id}))
        .await;
    assert!(
        attach_resp["error"].is_null(),
        "session.attach failed: {attach_resp}"
    );
    // Retain whole envelopes, not just their `kind`: the ordered baseline reads
    // the kinds, and `assert_memory_degradation_logs_well_formed` reads the
    // level/message of the async `log` events the baseline filters out.
    let mut events: Vec<Value> = attach_resp["result"]["events"]
        .as_array()
        .expect("attach must return a replay events array")
        .clone();
    let kinds = |events: &[Value]| -> Vec<String> {
        events
            .iter()
            .map(|e| e["kind"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        kinds(&events),
        vec!["session_started", "session_status_changed"],
        "session.create's own replay-visible event prefix"
    );

    // Step 3: run an engineer task via JSON-RPC (task.run), sessionful mode
    // (targeting the session just created, not auto-creating a new one).
    let run_resp = daemon
        .call(
            3,
            "task.run",
            json!({"task_description": "say hi", "session_id": session_id}),
        )
        .await;
    assert!(run_resp["error"].is_null(), "task.run failed: {run_resp}");
    assert_eq!(run_resp["result"]["session_id"], session_id);
    assert_eq!(run_resp["result"]["status"], "running");
    assert_eq!(
        run_resp["result"]["mode"], "daily-driver",
        "default mode with nothing configured"
    );

    // Step 4: receive lifecycle/tool/session events in real time until the
    // session reaches a terminal state — a bounded read loop, never a sleep.
    let mut iterations = 0;
    while !events.iter().any(|e| e["kind"] == "session_done") {
        iterations += 1;
        assert!(
            iterations < 30,
            "gave up waiting for session_done after {iterations} read rounds; kinds so far: {:?}",
            kinds(&events)
        );
        let lines = daemon.read_lines(20).await;
        assert!(
            !lines.is_empty(),
            "timed out waiting for more events; kinds so far: {:?}",
            kinds(&events)
        );
        for line in &lines {
            if let Some(envelope) = find_session_event(line, &session_id) {
                events.push(envelope);
            }
        }
    }

    // ── REGRESSION BASELINE: exact ordered event-kind sequence ──────────
    let observed = kinds(&events);
    assert_eq!(
        filter_async_kinds(observed.iter().map(String::as_str)),
        BASELINE_EVENT_KINDS,
        "event-kind sequence regressed from the pinned baseline"
    );
    // The `log` events the baseline filters out still owe their content (#5895).
    assert_memory_degradation_logs_well_formed(&events);

    // Step 5: reach terminal `finished` status.
    let status_resp = daemon
        .call(4, "session.status", json!({"session_id": session_id}))
        .await;
    assert!(
        status_resp["error"].is_null(),
        "session.status failed: {status_resp}"
    );
    assert_eq!(status_resp["result"]["status"], "finished");
    assert_eq!(status_resp["result"]["mode"], "daily-driver");

    // Step 6: inspect the transcript via JSON-RPC.
    let transcript_resp = daemon
        .call(
            5,
            "session.get_transcript",
            json!({"session_id": session_id}),
        )
        .await;
    assert!(
        transcript_resp["error"].is_null(),
        "session.get_transcript failed: {transcript_resp}"
    );
    let result = &transcript_resp["result"];
    let turns = result["turns"].as_array().expect("turns must be an array");

    // ── REGRESSION BASELINE: turn count, roles, tool calls, usage/cost ──
    assert_eq!(
        turns.len(),
        BASELINE_TRANSCRIPT_ROLES.len(),
        "turn count regressed from the pinned baseline: {turns:?}"
    );
    let roles: Vec<&str> = turns.iter().map(|t| t["role"].as_str().unwrap()).collect();
    assert_eq!(
        roles, BASELINE_TRANSCRIPT_ROLES,
        "transcript role sequence regressed from the pinned baseline"
    );
    let tool_calls: HashSet<&str> = turns
        .iter()
        .flat_map(|t| t["tool_calls"].as_array().unwrap())
        .map(|c| c.as_str().unwrap())
        .collect();
    assert_eq!(
        tool_calls,
        HashSet::from(["delegate_to_agent", "bash"]),
        "tool-call set regressed from the pinned baseline"
    );
    // Priced usage/cost: a deterministic function of the fixed mock
    // script's token counts and the fixed pricing table — never
    // wall-clock, so pinning the exact figures is safe and meaningful.
    assert_eq!(result["usage"]["prompt_tokens"], 80);
    assert_eq!(result["usage"]["completion_tokens"], 28);
    assert_eq!(result["cost_usd"], 0.00066);
    assert_eq!(result["mode"], "daily-driver");

    daemon.shutdown_via_eof_and_assert_clean_exit().await;
}

/// The SAME §13 narrative (steps 1-6), driven over `tcode serve --http` +
/// `GET /sessions/{id}/events` SSE instead of STDIO.
///
/// Why: §9 Requirement 2 mandates BOTH transports be exercised; applying
/// that at the milestone level (not just per-issue) proves the cut line
/// itself — not merely one transport's framing of it — is transport-agnostic.
/// What: `POST /rpc` for `session.create`/`task.run`/`session.status`/
/// `session.get_transcript`; a single SSE connection read in stages
/// (replay, then live events) for step 4, reusing
/// `assert_envelopes_contiguous` for the #2055 seq-contiguity guarantee.
/// Re-asserts the SAME [`BASELINE_EVENT_KINDS`]/[`BASELINE_TRANSCRIPT_ROLES`]
/// baseline so the two transports are proven to agree on it.
/// Test: this test.
#[tokio::test]
async fn m1_cutline_full_scenario_over_http() {
    let project = project_with_agents();
    let daemon =
        support::spawn_http_daemon_with_env(project.path(), &[("TCODE_MOCK_LLM", "echo")]).await;
    let client = reqwest::Client::new();
    let rpc_url = format!("{}/rpc", daemon.base_url);

    let post = |id: i64, method: &'static str, params: serde_json::Value| {
        let client = client.clone();
        let rpc_url = rpc_url.clone();
        async move {
            client
                .post(&rpc_url)
                .json(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
                .send()
                .await
                .expect("POST /rpc")
                .json::<serde_json::Value>()
                .await
                .expect("parse JSON response")
        }
    };

    let create_resp = post(
        1,
        "session.create",
        json!({"task": "M1 cut-line http task"}),
    )
    .await;
    assert!(
        create_resp["error"].is_null(),
        "session.create failed: {create_resp}"
    );
    let session_id = create_resp["result"]["id"].as_str().unwrap().to_string();

    let events_url = format!("{}/sessions/{session_id}/events", daemon.base_url);
    let mut sse = open_sse(&client, &events_url).await;
    let mut buffer = Vec::new();
    read_sse_until(
        &mut sse,
        &mut buffer,
        "session_status_changed",
        Duration::from_secs(5),
    )
    .await;

    let run_resp = post(
        2,
        "task.run",
        json!({"task_description": "say hi", "session_id": session_id}),
    )
    .await;
    assert!(run_resp["error"].is_null(), "task.run failed: {run_resp}");
    assert_eq!(run_resp["result"]["mode"], "daily-driver");

    read_sse_until(
        &mut sse,
        &mut buffer,
        "session_done",
        Duration::from_secs(10),
    )
    .await;
    let frames = parse_sse_frames(&String::from_utf8_lossy(&buffer));
    let (first_seq, last_seq) = assert_envelopes_contiguous(&frames);
    assert_eq!(first_seq, 1);
    // `seq` is assigned to EVERY recorded event, including any async
    // `index_readiness` or `log` that happened to land inside this window — so
    // pin contiguity against what was actually streamed rather than against the
    // deterministic baseline's length (see `filter_async_kinds`).
    assert_eq!(
        last_seq as usize,
        frames.len(),
        "per-session seq must run contiguously 1..=N over every streamed envelope"
    );
    let kinds = filter_async_kinds(frames.iter().map(|f| f["kind"].as_str().unwrap()));
    assert_eq!(
        kinds, BASELINE_EVENT_KINDS,
        "HTTP transport's event-kind sequence must match the SAME baseline as STDIO"
    );
    // The `log` events the baseline filters out still owe their content (#5895).
    assert_memory_degradation_logs_well_formed(&frames);
    drop(sse);

    let status_resp = post(3, "session.status", json!({"session_id": session_id})).await;
    assert_eq!(status_resp["result"]["status"], "finished");

    let transcript_resp = post(
        4,
        "session.get_transcript",
        json!({"session_id": session_id}),
    )
    .await;
    let turns = transcript_resp["result"]["turns"].as_array().unwrap();
    let roles: Vec<&str> = turns.iter().map(|t| t["role"].as_str().unwrap()).collect();
    assert_eq!(
        roles, BASELINE_TRANSCRIPT_ROLES,
        "HTTP transport's transcript roles must match the SAME baseline as STDIO"
    );

    daemon.shutdown_via_sigterm_and_assert_clean_exit().await;
}

/// §13's cancellation clause: `session.cancel` on an in-flight (or
/// already-completed — see below) `task.run` execution.
///
/// Why: cooperative cancellation (#2056) is checked once per turn boundary,
/// so whether a fast, no-network mock run is caught BEFORE its first turn
/// or already past it by the time `session.cancel` is processed is
/// inherently a race with no artificial delay lever to control it (see
/// `crate::task::mock_llm`'s docs — the echo script has no sleep). This
/// test calls `session.cancel` IMMEDIATELY after `task.run` returns (no
/// intervening read), which — because `task.run` reserves the execution
/// slot and returns BEFORE its `tokio::spawn`'d background task gets its
/// first poll — reliably wins the race in practice (confirmed flake-free
/// across repeated runs; see this ticket's report). The assertion accepts
/// EITHER legitimate outcome so the test can never flake: if cancellation
/// won the race, a second `session_status_changed` (running -> cancelled)
/// plus `session_done` must appear (cooperative completion lands via the
/// SAME generic `SessionRegistry::finish` path `Finished`/`Failed` use —
/// the distinct `session_cancelled` EVENT kind is exclusive to
/// `SessionRegistry::cancel`'s administrative, idle-session immediate-cancel
/// path, not this cooperative one); if the run's own completion won
/// instead, `status: "finished"` is equally correct (cancelling an
/// already-finished session is the pre-existing #2054 idempotent no-op).
/// What: `task.run` -> immediate `session.cancel` -> attach/read until a
/// terminal event -> assert the terminal status is one of the two
/// legitimate outcomes, with kind-sequence assertions specific to whichever
/// branch actually occurred.
/// Test: this test.
#[tokio::test]
async fn m1_cutline_cancel_path() {
    let project = project_with_agents();
    let mut daemon = StdioSession::spawn_with_mock_llm(project.path());

    let run_resp = daemon
        .call(1, "task.run", json!({"task_description": "say hi"}))
        .await;
    assert!(run_resp["error"].is_null(), "task.run failed: {run_resp}");
    let session_id = run_resp["result"]["session_id"]
        .as_str()
        .expect("task.run must return a session_id")
        .to_string();

    // Immediately request cancellation — no intervening read — to race the
    // background execution's very first turn-boundary check.
    let cancel_resp = daemon
        .call(2, "session.cancel", json!({"session_id": session_id}))
        .await;
    assert!(
        cancel_resp["error"].is_null(),
        "session.cancel failed: {cancel_resp}"
    );

    let attach_resp = daemon
        .call(3, "session.attach", json!({"session_id": session_id}))
        .await;
    assert!(
        attach_resp["error"].is_null(),
        "session.attach failed: {attach_resp}"
    );
    let mut kinds: Vec<String> = attach_resp["result"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap().to_string())
        .collect();

    // `session_done` is the ONE kind every terminal path (finished, failed,
    // OR cooperatively cancelled) publishes last — see this test's own doc
    // comment for why `session_cancelled` specifically is NOT part of the
    // cooperative-cancellation completion path.
    let mut iterations = 0;
    while !kinds.iter().any(|k| k == "session_done") {
        iterations += 1;
        assert!(
            iterations < 30,
            "gave up waiting for session_done after {iterations} read rounds; kinds so far: {kinds:?}"
        );
        let lines = daemon.read_lines(20).await;
        assert!(
            !lines.is_empty(),
            "timed out waiting for more events; kinds so far: {kinds:?}"
        );
        for line in &lines {
            if let Some(envelope) = find_session_event(line, &session_id) {
                kinds.push(envelope["kind"].as_str().unwrap().to_string());
            }
        }
    }

    let status_resp = daemon
        .call(4, "session.status", json!({"session_id": session_id}))
        .await;
    let final_status = status_resp["result"]["status"].as_str().unwrap();
    assert!(
        final_status == "cancelled" || final_status == "finished",
        "session.cancel must leave the session in a legitimate terminal state, got {final_status}: kinds={kinds:?}"
    );
    if final_status == "cancelled" {
        // Cooperative cancellation's completion path lands via
        // `SessionRegistry::finish` (the SAME generic terminal transition
        // `Finished`/`Failed` use), publishing `session_status_changed` +
        // `session_done` — NOT the distinct `session_cancelled` kind, which
        // is exclusive to `SessionRegistry::cancel`'s ADMINISTRATIVE
        // immediate-cancel path (an idle, non-executing session; see
        // `session::protocol::cancel`'s docs). Both are legitimate
        // "cancelled" observability shapes; this test exercises the
        // cooperative one.
        assert!(
            kinds
                .iter()
                .filter(|k| *k == "session_status_changed")
                .count()
                >= 2,
            "a cooperatively-cancelled run must publish a second status transition \
             (running -> cancelled): {kinds:?}"
        );
    }
    assert!(
        kinds.contains(&"session_done".to_string()),
        "every terminal session must publish session_done: {kinds:?}"
    );

    daemon.shutdown_via_eof_and_assert_clean_exit().await;
}

/// §13's closing clause: "replicate the same task through the thin CLI
/// (`tcode run-task`)."
///
/// Why: the cut line is only real if the SAME capability is reachable
/// through the CLI, not merely the raw JSON-RPC surface (#2060, spec §9
/// Requirement 1 "100% CLI/API-Testable"). This spawns the REAL `tcode`
/// binary as a black-box subprocess (not the daemon directly, and not via
/// `StdioSession` — that would be testing the API again, not the CLI).
/// What: `tcode run-task pm "..." --project <tmp>` with
/// `TCODE_MOCK_LLM=echo`; asserts the CLI's own stdout streamed the
/// tool-lifecycle narration and reported a `finished` status, and that the
/// process exited 0 — the observable, user-facing form of the SAME
/// baseline the API-level tests above pin structurally.
/// Test: this test.
#[test]
fn m1_cutline_replay_via_thin_cli() {
    let project = project_with_agents();
    let output = support::tcode_command()
        .args([
            "run-task",
            "pm",
            "say hi",
            "--project",
            &project.path().display().to_string(),
        ])
        .env("TCODE_MOCK_LLM", "echo")
        .output()
        .expect("spawn tcode run-task");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "tcode run-task must exit 0, got {:?}\nstdout: {stdout}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("tool_started") && stdout.contains("delegate_to_agent"),
        "CLI stdout must narrate the PM's delegate_to_agent dispatch: {stdout}"
    );
    assert!(
        stdout.contains("tool_started") && stdout.contains("bash"),
        "CLI stdout must narrate the engineer's bash dispatch: {stdout}"
    );
    assert!(
        stdout.contains("status=finished"),
        "CLI stdout must report the terminal finished status: {stdout}"
    );
}
