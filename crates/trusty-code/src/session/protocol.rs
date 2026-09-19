//! `session.*` JSON-RPC method handlers (#2054, vision spec Axiom 4 / §4.3).
//!
//! Why: this is the API surface Axiom 4 requires — every session operation
//! goes through JSON-RPC, reachable identically over STDIO and HTTP, so the
//! CLI (and later TUI/TELGUI/REST) never touch `SessionRegistry` directly.
//! What: [`register`] wires `session.create`, `session.list`,
//! `session.status`, `session.send`, `session.attach`, `session.detach`,
//! `session.cancel`, (#2058) `session.get_transcript`, (#2350)
//! `session.set_goal`/`session.clear_goal`/`session.get_goals`,
//! (DOC-39 §5.6 Slice D) `session.get_readiness`, (DOC-39 §5.4)
//! `session.get_agents`, (issue #3015) `session.get_context_budget`, and
//! (issue #3072) `session.get_search_audit` onto a [`Router`], all
//! closed over the SAME `Arc<SessionRegistry>` so every method sees a
//! consistent view. Each handler parses its typed `params`, forwards to the
//! matching `SessionRegistry` method, and maps the result onto the JSON-RPC
//! result shape the vision spec's §4.3 examples describe.
//! The three goal methods' handler bodies live in the sibling
//! `protocol_goals` module, `session.get_readiness`'s in the sibling
//! `protocol_readiness` module, `session.get_agents`'s in the sibling
//! `protocol_agents` module, and `session.get_context_budget`'s in the
//! sibling `protocol_budget` module (all kept out of this file purely for
//! the 500-SLOC cap — see each module's docs), but are registered here
//! alongside every other `session.*` method so this remains the one place
//! listing the full surface.
//! Test: `protocol::tests::*` (parameter validation, error mapping);
//! `protocol_goals::tests::*` (the three goal methods);
//! `protocol_readiness::tests::*` (`session.get_readiness`);
//! `protocol_agents::tests::*` (`session.get_agents`);
//! `protocol_budget::tests::*` (`session.get_context_budget`); the full
//! attach/detach streaming behaviour is covered by `session::registry_tests`
//! (registry-level) and `tests/session_e2e.rs`/`tests/task_e2e.rs`
//! (API-driven, real daemon).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::binding::ProjectBinding;
use crate::jsonrpc::{ConnectionContext, Router, RpcError};
use crate::workstreams::SharedWorkstreamStore;

use super::registry::SessionRegistry;

/// How long [`cancel`] waits for a signalled run to actually stop before
/// answering with an error instead of a cancelled snapshot (#8207).
///
/// Why: cancellation is observed at TURN boundaries, so the wait has to cover
/// finishing whatever tool call is in flight — a `bash` step, a delegated
/// sub-agent's turn — not just the flag check. The upper bound is not a taste
/// question: this grace and the TUI's
/// [`crate::tui_client::uds_rpc::DEFAULT_CALL_TIMEOUT`] are ONE contract,
/// because the client's budget covers the daemon's whole wait. The first cut
/// was thirty seconds against a fifteen-second client budget, so every cancel
/// taking 15-30s reached the user as a transport timeout and the fail-closed
/// `cancel_unconfirmed` reply was unreachable from the TUI. Ten seconds leaves
/// the answer five seconds to be written and read.
///
/// It also bounds a second stall: `crate::serve::transport` awaits each
/// dispatch inline, so a connection serving STDIO is blocked for exactly this
/// long while a cancel is confirming. Shortening the grace is the whole of the
/// mitigation here — restructuring that transport is not in #8207's scope.
/// Test: `protocol::tests::cancel_that_cannot_confirm_the_stop_is_an_error`
/// pins the fail-closed arm this bound exists to reach;
/// `protocol::tests::the_cancel_grace_fits_inside_the_clients_call_budget`
/// pins the contract with the client constant.
const CANCEL_CONFIRM_GRACE: Duration = Duration::from_secs(10);

