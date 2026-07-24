//! Unit tests for the `trim_to_budget` strategies and helpers (moved out of
//! `context/manager.rs` under the 500-SLOC file cap, issue #610). Basename
//! `tests.rs` so the line-cap gate applies the test-file cap.

use super::*;
use serde_json::json;

#[test]
fn trim_noop_when_under_budget() {
    let mgr = ContextManager::new(0.5);
    let msgs = vec![json!({"role":"system","content":"hi"})];
    let (out, outcome) = mgr.trim_to_budget(msgs.clone(), "claude-sonnet-4-6", 1);
    assert_eq!(outcome, TrimOutcome::default());
    assert!(!outcome.changed());
    assert_eq!(out.len(), 1);
}

#[test]
fn trim_drops_oldest_evictable() {
    // Eviction fallback: MANY moderate messages whose fair per-message cap
    // falls below MIN_TRUNCATED_TOKENS, so truncation can't help and the
    // original #69 oldest-first eviction fires. Budget is ~12.8k tokens
    // (10% of gpt-4's 128k); 120 messages of ~500 tokens each (~60k total)
    // give a fair share of ~106 tokens/msg < the 256-token floor.
    let mid = "a".repeat(2_000); // ~500 tokens
    let mgr = ContextManager::new(0.1);
    let mut msgs = vec![json!({"role":"system","content":"sys"})];
    for i in 0..120 {
        let role = if i % 2 == 0 { "user" } else { "assistant" };
        msgs.push(json!({"role": role, "content": mid.clone()}));
    }
    let (out, outcome) = mgr.trim_to_budget(msgs, "gpt-4", 1);
    assert!(outcome.evicted >= 1, "expected at least one eviction");
    assert_eq!(outcome.truncated, 0, "cap below floor: evict, not truncate");
    // Protected system message must survive at the front.
    assert_eq!(out[0]["role"], "system");
    assert!(out.len() < 121, "history must shrink");
}

/// Assert every `tool` message is paired with an assistant `tool_calls`
/// entry that declares its id — i.e. no orphaned `tool_call_id` that a
/// provider would reject.
fn assert_no_orphans(msgs: &[serde_json::Value]) {
    use std::collections::HashSet;
    let declared: HashSet<&str> = msgs
        .iter()
        .filter_map(|m| m.get("tool_calls").and_then(|v| v.as_array()))
        .flatten()
        .filter_map(|c| c.get("id").and_then(|v| v.as_str()))
        .collect();
    for m in msgs {
        if m.get("role").and_then(|r| r.as_str()) == Some("tool") {
            let id = m.get("tool_call_id").and_then(|v| v.as_str()).unwrap();
            assert!(
                declared.contains(id),
                "orphaned tool result {id}: no surviving assistant tool_calls declares it"
            );
        }
    }
}

