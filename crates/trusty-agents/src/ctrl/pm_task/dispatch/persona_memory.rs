//! Bound-palace memory recall, truthful memory introspection, and chat-turn
//! persistence for persona chat turns (issue #3928, toward DOC-54 §9.5/§9.6).
//!
//! Why: an agent's `[[stores]]` binding (#3878) declares what it KNOWS — for
//! Izzie, `palace = "owner-profile"` (12 drawers: an `identity`-tagged
//! self-description plus rich owner-profile facts) and `index = "bob-kb"`
//! (552 chunks). Until this module, `binding.palace` was **inert on the chat
//! path**: `persona.rs` read `stores.default_search_index()` to point
//! `vector_search` at the right corpus and nothing else, and
//! `classification::finish_turn` persisted turns to the *project-slug* palace
//! (`project_slug_at(cwd)`), never the agent's own. The runtime therefore
//! handed the model no memory at all, and Izzie — correctly, given what she
//! could see — told the owner *"I don't have memories that persist across
//! conversations — each time we talk, I start fresh."* That is false, and it
//! is the product-defining defect: the promise is infinite context.
//! What: three cooperating pieces, all fail-open.
//!   - [`build_persona_memory`] resolves the primary binding, fetches the
//!     `identity`-tagged drawers UNCONDITIONALLY (an agent must always know
//!     who it is) and semantically recalls the turn's query against the bound
//!     palace, recording honestly whether the daemon answered
//!     ([`MemoryHealth`]).
//!   - [`render_memory_block`] renders that into the prompt block, combining
//!     the recalled facts with an INTROSPECTED memory-architecture section
//!     built from live binding data (palace name, index name + real chunk
//!     count, whether recall actually succeeded). Introspection over
//!     injection is a standing owner rule: nothing here is a hardcoded "you
//!     have infinite memory" literal — every claim is derived from a probe,
//!     so a degraded binding produces a degraded (and still truthful) claim.
//!   - [`spawn_persist_turn`] writes the turn to the bound palace's
//!     chat-session store so future sessions can recall past conversations.
//!
//! Degradation ladder (a down daemon must never break chat):
//!   1. No `[[stores]]` binding, or one without a `palace` → [`render_memory_block`]
//!      returns `None`: zero prompt change, a real no-op. An agent with no
//!      bound memory keeps saying it has none, because that is TRUE for it.
//!   2. Palace bound but unreachable → the block still renders, stating the
//!      memory is temporarily unavailable and that the stored memories still
//!      exist. The agent must not conclude it is stateless from one failed read.
//!   3. Recall succeeds but returns nothing → rendered as an explicit
//!      "nothing matched this turn", never as absent memory.
//!   4. Persistence failures are logged and dropped — the user's answer is
//!      already on its way (mirrors `classification::finish_turn`'s detached
//!      write, #3840 critic HIGH-4).
//!
//! Session identity: turns land in ONE continuous, resumable chat session per
//! agent, keyed by the deterministic caller-supplied id
//! [`session_id_for`] (`persona-{agent}`) — trusty-memory's
//! `chat_session_create` is idempotent on a caller-chosen id (an existing
//! session is returned unchanged), so resuming needs no list-and-match scan.
//! This follows DOC-54 §9.1 ("Each agent maintains ONE continuous conversation
//! per session — not separate chat contexts") rather than fragmenting per
//! app launch.
//!
//! Test: `persona_memory_tests.rs` — pure rendering/truncation/session-id
//! tests plus mock-daemon coverage of recall injection, unconditional
//! identity inclusion, unreachable-palace degradation, and the introspection
//! block reflecting real binding data.

use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::stores::StoresConfig;

/// How many drawers to pull from the bound palace per turn.
///
/// Why: enough to carry the profile facts a conversational answer needs
/// (family/location/work each live in their own drawer) without crowding the
/// prompt or the model's attention. Recall is score-ranked, so the tail is
/// the least useful part of a larger window.
pub(crate) const RECALL_TOP_K: usize = 6;

/// Tag marking an agent's own self-description drawer(s).
const IDENTITY_TAG: &str = "identity";

/// Upper bound on identity drawers injected. An agent's self-description is a
/// handful of statements; a runaway tag must not flood the prompt.
const IDENTITY_LIMIT: usize = 8;

/// Per-drawer character budget in the rendered block. Drawer content is
/// operator/agent-authored prose of unbounded length; a single pathological
/// drawer must not dominate the context window.
const MAX_DRAWER_CHARS: usize = 800;