/// #8207: a build-time stop on the two halves of the cancel contract being
/// changed out of order — the daemon's wait must leave the client's call
/// budget room to carry the answer back.
const _: () = assert!(
    CANCEL_CONFIRM_GRACE.as_secs() + CANCEL_ANSWER_HEADROOM.as_secs()
        <= crate::tui_client::uds_rpc::DEFAULT_CALL_TIMEOUT.as_secs(),
    "CANCEL_CONFIRM_GRACE must fit inside tui_client's DEFAULT_CALL_TIMEOUT \
     with headroom for the reply — see #8207"
);

/// How much of the client's call budget is reserved for everything that is not
/// the wait itself: dialling the socket, framing, and writing the reply.
const CANCEL_ANSWER_HEADROOM: Duration = Duration::from_secs(5);

/// Register every `session.*` method onto `router`, all sharing `registry`.
///
/// Why: the one place that lists the full `session.*` surface — mirrors
/// `crate::serve::methods::register`'s role for the proof-of-life methods.
/// What: clones `registry` once per method (cheap — `Arc`) into a small
/// adapter closure that forwards to the corresponding free function below.
/// Test: `protocol::tests::register_wires_every_session_method`.
///
/// (Issue #3298) `workstreams` is the daemon's shared workstream store —
/// `session.create` resolves and persists the session's binding (explicit
/// `workstream_id` param, or DOC-48 §4.2's ambient active-workstream
/// default) through it before returning.
pub fn register(
    router: &mut Router,
    registry: Arc<SessionRegistry>,
    workstreams: SharedWorkstreamStore,
) {
    let r = registry.clone();
    let ws = workstreams.clone();
    router.register(
        "session.create",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            let ws = ws.clone();
            async move { create(&r, &ws, params, ctx).await }
        },
    );

    let r = registry.clone();
    router.register(
        "session.list",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { list(&r, params, ctx).await }
        },
    );

    let r = registry.clone();
    router.register(
        "session.status",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { status(&r, params, ctx).await }
        },
    );

    let r = registry.clone();
    router.register(
        "session.send",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { send(&r, params, ctx).await }
        },
    );

    let r = registry.clone();
    router.register(
        "session.attach",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { attach(&r, params, ctx).await }
        },
    );

    let r = registry.clone();
    router.register(
        "session.detach",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { detach(&r, params, ctx).await }
        },
    );

    let r = registry.clone();
    router.register(
        "session.cancel",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { cancel(&r, params, ctx).await }
        },
    );

    let r = registry.clone();
    router.register(
        "session.get_transcript",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { get_transcript(&r, params, ctx).await }
        },
    );

    let r = registry.clone();
    router.register(
        "session.set_goal",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { protocol_goals::set_goal(&r, params, ctx).await }
        },
    );

    let r = registry.clone();
    router.register(
        "session.clear_goal",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { protocol_goals::clear_goal(&r, params, ctx).await }
        },
    );

    let r = registry.clone();
    router.register(
        "session.get_goals",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { protocol_goals::get_goals(&r, params, ctx).await }
        },
    );

    let r = registry.clone();
    router.register(
        "session.get_readiness",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { protocol_readiness::get_readiness(&r, params, ctx).await }
        },
    );

    let r = registry.clone();
    router.register(
        "session.get_agents",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { protocol_agents::get_agents(&r, params, ctx).await }
        },
    );

    let r = registry.clone();
    router.register(
        "session.get_context_budget",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { protocol_budget::get_context_budget(&r, params, ctx).await }
        },
    );

    let r = registry.clone();
    router.register(
        "session.get_search_audit",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { protocol_search_audit::get_search_audit(&r, params, ctx).await }
        },
    );
}

/// `params` shape shared by every method that only needs a session id
/// (`status`, `attach`, `detach`, `cancel`).
#[derive(Deserialize)]
struct SessionIdParams {
    session_id: String,
}