/// Why: THE code-critic HIGH. A tools-only continuation (NO user message)
/// with a multi-tool-call turn: `assistant(tool_calls=[c1,c2,c3])` +
/// `tool(c1)` + `tool(c2)`(huge) + `tool(c3)`(huge). The `RECENCY_FALLBACK`
/// window would split the group (2 old / 2 recent); evicting the OLD region
/// then drops the assistant while `tool(c2)`/`tool(c3)` survive — orphaned
/// `tool_call_id`s the provider rejects. The pairing-atomic boundary must
/// pull the whole group into the recency window so it truncates rather than
/// evicts.
/// What: Assert no message is evicted, the huge results are truncated, and
/// no tool result is orphaned.
/// Test: This test.
#[test]
fn trim_keeps_tool_call_pairing_atomic_without_user_message() {
    let mgr = ContextManager::new(0.5); // sonnet: budget = 100k tokens
    let huge = "grep hit path/to/file.rs:42\n".repeat(300_000); // ~8 MB each
    let msgs = vec![
        json!({"role":"system","content":"You are izzie."}),
        json!({"role":"assistant","content":null,"tool_calls":[
            {"id":"c1","type":"function","function":{"name":"grep","arguments":"{}"}},
            {"id":"c2","type":"function","function":{"name":"grep","arguments":"{}"}},
            {"id":"c3","type":"function","function":{"name":"grep","arguments":"{}"}}
        ]}),
        json!({"role":"tool","tool_call_id":"c1","content":"small first result"}),
        json!({"role":"tool","tool_call_id":"c2","content": huge.clone()}),
        json!({"role":"tool","tool_call_id":"c3","content": huge}),
    ];
    let (out, outcome) = mgr.trim_to_budget(msgs, "anthropic/claude-sonnet-4-6", 1);

    assert_eq!(
        outcome.evicted, 0,
        "must not evict — would orphan tool results"
    );
    assert!(outcome.truncated >= 2, "the two huge results must truncate");
    assert_no_orphans(&out);
    // The declaring assistant and all three results survive.
    assert!(
        out.iter().any(|m| m.get("tool_calls").is_some()),
        "assistant tool_calls group must survive"
    );
    for id in ["c1", "c2", "c3"] {
        assert!(
            out.iter().any(|m| m["tool_call_id"] == id),
            "result {id} must survive"
        );
    }
    let total: u32 = out.iter().map(estimate_tokens).sum();
    let budget = (context_window("anthropic/claude-sonnet-4-6") as f32 * 0.5) as u32;
    assert!(
        total <= budget,
        "trimmed total {total} must fit budget {budget}"
    );
}

/// Why: Strategy 2 eviction must itself be pairing-atomic. An OLD assistant
/// tool-call with a huge NON-STRING content-block array is large (so
/// evicting it alone fits the budget) yet non-truncatable (truncation only
/// shortens string content); evicting it without its (small) result would
/// orphan that result. The atomic-group eviction must take the result with
/// it — otherwise a naive oldest-first loop stops after the big assistant
/// and strands `tool(old1)`.
/// What: system + huge-content-array assistant(old1) + tiny tool(old1) [OLD]
/// + user + assistant(c1) + tiny tool(c1) [live]. Assert the whole old group
/// is evicted together, the live turn survives, and nothing is orphaned.
/// Test: This test.
#[test]
fn trim_evicts_tool_call_group_atomically_no_orphan() {
    let mgr = ContextManager::new(0.5); // sonnet: budget = 100k tokens
    // ~150k-token, non-truncatable (array content, not a plain string).
    let huge_text = "a".repeat(600_000);
    let msgs = vec![
        json!({"role":"system","content":"sys"}),
        json!({"role":"assistant","content":[{"type":"text","text": huge_text}],"tool_calls":[
            {"id":"old1","type":"function","function":{"name":"grep","arguments":"{}"}}
        ]}),
        json!({"role":"tool","tool_call_id":"old1","content":"tiny old result"}),
        json!({"role":"user","content":"the live question"}),
        json!({"role":"assistant","content":null,"tool_calls":[
            {"id":"c1","type":"function","function":{"name":"grep","arguments":"{}"}}
        ]}),
        json!({"role":"tool","tool_call_id":"c1","content":"tiny live result"}),
    ];
    let (out, outcome) = mgr.trim_to_budget(msgs, "anthropic/claude-sonnet-4-6", 1);

    assert!(outcome.evicted >= 2, "the whole old group must be evicted");
    assert_no_orphans(&out);
    // Old group (assistant + result) both gone — no half-evicted pairing.
    assert!(
        !out.iter().any(|m| m["tool_call_id"] == "old1"),
        "old result must be evicted with its assistant"
    );
    assert!(
        !out.iter().any(|m| m
            .get("tool_calls")
            .and_then(|v| v.as_array())
            .is_some_and(|a| a.iter().any(|c| c["id"] == "old1"))),
        "old assistant tool-call must be evicted"
    );
    // The live turn survives intact.
    assert!(out.iter().any(|m| m["content"] == "the live question"));
    assert_eq!(out[out.len() - 1]["tool_call_id"], "c1");
}