/// Connect timeout for daemon reads — kept short so a down daemon costs a
/// handshake, not a turn (mirrors `api::server::workstreams`).
const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);

/// Per-call timeout for daemon reads/writes.
const CALL_TIMEOUT: Duration = Duration::from_secs(3);

/// Whether the bound palace actually answered this turn.
///
/// Why: the introspection block's honesty depends on distinguishing "you have
/// no memory" from "your memory exists but I could not read it just now" —
/// collapsing the two is exactly the bug this module fixes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MemoryHealth {
    /// The agent's binding declares no palace — it genuinely has no
    /// cross-conversation recall.
    NoPalaceBound,
    /// The palace answered (possibly with zero matching drawers).
    Reachable,
    /// The palace is bound but did not answer; the reason is surfaced verbatim.
    Unavailable(String),
}

/// Live, introspected facts about the agent's `[[stores]]` binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BindingFacts {
    /// Bound trusty-memory palace id, when one is declared.
    pub(crate) palace: Option<String>,
    /// Resolved trusty-search index id (declared `index`, else the store name).
    pub(crate) index: String,
    /// Real chunk count from the search daemon, when it answered.
    pub(crate) index_chunk_count: Option<u64>,
    /// Whether the search index probe succeeded.
    pub(crate) index_connected: bool,
}

/// Everything the prompt builder needs about this turn's memory.
#[derive(Debug, Clone)]
pub(crate) struct PersonaMemory {
    /// `None` when the agent declares no `[[stores]]` binding at all.
    pub(crate) binding: Option<BindingFacts>,
    /// `identity`-tagged drawer contents — the agent's own self-description.
    pub(crate) identity: Vec<String>,
    /// Query-relevant drawer contents recalled for this turn.
    pub(crate) recalled: Vec<String>,
    pub(crate) health: MemoryHealth,
}

impl PersonaMemory {
    /// The all-quiet value used when an agent has no store binding.
    fn unbound() -> Self {
        Self {
            binding: None,
            identity: Vec::new(),
            recalled: Vec::new(),
            health: MemoryHealth::NoPalaceBound,
        }
    }
}

/// Deterministic, resumable chat-session id for `agent_name`.
///
/// Why: trusty-memory's REST `create` mints a random UUID and accepts no
/// caller id, which would strand every launch in a fresh unreachable session.
/// The RPC `chat_session_create` DOES accept a caller-chosen id and is
/// idempotent on it, so a stable derived id gives resumability with no
/// list-and-match scan and no local state to keep in sync.
/// Test: `session_id_is_stable_and_agent_scoped`.
pub(crate) fn session_id_for(agent_name: &str) -> String {
    format!("persona-{agent_name}")
}

/// Shared HTTP client for daemon reads. `None` when the client cannot be
/// built, which callers treat as "daemon unavailable" rather than an error.
fn build_http_client() -> Option<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(CALL_TIMEOUT)
        .build()
        .ok()
}

/// Minimal projection of trusty-memory's `Drawer` — the recall and
/// list-drawers routes both return this shape (recall adds `score`/`layer`,
/// which ranking already applied server-side, so neither is read here).
#[derive(Debug, Deserialize)]
struct DrawerRow {
    content: String,
    #[serde(default)]
    tags: Vec<String>,
}

/// Truncate `s` to at most [`MAX_DRAWER_CHARS`] characters on a CHARACTER
/// boundary, appending an ellipsis marker when it was cut.
///
/// Why: byte-slicing untrusted UTF-8 prose panics mid-codepoint (#3685) —
/// drawer content is routinely non-ASCII (the owner profile carries Japanese
/// heritage notes and typographic em-dashes).
/// Test: `truncate_is_utf8_safe_on_multibyte_content`,
/// `truncate_leaves_short_content_untouched`.
fn truncate_drawer(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= MAX_DRAWER_CHARS {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(MAX_DRAWER_CHARS).collect();
    format!("{cut}…")
}

/// Fetch drawers carrying an exact `tag` from `palace`.
///
/// Returns `Err` with a human-readable reason so the caller can surface it in
/// the introspection block rather than silently degrading to "no memory".
async fn fetch_drawers_by_tag(
    client: &reqwest::Client,
    base_url: &str,
    palace: &str,
    tag: &str,
    limit: usize,
) -> Result<Vec<DrawerRow>, String> {
    let url = format!(
        "{}/api/v1/palaces/{palace}/drawers",
        base_url.trim_end_matches('/')
    );
    let resp = client
        .get(&url)
        .query(&[("tag", tag), ("limit", &limit.to_string())])
        .send()
        .await
        .map_err(|e| format!("trusty-memory unreachable: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!(
            "trusty-memory returned HTTP {} for palace `{palace}`",
            resp.status()
        ));
    }
    resp.json::<Vec<DrawerRow>>()
        .await
        .map_err(|e| format!("unreadable drawer list: {e}"))
}