/// `params` shape for `session.create`.
///
/// `project` is now a project PATH, not the free-form label it used to be — see
/// [`create`]'s docs for the reconciliation and its compatibility implications.
#[derive(Deserialize)]
struct CreateParams {
    task: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    project: Option<PathBuf>,
    /// (Issue #3298, DOC-48 §4.1) Explicit workstream to bind this NEW
    /// session to. `None` falls back to §4.2's ambient active-workstream
    /// default (or stays projectless if nothing is active) — see
    /// [`create`]'s docs.
    #[serde(default)]
    workstream_id: Option<String>,
    /// (#8184) Opt this session INTO the delegating PM — the pre-#8184 shape,
    /// where the top-level agent gets `delegate_to_agent` and no filesystem
    /// tools of its own.
    ///
    /// Why: an interactive session's default is now the #8031 solo agent (see
    /// [`create`]'s docs), so PM/delegation mode needs a way to be asked for.
    /// What: `#[serde(default)]` — an omitted field is `false`, i.e. SOLO.
    /// Inverted into [`crate::session::Session::no_delegate`] at the mint.
    /// Test: `tests::create_with_delegate_true_keeps_the_delegating_pm`.
    #[serde(default)]
    delegate: bool,
}

/// `params` shape for `session.send`.
#[derive(Deserialize)]
struct SendParams {
    session_id: String,
    input: String,
}

/// Deserialise `params` into `T`, mapping a failure onto
/// `-32602 Invalid params` with the method name for context.
fn parse<T: DeserializeOwned>(params: Value, method: &str) -> Result<T, RpcError> {
    serde_json::from_value(params).map_err(|e| RpcError::invalid_params(format!("{method}: {e}")))
}

/// `session.create(task, agent?, project?) -> Session` (vision spec Axiom 4).
///
/// Why: mints a brand-new daemon-owned session.
/// What: validates `task` is non-empty (`-32003 invalid_argument`
/// otherwise), resolves `project` into a typed [`ProjectBinding`], then
/// delegates to `SessionRegistry::create`. The returned `Session` already has
/// `status: "running"` — see `session::model`'s docs on why M1 has no
/// queued/created-but-not-running state.
///
/// **The `project` reconciliation (spec DOC-39 §5.5, AC-16.2).** This param was
/// an untyped, free-form `Option<String>` LABEL: never validated, never bound,
/// not enumerable, and disconnected from the `PathBuf` `task.run` demanded. One
/// concept lived on two surfaces that could not agree. It is now the same
/// `ProjectBinding` `task.run` uses, resolved from a project PATH.
///
/// This is a deliberate BREAKING change to this method's contract: a caller that
/// previously passed a decorative label (`"my-app"`) now receives
/// `-32003 invalid_argument` unless that string names a real directory. Erroring
/// is the point — silently accepting a label that binds nothing and indexes
/// nothing is exactly the failure this reconciliation exists to end, and a
/// caller who learns their "project" was never real is strictly better off than
/// one who never finds out. Omitting `project` entirely remains valid and means
/// PROJECTLESS — a supported state, never an error (AC-2.1).
/// Test: `protocol::tests::create_rejects_empty_task`,
/// `protocol::tests::create_returns_running_session`,
/// `protocol::tests::create_without_project_is_projectless`,
/// `protocol::tests::create_binds_a_real_directory`,
/// `protocol::tests::create_rejects_a_label_that_is_not_a_directory`.
///
/// **(Issue #3298, DOC-48 §4.1/§4.2) Workstream binding.** Resolves and
/// VALIDATES the EFFECTIVE workstream target via
/// `crate::workstreams::protocol::resolve_validate_bind` — an explicit
/// `workstream_id` param wins; otherwise the daemon's active workstream is
/// the ambient default (§4.2); if neither applies the session stays
/// projectless (valid). **Validation happens BEFORE the session is minted**
/// (PR #3354 code-critic HIGH 1): `SessionRegistry` has no delete/rollback,
/// so a malformed/unknown/closed `workstream_id` must reject the call while
/// nothing has been created yet — otherwise every rejected bind would
/// strand an orphaned, caller-invisible session in the registry for the
/// daemon lifetime. The mint runs as a closure inside that helper, under
/// the SAME store lock as validation and the subsequent bind, keeping the
/// whole sequence TOCTOU-free (see the helper's docs).
/// Test: `workstream_binding_tests::create_binds_ambient_active_workstream`,
/// `workstream_binding_tests::create_binds_explicit_workstream_overriding_ambient`,
/// `workstream_binding_tests::create_stays_projectless_without_explicit_or_active`,
/// `workstream_binding_tests::create_rejects_closed_explicit_workstream`,
/// `workstream_binding_tests::create_rejected_bind_leaves_no_phantom_session`.
///
/// **(#8184) The session's agent shape defaults to SOLO.** A session minted
/// here runs #8031's single-agent path: the named agent carries its own tcode
/// tools (read/edit/write/bash) and `delegate_to_agent` is never registered.
/// This is a deliberate default change — `tcode tui`'s interactive session is
/// minted through this method and could not do a basic read-edit loop while
/// the delegating PM registry (which has no filesystem tools at all) was the
/// only shape available. `delegate: true` asks for the pre-#8184 PM instead;
/// `task.run`'s own mint path is untouched and stays delegating by default.
/// The chosen shape is persisted on `Session.no_delegate`, so
/// `session.status`/`session.list` state which agent a session runs.
/// Test: `tests::create_defaults_to_the_solo_agent`,
/// `tests::create_with_delegate_true_keeps_the_delegating_pm`,
/// `tests::create_without_project_defaults_to_the_solo_agent`.
async fn create(
    registry: &SessionRegistry,
    workstreams: &SharedWorkstreamStore,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: CreateParams = parse(params, "session.create")?;
    if p.task.trim().is_empty() {
        return Err(RpcError::invalid_argument("task must not be empty"));
    }
    let binding = ProjectBinding::resolve(p.project)
        .map_err(|e| RpcError::invalid_argument(format!("session.create: {e}")))?;
    protocol_workstream::mint_bound_session(
        registry,
        workstreams,
        p.workstream_id.as_deref(),
        "session.create",
        // #8184: solo unless the caller explicitly asks for the PM.
        || {
            registry
                .create_with_delegation(p.task, p.agent, binding, !p.delegate)
                .id
        },
    )
    .await
}

