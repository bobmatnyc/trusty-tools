//! End-to-end acceptance proof for DOC-39 Slice B (per-hit path+score on
//! `Event::SearchPerformed`) — the mandatory API-driven e2e gate for this
//! slice.
//!
//! Why: the Search audit trail / "what drove this" UI needs to know WHICH
//! files a `search_code` call touched, not just how many. Unit tests already
//! pin the parsing (`tools::trusty_search::tests::parse_search_hits_*`) and
//! the in-process threading
//! (`session::registry::registry_tests::record_search_performed_publishes_hits_with_path_and_score`),
//! but Bob's standing testability directive requires the capability also be
//! proven reachable over the REAL wire, against a real (subprocess) daemon —
//! this is that proof.
//! What: spawns the real `tcode serve --stdio` binary with a FAKE
//! `trusty-search` MCP server substituted onto its `PATH` (a POSIX shell
//! script that speaks just enough MCP to answer `initialize`, `list_indexes`,
//! and `search_semantic` with a canned, KNOWN result set — see
//! [`write_fake_trusty_search`]), drives `TCODE_MOCK_LLM=echo-search`'s
//! scripted PM -> engineer `search_code` call
//! ([`trusty_code::task::mock_llm::SearchEchoLlmClient`]) to completion, and
//! asserts the resulting `SearchPerformed` event's `hits` array matches that
//! canned result set by BOTH `path` AND `score` — not merely `hit_count`.
//! Test: this module is itself the test surface.

mod support;

use serde_json::{Value, json};
use support::{StdioSession, find_session_event, project_with_agents};

/// The canned search hits the fake trusty-search server's `search_semantic`
/// reply carries — the known result set every assertion in this file checks
/// `hits` against.
const FAKE_HITS: &[(&str, f64)] = &[("src/auth.rs", 0.87), ("src/session/session.rs", 0.52)];

/// Write a fake `trusty-search` MCP server, executable at
/// `<dir>/trusty-search`, answering `initialize`, `list_indexes` (with one
/// index whose `root_path` is `project_root`), and `search_semantic` (with
/// [`FAKE_HITS`]) in that fixed order.
///
/// Why: `tools::trusty_search::TrustySearchTool` always spawns the literal
/// binary name `trusty-search` resolved via `PATH` — its `with_binary`
/// override is `#[cfg(test)]`-only and unreachable from this crate-external
/// integration test — so the only seam to substitute a fake server is a
/// same-named executable placed earlier on the spawned daemon's `PATH` (see
/// `support::StdioSession::spawn_with_mock_llm_variant_and_envs`).
/// What: mirrors `tools::trusty_search::tests::write_fake_mcp_server`'s
/// shape — a POSIX shell script that replies to the Nth request line
/// carrying an `"id":` field with the Nth canned response, verbatim, so the
/// handshake's `notifications/initialized` notification (no `id`) is
/// silently skipped. `root_path` is the ALREADY-canonicalized project root
/// (matching `ProjectBinding::resolve`'s own canonicalization) so
/// `tools::trusty_search::pick_index`'s exact-match branch resolves it
/// without relying on a second independent canonicalization agreeing
/// byte-for-byte.
/// Test: exercised by this module's own e2e test.
#[cfg(unix)]
fn write_fake_trusty_search(dir: &std::path::Path, project_root: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let canonical_root = std::fs::canonicalize(project_root)
        .expect("canonicalize project root")
        .to_str()
        .expect("utf8 project root")
        .to_string();

    let init_resp = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "serverInfo": { "name": "fake-trusty-search", "version": "0.0.0" },
            "protocolVersion": "2024-11-05"
        }
    });
    let list_indexes_body = json!({
        "indexes": [{ "id": "test-idx", "root_path": canonical_root, "size_bytes": 0 }]
    })
    .to_string();
    let list_indexes_resp = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "result": { "content": [{ "type": "text", "text": list_indexes_body }] }
    });
    let search_body = json!({
        "results": FAKE_HITS
            .iter()
            .map(|(path, score)| json!({ "path": path, "score": score }))
            .collect::<Vec<_>>()
    })
    .to_string();
    let search_resp = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "result": { "content": [{ "type": "text", "text": search_body }] }
    });

    let script = format!(
        "#!/bin/sh\ni=0\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"id\":'*)\n      i=$((i + 1))\n      case \"$i\" in\n        1) printf '%s\\n' '{}' ;;\n        2) printf '%s\\n' '{}' ;;\n        3) printf '%s\\n' '{}' ;;\n      esac\n      ;;\n  esac\ndone\n",
        init_resp, list_indexes_resp, search_resp,
    );

    let path = dir.join("trusty-search");
    std::fs::write(&path, script).expect("write fake trusty-search script");
    let mut perms = std::fs::metadata(&path)
        .expect("stat fake trusty-search script")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod fake trusty-search script");
}