/// Semantically recall `query` against `palace`.
///
/// Note the query parameter is `q` — trusty-memory's REST `RecallQuery` uses
/// `q`, while its RPC `memory_recall` tool uses `query`. Using the wrong one
/// is a 400, not an empty result.
async fn recall_drawers(
    client: &reqwest::Client,
    base_url: &str,
    palace: &str,
    query: &str,
    top_k: usize,
) -> Result<Vec<DrawerRow>, String> {
    let url = format!(
        "{}/api/v1/palaces/{palace}/recall",
        base_url.trim_end_matches('/')
    );
    let resp = client
        .get(&url)
        .query(&[("q", query), ("top_k", &top_k.to_string())])
        .send()
        .await
        .map_err(|e| format!("trusty-memory unreachable: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!(
            "trusty-memory returned HTTP {} recalling from `{palace}`",
            resp.status()
        ));
    }
    resp.json::<Vec<DrawerRow>>()
        .await
        .map_err(|e| format!("unreadable recall response: {e}"))
}

/// Probe the bound search index for its real chunk count.
///
/// Kept separate from `crate::stores::resolve_store_statuses` (which probes
/// the palace too, duplicating the reads this module already performs) so a
/// turn makes exactly one search-daemon call.
async fn probe_index(
    client: &reqwest::Client,
    search_base: Option<&str>,
    index: &str,
) -> (bool, Option<u64>) {
    let Some(base) = search_base else {
        return (false, None);
    };
    let url = format!("{}/indexes/{index}/status", base.trim_end_matches('/'));
    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<serde_json::Value>().await {
            Ok(v) => (
                true,
                v.get("chunk_count").and_then(serde_json::Value::as_u64),
            ),
            Err(_) => (true, None),
        },
        _ => (false, None),
    }
}

/// Assemble this turn's memory context from the agent's `[[stores]]` binding.
///
/// Why: this is the seam the defect lived in — the binding was resolved for
/// `vector_search` routing and then dropped, so nothing ever read
/// `binding.palace` on the chat path.
/// What: takes the FIRST binding (`StoresConfig::primary` — exactly one store
/// per agent is the owner's rule), unconditionally fetches `identity`-tagged
/// drawers, and recalls `query`. Identity drawers are de-duplicated out of the
/// recall list so a self-description that also scores well is not printed
/// twice. Never returns an error: an unreachable daemon yields
/// [`MemoryHealth::Unavailable`] with the reason, never a failed turn.
/// Test: `build_persona_memory_*` (mock-daemon, `persona_memory_tests.rs`).
pub(crate) async fn build_persona_memory(
    stores: &StoresConfig,
    memory_base: Option<&str>,
    search_base: Option<&str>,
    query: &str,
) -> PersonaMemory {
    let Some(binding) = stores.primary() else {
        return PersonaMemory::unbound();
    };
    let Some(client) = build_http_client() else {
        return PersonaMemory::unbound();
    };

    let index = binding.resolved_index().to_string();
    let (index_connected, index_chunk_count) = probe_index(&client, search_base, &index).await;
    let facts = BindingFacts {
        palace: binding.palace.clone(),
        index,
        index_chunk_count,
        index_connected,
    };

    let (Some(palace), Some(base)) = (binding.palace.as_deref(), memory_base) else {
        // A store with no palace has no cross-conversation recall — that is a
        // configuration fact, not a failure, and the block says so.
        return PersonaMemory {
            binding: Some(facts),
            identity: Vec::new(),
            recalled: Vec::new(),
            health: MemoryHealth::NoPalaceBound,
        };
    };

    let identity_rows =
        fetch_drawers_by_tag(&client, base, palace, IDENTITY_TAG, IDENTITY_LIMIT).await;
    let recall_rows = recall_drawers(&client, base, palace, query, RECALL_TOP_K).await;

    // Health follows whether recall — the load-bearing read — answered. A
    // missing identity tag is a content gap, not an outage.
    let health = match (&identity_rows, &recall_rows) {
        (_, Ok(_)) => MemoryHealth::Reachable,
        (Ok(_), Err(reason)) | (Err(reason), Err(_)) => MemoryHealth::Unavailable(reason.clone()),
    };

    let identity: Vec<String> = identity_rows
        .unwrap_or_default()
        .iter()
        .map(|d| truncate_drawer(&d.content))
        .collect();

    let recalled: Vec<String> = recall_rows
        .unwrap_or_default()
        .iter()
        .filter(|d| !d.tags.iter().any(|t| t == IDENTITY_TAG))
        .map(|d| truncate_drawer(&d.content))
        .collect();

    tracing::info!(
        palace = %palace,
        identity_drawers = identity.len(),
        recalled_drawers = recalled.len(),
        health = ?health,
        "persona memory recalled from bound palace"
    );

    PersonaMemory {
        binding: Some(facts),
        identity,
        recalled,
        health,
    }
}