/// `session.list() -> [Session]` (vision spec Axiom 4).
///
/// Why: enumerates every session currently owned by the daemon.
/// What: wraps `SessionRegistry::list` as `{"sessions": [...]}`.
/// Test: `protocol::tests::list_returns_sessions_key`.
async fn list(
    registry: &SessionRegistry,
    _params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    Ok(json!({ "sessions": registry.list() }))
}

/// `session.status(session_id) -> Session` (vision spec Axiom 4).
///
/// Why: point lookup for one session's current state.
/// What: `-32007 session_not_found` if unknown; otherwise the `Session`.
/// Test: `protocol::tests::status_unknown_session_maps_to_session_not_found`.
async fn status(
    registry: &SessionRegistry,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: SessionIdParams = parse(params, "session.status")?;
    Ok(json!(registry.status(&p.session_id)?))
}

/// `session.send(session_id, input) -> { acknowledged }` (vision spec
/// Axiom 4).
///
/// Why: the client -> daemon input path.
/// What: `-32007 session_not_found` if unknown; otherwise
/// `{"acknowledged": true}` after `SessionRegistry::send` publishes the
/// observable `Event::SessionInput`.
/// Test: `protocol::tests::send_unknown_session_maps_to_session_not_found`.
async fn send(
    registry: &SessionRegistry,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: SendParams = parse(params, "session.send")?;
    registry.send(&p.session_id, &p.input)?;
    Ok(json!({ "acknowledged": true }))
}