/// Drive `task.run` -> `session.attach` -> read-until-`session_done` using
/// the `echo-search` mock LLM script, returning every session event's full
/// JSON envelope (not just its `kind`, unlike `support::run_task_to_completion`
/// — this test needs each envelope's `event.hits` field). Mirrors
/// `agent_id_e2e::run_fanout_task_to_completion`.
async fn run_search_task_to_completion(daemon: &mut StdioSession) -> Vec<Value> {
    let run_resp = daemon
        .call(
            1,
            "task.run",
            json!({"task_description": "find where auth lives"}),
        )
        .await;
    assert!(run_resp["error"].is_null(), "task.run failed: {run_resp}");
    let session_id = run_resp["result"]["session_id"]
        .as_str()
        .expect("task.run must return a session_id")
        .to_string();

    let attach_resp = daemon
        .call(2, "session.attach", json!({"session_id": session_id}))
        .await;
    assert!(
        attach_resp["error"].is_null(),
        "attach failed: {attach_resp}"
    );
    let mut envelopes: Vec<Value> = attach_resp["result"]["events"]
        .as_array()
        .expect("attach must return a replay events array")
        .clone();

    let mut iterations = 0;
    let is_done = |envs: &[Value]| {
        envs.iter()
            .any(|e| e["kind"].as_str() == Some("session_done"))
    };
    while !is_done(&envelopes) {
        iterations += 1;
        assert!(
            iterations < 20,
            "gave up waiting for session_done after {iterations} read rounds; \
             envelopes so far: {envelopes:?}"
        );
        let lines = daemon.read_lines(20).await;
        assert!(
            !lines.is_empty(),
            "timed out waiting for more events; envelopes so far: {envelopes:?}"
        );
        for line in &lines {
            if let Some(envelope) = find_session_event(line, &session_id) {
                envelopes.push(envelope);
            }
        }
    }

    envelopes
}

/// DOC-39 Slice B's direct acceptance proof: `search_code`'s
/// `Event::SearchPerformed.hits` must carry each hit's REAL `path` AND
/// `score` from the underlying trusty-search response — not merely a
/// `hit_count` — over the REAL JSON-RPC wire against a (mock) trusty-search
/// MCP backend.
///
/// Why: this is the exact requirement the ticket's testability directive
/// states — "a test that only checks `hit_count` does NOT satisfy this
/// task". Uses `TCODE_MOCK_LLM=echo-search`
/// (`trusty_code::task::mock_llm::SearchEchoLlmClient`) for a deterministic,
/// key-free run scripting the PM delegating to `python-engineer`, which
/// calls `search_code(query="where does auth live", mode="semantic")`
/// against the fake trusty-search server (see [`write_fake_trusty_search`])
/// substituted onto the daemon's `PATH`.
/// What: spawns the daemon rooted at a throwaway project with the fake
/// server on `PATH`, runs the scripted task to completion, finds the
/// `search_performed` event, and asserts its `hits` array equals
/// [`FAKE_HITS`] exactly (both `path` and `score` per entry, in order) —
/// the assertion this whole file exists to make.
/// Test: this test.
#[tokio::test]
#[cfg(unix)]
async fn search_code_hit_paths_and_scores_reach_the_search_performed_event() {
    let project = project_with_agents();
    let fake_search_dir = tempfile::tempdir().expect("fake trusty-search dir");
    write_fake_trusty_search(fake_search_dir.path(), project.path());

    let fake_search_dir_str = fake_search_dir
        .path()
        .to_str()
        .expect("utf8 fake trusty-search dir");
    let real_path = std::env::var("PATH").unwrap_or_default();
    let patched_path = format!("{fake_search_dir_str}:{real_path}");

    let mut daemon = StdioSession::spawn_with_mock_llm_variant_and_envs(
        project.path(),
        trusty_code::task::mock_llm::MOCK_LLM_ECHO_SEARCH,
        &[("PATH", &patched_path)],
    );

    let envelopes = run_search_task_to_completion(&mut daemon).await;

    let search_events: Vec<&Value> = envelopes
        .iter()
        .filter(|e| e["kind"].as_str() == Some("search_performed"))
        .collect();

    assert_eq!(
        search_events.len(),
        1,
        "expected exactly one search_performed event; got: {search_events:?}\n\
         all envelopes: {envelopes:?}"
    );

    let event = &search_events[0]["event"];
    assert_eq!(event["agent"].as_str(), Some("python-engineer"));
    assert_eq!(
        event["hit_count"].as_u64(),
        Some(FAKE_HITS.len() as u64),
        "hit_count must match the real result set's size: {event:?}"
    );

    let hits = event["hits"].as_array().expect("hits must be an array");
    assert_eq!(
        hits.len(),
        FAKE_HITS.len(),
        "hits length must match hit_count: {hits:?}"
    );
    for (got, (expected_path, expected_score)) in hits.iter().zip(FAKE_HITS.iter()) {
        assert_eq!(
            got["path"].as_str(),
            Some(*expected_path),
            "DOC-39 Slice B: hit path must match the real trusty-search result set, \
             not just hit_count: {got:?}"
        );
        assert_eq!(
            got["score"].as_f64(),
            Some(*expected_score),
            "DOC-39 Slice B: hit score must match the real trusty-search result set, \
             not just hit_count: {got:?}"
        );
    }
}