/// Render the introspected memory-architecture section.
///
/// Every sentence here is derived from a live probe — palace id and whether it
/// answered, index id, real chunk count. Introspection over injection: an
/// agent whose daemon is down describes a degraded memory, because that is
/// what it actually has this turn.
fn render_introspection(facts: &BindingFacts, health: &MemoryHealth) -> String {
    let mut lines = Vec::new();

    match (&facts.palace, health) {
        (Some(palace), MemoryHealth::Reachable) => lines.push(format!(
            "- Memory palace `{palace}`: connected. Long-term facts you have stored across \
             conversations; the entries below were recalled from it just now."
        )),
        (Some(palace), MemoryHealth::Unavailable(reason)) => lines.push(format!(
            "- Memory palace `{palace}`: TEMPORARILY UNREACHABLE ({reason}). Your stored \
             memories still exist — you simply cannot read them this turn. Say that plainly if \
             asked; do not conclude you have no memory."
        )),
        (Some(palace), MemoryHealth::NoPalaceBound) => lines.push(format!(
            "- Memory palace `{palace}`: bound but not consulted this turn."
        )),
        (None, _) => lines.push(
            "- No memory palace is bound to you, so you have no cross-conversation recall."
                .to_string(),
        ),
    }

    let index = &facts.index;
    match (facts.index_connected, facts.index_chunk_count) {
        (true, Some(chunks)) => lines.push(format!(
            "- Knowledge index `{index}`: connected, {chunks} indexed chunks. Search it with \
             your `vector_search` tool — an unqualified query defaults to this index."
        )),
        (true, None) => lines.push(format!(
            "- Knowledge index `{index}`: connected. Search it with your `vector_search` tool."
        )),
        (false, _) => lines.push(format!(
            "- Knowledge index `{index}`: not reachable right now, so semantic search over it \
             may fail this turn."
        )),
    }

    format!(
        "### Your memory architecture (introspected live, not asserted)\n{}",
        lines.join("\n")
    )
}

/// Render the full memory context block for the system prompt.
///
/// Returns `None` when the agent has no `[[stores]]` binding at all — a real
/// no-op, so unbound agents see a byte-identical prompt to before this change.
/// Test: `render_returns_none_without_binding`,
/// `render_includes_recall_and_identity`,
/// `render_degrades_truthfully_when_unavailable`,
/// `render_introspection_reflects_binding_facts`.
pub(crate) fn render_memory_block(memory: &PersonaMemory) -> Option<String> {
    let facts = memory.binding.as_ref()?;

    let mut out = String::from("## Your persistent memory\n\n");
    out.push_str(&render_introspection(facts, &memory.health));

    if !memory.identity.is_empty() {
        out.push_str("\n\n### Who you are (from your own identity memory)\n");
        for entry in &memory.identity {
            out.push_str(&format!("- {entry}\n"));
        }
        out.truncate(out.trim_end().len());
    }

    out.push_str("\n\n### Your persistent memory recalls, for this turn\n");
    match &memory.health {
        MemoryHealth::Unavailable(_) => out.push_str(
            "(memory temporarily unavailable — see above; this is a read failure, not an \
             absence of memories)",
        ),
        MemoryHealth::NoPalaceBound => {
            out.push_str("(no memory palace is bound to you, so there is nothing to recall)")
        }
        MemoryHealth::Reachable if memory.recalled.is_empty() => out.push_str(
            "(nothing in your stored memory matched this particular turn — your memory is \
             working, it simply had no relevant entry)",
        ),
        MemoryHealth::Reachable => {
            for entry in &memory.recalled {
                out.push_str(&format!("- {entry}\n"));
            }
            out.truncate(out.trim_end().len());
        }
    }

    out.push_str(
        "\n\nWhen asked what you remember, what you are, or how your memory works, answer from \
         THIS block. You have persistent memory that survives across conversations — never say \
         you start fresh each time, that each conversation begins blank, or that you cannot \
         remember previous sessions. Do not overclaim either: describe only what is listed \
         above, and say plainly when something simply was not recalled. Speak about it in plain \
         human language; you need not name the underlying palace or index unless the user asks \
         for that technical detail.",
    );

    Some(out)
}