/// `session.attach(session_id) -> { session_id, events, stream_url }`
/// (vision spec Axiom 4 / §4.4 session-attach protocol).
///
/// Why: the streaming half of the protocol. `events` is the ring-buffer
/// replay (§12, 11.2) so a freshly-attached client sees recent history
/// immediately; live events follow as server-initiated notifications.
/// What: over STDIO, `ctx.notify` is the long-lived per-process channel —
/// live events are pushed on the SAME connection as
/// `{"jsonrpc":"2.0","method":"session.event","params":{...}}` lines,
/// interleaved with ordinary responses. Over HTTP, `ctx.notify` is a
/// throwaway per-request channel (the forwarder self-terminates once the
/// response is written); the real HTTP live-streaming path is the
/// dedicated `GET /sessions/{id}/events` SSE route
/// (`crate::serve::http::session_events_sse`), which is why `stream_url` is
/// always included — HTTP clients need it, STDIO clients simply ignore it.
/// `-32007 session_not_found` if `session_id` is unknown.
/// Test: `protocol::tests::attach_unknown_session_maps_to_session_not_found`;
/// the streaming behaviour itself is covered by
/// `session::registry_tests::attach_forwards_live_events_until_detach` and
/// the API-driven `tests/session_e2e.rs`.
async fn attach(
    registry: &SessionRegistry,
    params: Value,
    ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: SessionIdParams = parse(params, "session.attach")?;
    let events = registry.attach(&p.session_id, ctx.connection_id, ctx.notify.clone())?;
    Ok(json!({
        "session_id": p.session_id,
        "events": events,
        "stream_url": format!("/sessions/{}/events", p.session_id),
    }))
}

/// `session.detach(session_id) -> {}` (vision spec Axiom 4).
///
/// Why: stops this connection's live-event forwarding for the session.
/// What: idempotent — detaching without a prior attach is a success no-op
/// (see `SessionRegistry::detach`). `-32007 session_not_found` if
/// `session_id` itself is unknown.
/// Test: `protocol::tests::detach_unknown_session_maps_to_session_not_found`.
async fn detach(
    registry: &SessionRegistry,
    params: Value,
    ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: SessionIdParams = parse(params, "session.detach")?;
    registry.detach(&p.session_id, ctx.connection_id)?;
    Ok(json!({}))
}

/// `session.cancel(session_id) -> Session` (vision spec §12, 11.6
/// Cancellation Semantics).
///
/// Why: explicit termination signal. #2056 splits this into two paths: a
/// session with a background `task.run` execution in flight must be
/// signalled COOPERATIVELY (the executing `AgentLoop`(s) observe the flag at
/// their next turn boundary and unwind themselves — see
/// `agent_loop::AgentLoopError::Cancelled` — before `crate::task::executor`
/// lands the terminal transition via `SessionRegistry::finish`); a session
/// with nothing executing keeps the original #2054 immediate-transition
/// behaviour.
/// What: idempotent either way. `-32007 session_not_found` if `session_id`
/// is unknown. When `SessionRegistry::is_executing` is true, calls
/// `request_cancel` (sets the flag) and then BLOCKS on
/// `SessionRegistry::await_cancelled` until the spawned run has actually
/// stopped, returning the post-stop snapshot. Otherwise falls back to
/// `SessionRegistry::cancel` (the #2054 immediate `status: "cancelled"` path).
///
/// (#8207) The wait is the fix for a client being told a task had stopped
/// while it had not: this used to return the still-`running` snapshot the
/// instant the flag was set, so the caller reopened its input and the next
/// `task.run` was rejected with `-32003 invalid_argument` ("already has a task
/// running"). The reply is now the answer to "did it stop", which makes
/// "reported cancelled" and "actually stopped" one fact instead of two. The
/// wait is FAIL-CLOSED: a run that outlives [`CANCEL_CONFIRM_GRACE`] answers
/// with `await_cancelled`'s `-32010 cancel_unconfirmed` error, never a
/// cancelled snapshot, so a client can say "still cancelling" rather than lie
/// — and can tell that apart from a `-32603` daemon fault, which is why the
/// code is a domain one rather than `internal`.
/// Test: `protocol::tests::cancel_unknown_session_maps_to_session_not_found`,
/// `protocol::tests::cancel_executing_session_requests_cooperative_cancel`,
/// `protocol::tests::cancel_waits_for_the_task_to_stop_before_reporting`,
/// `protocol::tests::a_prompt_right_after_cancel_is_accepted_not_rejected`,
/// `protocol::tests::cancel_that_cannot_confirm_the_stop_is_an_error`,
/// `protocol::tests::the_cancel_grace_fits_inside_the_clients_call_budget`.
async fn cancel(
    registry: &SessionRegistry,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: SessionIdParams = parse(params, "session.cancel")?;
    if registry.is_executing(&p.session_id) {
        registry.request_cancel(&p.session_id)?;
        registry
            .await_cancelled(&p.session_id, CANCEL_CONFIRM_GRACE)
            .await?;
        Ok(json!(registry.status(&p.session_id)?))
    } else {
        Ok(json!(registry.cancel(&p.session_id)?))
    }
}