/// Why: code-critic round-4 HIGH. Strategy 2's atomic sweep must be
/// adjacency-independent. Adversarial/replayed history can interleave groups
/// inside OLD — `assistant(a1)`, `assistant(a2)`, `tool(a1)`, `tool(a2)` —
/// where the results are NOT immediately consecutive after their declarer.
/// A consecutive-only sweep evicted `assistant(a1)` and left `tool(a1)`
/// orphaned (no declarer). Both groups sit in OLD, so the recency boundary
/// clamp never engages; the whole-Vec sweep must catch them.
/// What: Non-truncatable array-content declarers force Strategy 2 eviction;
/// assert both interleaved groups leave whole, the live turn survives, and no
/// tool result is orphaned.
/// Test: This test.
#[test]
fn trim_evicts_interleaved_tool_call_groups_atomically() {
    let mgr = ContextManager::new(0.5); // sonnet: budget = 100k tokens
    let huge = "a".repeat(600_000); // non-truncatable array content
    let big_array = || json!([{"type":"text","text": huge.clone()}]);
    let msgs = vec![
        json!({"role":"system","content":"sys"}),
        // Two declarers first, THEN their results — interleaved, both in OLD.
        json!({"role":"assistant","content": big_array(),"tool_calls":[
            {"id":"a1","type":"function","function":{"name":"grep","arguments":"{}"}}
        ]}),
        json!({"role":"assistant","content": big_array(),"tool_calls":[
            {"id":"a2","type":"function","function":{"name":"grep","arguments":"{}"}}
        ]}),
        json!({"role":"tool","tool_call_id":"a1","content":"tiny a1"}),
        json!({"role":"tool","tool_call_id":"a2","content":"tiny a2"}),
        // Live turn.
        json!({"role":"user","content":"the live question"}),
        json!({"role":"assistant","content":null,"tool_calls":[
            {"id":"c1","type":"function","function":{"name":"grep","arguments":"{}"}}
        ]}),
        json!({"role":"tool","tool_call_id":"c1","content":"tiny live"}),
    ];
    let (out, outcome) = mgr.trim_to_budget(msgs, "anthropic/claude-sonnet-4-6", 1);

    assert!(outcome.evicted >= 2, "interleaved groups must be evicted");
    assert_no_orphans(&out);
    // Each declarer is non-truncatable and evicted, so its result goes too.
    assert!(!out.iter().any(|m| m["tool_call_id"] == "a1"));
    assert!(!out.iter().any(|m| m["tool_call_id"] == "a2"));
    // The live turn survives intact.
    assert!(out.iter().any(|m| m["content"] == "the live question"));
    assert_eq!(out[out.len() - 1]["tool_call_id"], "c1");
}