/// Persist one completed turn to the bound palace's chat-session store, off
/// the response path.
///
/// Why: the user's answer is already returned by the time this runs — a
/// two-call write (idempotent session create, then turn append) must not sit
/// in front of it (mirrors `classification::finish_turn`, #3840 critic
/// HIGH-4). Failures are logged and dropped: losing a persisted turn must
/// never surface as a failed chat turn.
/// What: no-op unless the agent binds a palace. Uses trusty-memory's RPC
/// surface — `chat_turn_append` has no REST route, and RPC `chat_session_create`
/// is the only path accepting a caller-supplied (hence resumable) session id.
/// Test: `spawn_persist_turn_writes_both_messages` (mock-daemon),
/// `spawn_persist_turn_is_noop_without_palace`.
pub(crate) fn spawn_persist_turn(
    stores: &StoresConfig,
    memory_base: Option<&str>,
    agent_name: &str,
    user_input: &str,
    response: &str,
) {
    let (Some(palace), Some(base)) = (
        stores.primary().and_then(|b| b.palace.clone()),
        memory_base.map(str::to_string),
    ) else {
        return;
    };
    let session_id = session_id_for(agent_name);
    let agent = agent_name.to_string();
    let prompt = user_input.to_string();
    let reply = response.to_string();

    tokio::spawn(async move {
        if let Err(e) = persist_turn(&base, &palace, &session_id, &prompt, &reply).await {
            tracing::warn!(
                agent = %agent,
                palace = %palace,
                error = %e,
                "failed to persist persona chat turn to bound palace"
            );
        }
    });
}

/// Awaited body of [`spawn_persist_turn`], separated so tests can observe the
/// write deterministically instead of racing a detached task.
async fn persist_turn(
    base_url: &str,
    palace: &str,
    session_id: &str,
    user_input: &str,
    response: &str,
) -> Result<(), String> {
    let client = build_http_client().ok_or_else(|| "http client unavailable".to_string())?;

    // Idempotent: an existing session is returned unchanged, so this is safe
    // to issue on every turn and doubles as the resume path. No `title` is
    // sent — trusty-memory applies a title ONLY when it generates the session
    // id itself, so passing one alongside a caller-supplied id is silently
    // dropped (live-verified: the session reads back `title: null`). The
    // agent-scoped `session_id` already identifies the thread.
    call_memory_tool(
        &client,
        base_url,
        "chat_session_create",
        json!({
            "palace": palace,
            "session_id": session_id,
        }),
    )
    .await?;

    call_memory_tool(
        &client,
        base_url,
        "chat_turn_append",
        json!({
            "palace": palace,
            "session_id": session_id,
            "prompt": user_input,
            "response": response,
        }),
    )
    .await?;

    Ok(())
}

/// Invoke a trusty-memory MCP tool over the daemon's JSON-RPC surface.
///
/// The `chat_*` tools are absent from trusty-memory's direct-dispatch
/// `TOOL_METHODS` allow-list, so they are only reachable wrapped in
/// `tools/call`. `/rpc` always returns HTTP 200 — failures live in the
/// envelope's `error` member, so the status code alone proves nothing.
async fn call_memory_tool(
    client: &reqwest::Client,
    base_url: &str,
    tool: &str,
    arguments: serde_json::Value,
) -> Result<(), String> {
    let url = format!("{}/rpc", base_url.trim_end_matches('/'));
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": tool, "arguments": arguments },
    });
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("{tool}: trusty-memory unreachable: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("{tool}: HTTP {}", resp.status()));
    }
    let envelope: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("{tool}: unreadable response: {e}"))?;
    if let Some(err) = envelope.get("error") {
        return Err(format!("{tool}: rpc error: {err}"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "persona_memory_tests.rs"]
mod persona_memory_tests;