/// `session.get_transcript(session_id) -> TranscriptRecord` (#2058, vision
/// spec §11.1 Transcript Persistence / §4.3 API Surface).
///
/// Why: completes the M1 cut-line's "inspect transcript" verb — read-only
/// access to the run record `task.run` (#2056) persists via
/// `SessionRegistry::set_run_outcome`.
/// What: `-32007 session_not_found` if `session_id` is unknown; otherwise
/// `SessionRegistry::get_transcript`'s `TranscriptRecord` verbatim — turns,
/// aggregate usage, and stored cost, never recomputed here. A session that
/// has never run a task returns a `TranscriptRecord` with an empty `turns`
/// array rather than an error (see `SessionRegistry::get_transcript`'s docs).
/// Test: `protocol::tests::get_transcript_unknown_session_maps_to_session_not_found`,
/// `protocol::tests::get_transcript_on_never_run_session_is_empty`.
async fn get_transcript(
    registry: &SessionRegistry,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: SessionIdParams = parse(params, "session.get_transcript")?;
    Ok(json!(registry.get_transcript(&p.session_id)?))
}

/// `session.set_goal`/`session.clear_goal`/`session.get_goals` (#2350),
/// split into their own file purely to keep this production file under the
/// crate's 500-SLOC cap — a child module of `protocol` (declared via
/// `#[path = ...] mod protocol_goals;`), so it shares full access to this
/// module's private `SessionIdParams`/`parse` helpers exactly as if these
/// handlers were still defined here.
#[path = "protocol_goals.rs"]
mod protocol_goals;

/// `session.create`'s workstream-binding step (DOC-48 §4.1/§4.2, issue
/// #3298), split out for the same 500-SLOC-cap reason as `protocol_goals`
/// above.
#[path = "protocol_workstream.rs"]
mod protocol_workstream;

/// `session.get_readiness` (DOC-39 §5.6 Slice D), split into its own file for
/// the same 500-SLOC-cap reason as `protocol_goals` above.
#[path = "protocol_readiness.rs"]
mod protocol_readiness;

/// `session.get_agents` (DOC-39 §5.4), split into its own file for the same
/// 500-SLOC-cap reason as `protocol_goals` above.
#[path = "protocol_agents.rs"]
mod protocol_agents;

/// `session.get_context_budget` (issue #3015), split into its own file for
/// the same 500-SLOC-cap reason as `protocol_goals` above.
#[path = "protocol_budget.rs"]
mod protocol_budget;

/// `session.get_search_audit` (issue #3072), split into its own file for the
/// same 500-SLOC-cap reason as `protocol_goals` above.
#[path = "protocol_search_audit.rs"]
mod protocol_search_audit;

/// `protocol::tests` (parameter validation, error mapping), split into its
/// own file for the same reason `sessions_write_tests.rs`/`registry_tests.rs`
/// are split — a `_tests.rs`-suffixed sibling file falls under the crate's
/// 1500-SLOC test cap rather than counting against this production file's
/// 500-SLOC budget.
#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;

/// `protocol::binding_tests` (project-binding validation, AC-2.1/AC-16.2),
/// split out for the same reason as `tests` above.
#[cfg(test)]
#[path = "protocol_binding_tests.rs"]
mod binding_tests;

/// `session.create`'s workstream-binding tests (issue #3298), split into its
/// own file for the same reason `sessions_write_tests.rs`/`registry_tests.rs`
/// are split — a `_tests.rs`-suffixed sibling file falls under the crate's
/// 1500-SLOC test cap rather than counting against this production file's
/// 500-SLOC budget.
#[cfg(test)]
#[path = "protocol_workstream_binding_tests.rs"]
mod workstream_binding_tests;