/// Why: The real demo path — the model calls an MCP tool (`grep`) SEVERAL
/// times, accumulating MULTIPLE megabyte results. The single-dominant-message
/// truncation didn't cover this and eviction still collapsed the turn to the
/// system prompt (observed live as `evicted=7 before=8 after=1`). Water-fill
/// truncation must shrink ALL oversized results and evict nothing.
/// What: system + three interleaved (user? no — assistant tool-call + huge
/// tool result) blocks, each result multi-MB; assert all survive, all three
/// results truncated, none evicted, and the set fits budget.
/// Test: This test.
#[test]
fn trim_truncates_multiple_oversized_messages() {
    let mgr = ContextManager::new(0.5); // sonnet: budget = 100k tokens
    let huge = "grep match with file path and code line\n".repeat(160_000); // ~6 MB
    let mut msgs = vec![
        json!({"role":"system","content":"You are izzie."}),
        json!({"role":"user","content":"Find estimate_tokens, water_fill_cap, and trim_to_budget."}),
    ];
    for id in ["call_1", "call_2", "call_3"] {
        msgs.push(json!({
            "role":"assistant","content":null,
            "tool_calls":[{"id":id,"type":"function",
                "function":{"name":"grep","arguments":"{\"pattern\":\"x\"}"}}]
        }));
        msgs.push(json!({"role":"tool","tool_call_id":id,"content":huge.clone()}));
    }
    let original_len = msgs.len();
    let (out, outcome) = mgr.trim_to_budget(msgs, "anthropic/claude-sonnet-4-6", 1);

    assert_eq!(outcome.evicted, 0, "must not evict any turn");
    assert_eq!(outcome.truncated, 3, "all three huge results truncated");
    assert_eq!(out.len(), original_len, "every turn must survive");
    // The question and all three tool pairings are intact.
    assert_eq!(
        out[1]["content"],
        "Find estimate_tokens, water_fill_cap, and trim_to_budget."
    );
    for id in ["call_1", "call_2", "call_3"] {
        assert!(
            out.iter().any(|m| m["tool_call_id"] == id),
            "tool result for {id} must survive"
        );
    }
    // Each truncated result still carries quotable signal.
    let tool_msgs: Vec<&str> = out
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| m["content"].as_str().unwrap())
        .collect();
    assert_eq!(tool_msgs.len(), 3);
    for t in &tool_msgs {
        assert!(t.contains("grep match"), "must retain leading matches");
        assert!(t.contains("truncated"), "elision marker present");
    }
    let total: u32 = out.iter().map(estimate_tokens).sum();
    let budget = (context_window("anthropic/claude-sonnet-4-6") as f32 * 0.5) as u32;
    assert!(
        total <= budget,
        "trimmed total {total} must fit budget {budget}"
    );
}

#[test]
fn trim_respects_protected_count_greater_than_len() {
    let mgr = ContextManager::new(0.1);
    let msgs = vec![json!({"role":"system","content":"s"})];
    // protected_count=5 > len=1 should not panic.
    let (out, outcome) = mgr.trim_to_budget(msgs, "gpt-4", 5);
    assert!(!outcome.changed());
    assert_eq!(out.len(), 1);
}

/// Why: THE regression for the demo-blocking bug. An MCP tool result (e.g.
/// `grep` via trusty-search) is not shrunk by any `compress_tool_output`
/// filter and can be multiple megabytes. Under the old pure oldest-first
/// eviction, this single huge tool message forced eviction of the user's
/// question AND the assistant tool-call turn AND itself, collapsing a
/// 4-message conversation to just the protected system message — so the
/// follow-up completion answered with zero context.
/// What: Build the exact live shape — system, user question, assistant
/// tool-call, oversized `tool` result — and assert the trimmer TRUNCATES
/// the result in place (does NOT evict) so every turn (question +
/// tool-call/tool pairing) survives.
/// Test: This test.
#[test]
fn trim_truncates_single_oversized_message() {
    let mgr = ContextManager::new(0.5); // sonnet: budget = 100k tokens
    // ~7 MB tool result — vastly over the 100k-token (~400 KB) budget.
    let huge = "grep match line with some file path and code\n".repeat(160_000);
    let original_len = huge.len();
    let msgs = vec![
        json!({"role":"system","content":"You are izzie, a helpful assistant."}),
        json!({"role":"user","content":"Where is estimate_tokens defined?"}),
        json!({
            "role":"assistant",
            "content": null,
            "tool_calls":[{
                "id":"call_1",
                "type":"function",
                "function":{"name":"grep","arguments":"{\"pattern\":\"estimate_tokens\"}"}
            }]
        }),
        json!({"role":"tool","tool_call_id":"call_1","content": huge}),
    ];
    let (out, outcome) = mgr.trim_to_budget(msgs, "anthropic/claude-sonnet-4-6", 1);

    // The live conversation is NOT evicted: all four turns survive.
    assert_eq!(outcome.evicted, 0, "must not evict any turn");
    assert_eq!(outcome.truncated, 1, "must truncate the oversized result");
    assert_eq!(out.len(), 4, "every turn (incl. the question) must survive");

    // Roles/pairing preserved so the follow-up request is well-formed.
    assert_eq!(out[0]["role"], "system");
    assert_eq!(out[1]["role"], "user");
    assert_eq!(out[1]["content"], "Where is estimate_tokens defined?");
    assert_eq!(out[2]["role"], "assistant");
    assert_eq!(out[3]["role"], "tool");
    assert_eq!(out[3]["tool_call_id"], "call_1");

    // The tool result was shortened but still carries usable signal.
    let kept = out[3]["content"].as_str().unwrap();
    assert!(kept.len() < original_len, "result must be shortened");
    assert!(
        kept.contains("grep match line"),
        "truncated result must retain leading matches to quote"
    );
    assert!(kept.contains("truncated"), "elision marker must be present");

    // And the whole set now fits the budget.
    let total: u32 = out.iter().map(estimate_tokens).sum();
    let budget = (context_window("anthropic/claude-sonnet-4-6") as f32 * 0.5) as u32;
    assert!(
        total <= budget,
        "trimmed total {total} must fit budget {budget}"
    );
}

/// Why: Recency guarantee (code-critic MEDIUM). In a uniformly-sized long
/// conversation — no megabyte outlier — the NEWEST turn must keep full
/// fidelity, exactly as the old oldest-first eviction implicitly guaranteed.
/// Naive water-fill shrank the newest message too; the recency window fixes
/// that. This is the critic's empirical repro: 1 system + 10 × 12 KB
/// messages under gpt-4's ~12.8k-token (10%) budget.
/// What: The last user message and everything after it (the live turn) are
/// returned byte-for-byte unchanged; older messages are truncated or evicted.
/// Test: This test.
#[test]
fn trim_preserves_newest_turn_in_uniform_history() {
    let body = "x".repeat(12_000); // ~3k tokens each; 10 of them ≫ 12.8k budget
    let mgr = ContextManager::new(0.1); // gpt-4: budget = 12_800 tokens
    let mut msgs = vec![json!({"role":"system","content":"sys"})];
    for i in 0..10 {
        let role = if i % 2 == 0 { "user" } else { "assistant" };
        msgs.push(json!({"role": role, "content": body.clone()}));
    }
    // Newest turn = last user message (index 9) + trailing assistant (10).
    let newest_user_before = msgs[9]["content"].as_str().unwrap().to_string();
    let newest_asst_before = msgs[10]["content"].as_str().unwrap().to_string();

    let (out, outcome) = mgr.trim_to_budget(msgs, "gpt-4", 1);

    assert!(outcome.changed(), "must trim something");
    // The newest turn survived byte-for-byte — NOT shrunk.
    assert_eq!(out[9]["content"].as_str().unwrap(), newest_user_before);
    assert_eq!(out[10]["content"].as_str().unwrap(), newest_asst_before);
    assert_eq!(
        newest_asst_before.len(),
        12_000,
        "newest message kept at full 12k-byte fidelity"
    );
    // Older messages were shrunk (truncated and/or evicted).
    assert!(
        out.len() < 11 || outcome.truncated > 0,
        "old history must shrink"
    );
    // The system header is intact and first.
    assert_eq!(out[0]["role"], "system");
    // And the whole set now fits the budget.
    let total: u32 = out.iter().map(estimate_tokens).sum();
    assert!(total <= 12_800, "trimmed total {total} must fit budget");
}

/// Why: The recency window must NOT save the live turn at the cost of
/// re-breaking #3776 — when the oversized message IS in the live turn (the
/// MCP tool result immediately after the question), Strategy 3 must truncate
/// it in place, and the question must survive.
/// What: system + user question + assistant tool-call + huge tool result
/// (all within the live turn); assert the result is truncated, nothing
/// evicted, and the question is byte-for-byte intact.
/// Test: This test.
#[test]
fn trim_truncates_oversized_result_inside_live_turn() {
    let mgr = ContextManager::new(0.5);
    let huge = "grep hit path/to/file.rs:42 code\n".repeat(300_000); // ~9 MB
    let msgs = vec![
        json!({"role":"system","content":"You are izzie."}),
        json!({"role":"user","content":"Where is trim_to_budget defined?"}),
        json!({
            "role":"assistant","content":null,
            "tool_calls":[{"id":"c1","type":"function",
                "function":{"name":"grep","arguments":"{}"}}]
        }),
        json!({"role":"tool","tool_call_id":"c1","content": huge}),
    ];
    let (out, outcome) = mgr.trim_to_budget(msgs, "anthropic/claude-sonnet-4-6", 1);
    assert_eq!(outcome.evicted, 0, "must not evict the question");
    assert_eq!(outcome.truncated, 1, "must truncate the in-turn result");
    assert_eq!(out.len(), 4);
    assert_eq!(out[1]["content"], "Where is trim_to_budget defined?");
    assert!(out[3]["content"].as_str().unwrap().contains("grep hit"));
    assert!(out[3]["content"].as_str().unwrap().contains("truncated"));
}

/// Why: The water-fill cap is the core of the truncation strategy — it must
/// pick the largest cap whose capped sum fits the available budget.
/// What: A single dominant message gets (avail − others); several equal
/// oversized messages split the remainder equally; an empty slice yields 0.
/// Test: This test.
#[test]
fn water_fill_cap_computes_equitable_cap() {
    // Empty region.
    assert_eq!(water_fill_cap(&[], 1000), 0);
    // Single dominant message: keeps avail minus the small ones.
    // [5, 1_000_000], avail 500 -> 5 fits, cap = 495.
    assert_eq!(water_fill_cap(&[5, 1_000_000], 500), 495);
    // Three equal huge messages split the budget: 90_000 / 3 = 30_000.
    assert_eq!(
        water_fill_cap(&[1_000_000, 1_000_000, 1_000_000], 90_000),
        30_000
    );
    // Mixed: small fits fully, rest split. [10, 10, 10_000], avail 1000
    // -> 10 + 10 consumed, cap = 980 over the last one.
    assert_eq!(water_fill_cap(&[10, 10, 10_000], 1000), 980);
}

/// Why: The token estimate for the MCP-shaped `tool` message must be sane —
/// proportional to its content bytes — so the trimmer's size accounting is
/// correct for this shape (it is what triggers truncation).
/// What: A `tool` message with a known-length string content estimates to
/// ~len/4 tokens; a huge one estimates huge (justifying truncation).
/// Test: This test.
#[test]
fn estimate_tokens_sane_for_mcp_tool_message() {
    let small = json!({"role":"tool","tool_call_id":"c1","content":"a".repeat(400)});
    assert_eq!(estimate_tokens(&small), 100); // 400 bytes / 4

    let huge = json!({"role":"tool","tool_call_id":"c1","content":"a".repeat(4_000_000)});
    assert_eq!(estimate_tokens(&huge), 1_000_000); // 4 MB / 4
}

/// Why: Truncation must only touch string content; a null/absent or
/// structured content must be left alone so the caller can fall back to
/// eviction rather than corrupt the message.
/// What: A string content longer than target is shortened (returns true); a
/// null content is untouched (returns false).
/// Test: This test.
#[test]
fn truncate_message_content_string_vs_non_string() {
    let mut str_msg = json!({"role":"tool","content":"x".repeat(10_000)});
    assert!(truncate_message_content(&mut str_msg, 256));
    assert!(str_msg["content"].as_str().unwrap().contains("truncated"));

    let mut null_msg = json!({"role":"assistant","content":null,"tool_calls":[]});
    assert!(!truncate_message_content(&mut null_msg, 256));
    assert!(null_msg["content"].is_null());
}
