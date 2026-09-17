//! `task.run` JSON-RPC method (#2056, vision spec §4.3): start a
//! session-driven task execution.
//!
//! Why: this is the API surface the M1 control-plane cut line needs — a
//! caller creates (or targets an existing) session and asks the daemon to
//! actually EXECUTE a task against it, asynchronously, streaming progress via
//! the #2054/#2055 `session.attach` path. Kept as its own JSON-RPC method
//! (rather than overloading `session.send`) because `session.send`'s
//! existing #2054/#2055 contract — record an observable `SessionInput` event
//! — is already tested and shipped; `task.run` is the deliberate, explicit
//! "start executing" entry point the issue names.
//! What: [`register`] wires `task.run` onto a [`Router`], sharing the SAME
//! `Arc<SessionRegistry>` every `session.*` method uses. Params match the
//! vision spec §4.3 example almost verbatim: `task_description`,
//! `agent_name` (top-level/PM agent, default `"pm"`), `context` (accepted for
//! forward-compatibility; not yet injected into the prompt — project
//! `CLAUDE.md` context already flows through `run_task`'s existing
//! machinery, and free-form per-call `context` injection is a smaller
//! follow-up, not core to this cut line), `model_override` (pins the
//! delegated ENGINEER's model for this run), and an ADDITIONAL optional
//! `session_id` for "sessionful" execution against an already-`session.create`d
//! session (the spec's "single-shot or sessionful" phrasing).
//! Test: `task::protocol::tests::*`; the full flow end-to-end (a real
//! subprocess) in `tests/task_e2e.rs`.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::binding::ProjectBinding;
use crate::jsonrpc::{ConnectionContext, Router, RpcError};
use crate::session::SessionRegistry;
use crate::workstreams::SharedWorkstreamStore;

use super::executor::{TaskRunParams, spawn_task_run};
use super::mock_llm::build_llm_client;

/// Default top-level/PM agent name used when `task.run`'s `agent_name` param
/// is omitted (#3437).
///
/// Why: #3437 — every GUI-initiated run omits `agent_name` (no agent-roster
/// endpoint exists yet for the GUI to pick from), so `task_run`'s default
/// MUST resolve against a real agent in every resolution tier (disk then
/// embedded, see `agents::resolve_agent`) or 100% of daemon-default runs fail
/// agent resolution before a single turn executes — exactly the failure mode
/// #3437 traced. A shared `pub` const (referenced from both this call site
/// and `assets::tests::default_task_run_agent_resolves_against_default_agents`)
/// is the drift-guard: the literal here and the embedded roster entry it must
/// resolve against can never independently rot out of sync the way two
/// separately-typed `"pm"` string literals could.
/// What: `"pm"` — must have a matching `EmbeddedAgent` entry in
/// [`crate::assets::DEFAULT_AGENTS`] (see `assets/agents/pm.md`).
/// Test: `task::protocol::tests::task_run_creates_session_when_none_given`
/// (exercises the default through a real `task_run` call);
/// `assets::tests::default_task_run_agent_resolves_against_default_agents`
/// (pins that the literal resolves against the embedded roster).
pub const DEFAULT_TASK_RUN_AGENT_NAME: &str = "pm";

/// Register `task.run` onto `router`.
///
/// Why: the one place that wires the method, mirroring
/// `session::protocol::register`'s role for `session.*`.
/// What: closes over `registry`, `binding`, and `agents_dir` — all shared,
/// cheap to clone (`Arc`/`PathBuf`) per call. `binding` replaces what was a
/// REQUIRED `project: PathBuf`: a `PathBuf` cannot express "no project", so the
/// projectless state the shell's entry screen renders was unreachable — the
/// daemon could not be started, let alone run a task, without one. It is now a
/// [`ProjectBinding`], whose `None` variant is that state.
/// Test: `task::protocol::tests::register_wires_task_run`,
/// `task::protocol::tests::register_wires_task_run_projectless`.
/// (Issue #3298) `workstreams` is the daemon's shared workstream store —
/// `task.run` resolves and persists a freshly-minted session's binding
/// (explicit `workstream_id` param, or DOC-48 §4.2's ambient
/// active-workstream default) through it, or enforces §4.1's immutability
/// rule against an EXISTING session's persisted binding — see
/// `task_run`'s docs.
pub fn register(
    router: &mut Router,
    registry: Arc<SessionRegistry>,
    binding: ProjectBinding,
    agents_dir: PathBuf,
    workstreams: SharedWorkstreamStore,
) {
    register_with_permissions(router, registry, binding, agents_dir, workstreams, None);
}

/// [`register`] with the daemon's permission broker (#7948).
///
/// Why: `crate::serve::build_router` owns the one broker
/// `session.permission.respond` answers into; every other caller (tests)
/// keeps [`register`], whose runs are headless.
/// What: `permissions: None` makes every `task.run` headless — an `ask`
/// resolves at once per `TCODE_PERMISSION_MODE`. The mode is read from the
/// daemon's environment, never from the request: the caller asking to run a
/// task must not be able to widen its own permissions.
/// Test: `serve::http_tests::*` (through `build_router`),
/// `permissions::tests::protocol_tests::respond_resolves_a_pending_request`.
pub fn register_with_permissions(
    router: &mut Router,
    registry: Arc<SessionRegistry>,
    binding: ProjectBinding,
    agents_dir: PathBuf,
    workstreams: SharedWorkstreamStore,
    permissions: Option<Arc<crate::permissions::PermissionBroker>>,
) {
    router.register("task.run", move |params: Value, _ctx: ConnectionContext| {
        let registry = Arc::clone(&registry);
        let binding = binding.clone();
        let agents_dir = agents_dir.clone();
        let workstreams = workstreams.clone();
        let permissions = permissions.clone();
        async move {
            task_run_with_permissions(
                registry,
                params,
                binding,
                agents_dir,
                workstreams,
                permissions,
            )
            .await
        }
    });
}

/// `task.run` request params (vision spec §4.3).
#[derive(Deserialize)]
struct TaskRunRequestParams {
    task_description: String,
    #[serde(default)]
    agent_name: Option<String>,
    /// Accepted for forward-compatibility; not yet injected into the
    /// assembled prompt — see the module docs.
    #[serde(default)]
    #[allow(dead_code)]
    context: Option<String>,
    #[serde(default)]
    model_override: Option<String>,
    /// (#8030) Pins the TOP-LEVEL agent's model for this run, the sibling
    /// `model_override` never had — that field only ever rewired the
    /// DELEGATED engineer, so a caller could not repoint the agent that
    /// actually orchestrates.
    ///
    /// Why: the owner rule (2026-09-15) is that every tcode capability is
    /// reachable from CLI/API; before this, the top-level model was reachable
    /// only by hand-editing a deployed `.trusty-code/agents/<agent>.md`.
    /// What: `#[serde(default)]` — omitted means "no override", which is the
    /// pre-#8030 behaviour exactly. A short alias (`opus`/`sonnet`/`haiku`)
    /// is accepted and normalised downstream by
    /// `crate::provider::resolve_model_with_override`.
    /// Test: `task::protocol::tests::task_run_params_parse_pm_model`,
    /// `task::protocol::tests::task_run_params_default_pm_model_to_none`.
    #[serde(default)]
    pm_model: Option<String>,
    /// (#8128) Raises (or lowers) the PM loop's turn cap for this run.
    ///
    /// Why: the cap was `AgentLoopConfig::default()`'s 8 with no override, so
    /// a multi-step delivery task could not be given more turns.
    /// What: `#[serde(default)]` — omitted keeps the built-in 8. Zero is
    /// rejected as `invalid_argument` by [`task_run`]: a zero-turn loop makes
    /// no LLM call and would report an empty run as a normal one.
    /// Test: `task::protocol::tests::task_run_params_parse_max_turns`,
    /// `task::protocol::tests::task_run_rejects_zero_max_turns`.
    #[serde(default)]
    max_turns: Option<u32>,
    /// #2056 extension beyond the spec's literal example: when present,
    /// runs against an EXISTING session (created via `session.create`)
    /// instead of minting a fresh one — the spec's "sessionful" execution
    /// mode.
    #[serde(default)]
    session_id: Option<String>,
    /// #2059: per-call `HarnessMode` override (vision spec §5.9's tier-2
    /// precedence source, above `.claude/settings.json` and below
    /// `TRUSTY_CODE_MODE`). Leniently parsed by
    /// `crate::mode::resolve_mode` — an unrecognised string here degrades
    /// to "this source does not contribute", NOT a request error.
    #[serde(default)]
    mode: Option<String>,
    /// #2207: per-call wall-clock deadline override, in seconds, applied to
    /// BOTH the PM's own loop and the delegated engineer's loop. `None`
    /// falls through to `crate::provider::resolve_deadline_secs`'s env-var
    /// (`TCODE_RUN_DEADLINE_SECONDS`) and default (1800s) tiers.
    #[serde(default)]
    deadline_secs: Option<u64>,
    /// #3178: per-call project override (DOC-39 §5.5, AC-16.2 convergence).
    /// `None` preserves today's back-compat behaviour — the process-boot-time
    /// `binding` `register` closed over. `Some(path)` is resolved through the
    /// SAME [`ProjectBinding::resolve`] `session.create`'s `CreateParams.project`
    /// uses (see `crate::session::protocol::create`'s docs), so a nonexistent
    /// or non-directory path maps to `-32003 invalid_argument` identically on
    /// both surfaces — this is the keystone convergence the issue names:
    /// `task.run` and `session.create` can no longer disagree about what a
    /// project is or how one is resolved.
    ///
    /// **Invariant when paired with `session_id` (reusing an existing
    /// session):** a session's persisted `Session.binding` is authoritative —
    /// `SessionRegistry` has no binding-update path (it is set once, in
    /// `SessionRegistry::create`). `project` may only RESTATE that same root;
    /// naming a DIFFERENT root is rejected with `-32003 invalid_argument`
    /// rather than silently executing the run against a project
    /// `session.status`/`session.list` would never agree it is bound to. See
    /// [`task_run`]'s docs.
    #[serde(default)]
    project: Option<PathBuf>,
    /// (Issue #3298, DOC-48 §4.1/§4.2) Explicit workstream to bind a
    /// FRESHLY-MINTED session to (`session_id` absent), or to RESTATE an
    /// existing reused session's own persisted binding — see
    /// [`task_run`]'s docs on the immutability check this triggers on the
    /// `session_id`-present path. `None` on the mint path falls back to
    /// §4.2's ambient active-workstream default; `None` on the reuse path
    /// never re-applies ambient targeting (a session's binding, once set, is
    /// immutable — see `crate::workstreams::protocol::check_workstream_immutable`'s
    /// docs).
    #[serde(default)]
    workstream_id: Option<String>,
    /// (#8031) Run `agent_name` ALONE — skip `delegate_to_agent` registration
    /// for this run and give `agent_name` its own tcode tools instead, so it
    /// does the work rather than handing it to `python-engineer`.
    ///
    /// Why: the API surface has to express the same run shape the CLI's
    /// `--no-delegate` flag does, or a non-CLI caller (the GUI, an MCP
    /// client) cannot request a guaranteed single-agent run at all.
    /// What: `#[serde(default)]` — an omitted field is `false`, which is
    /// exactly the pre-#8031 behaviour, so no existing caller changes. `true`
    /// registers the same tool set, under the same `tools.allowed` and
    /// `permissions:` gating, a delegation of `agent_name` would get — see
    /// [`crate::task::executor::TaskRunParams::no_delegate`].
    /// Test: `task::protocol::tests::task_run_params_default_no_delegate_to_false`,
    /// `task::protocol::tests::task_run_params_parse_no_delegate_true`.
    #[serde(default)]
    no_delegate: bool,
}

/// `task.run(task_description, agent_name?, context?, model_override?,
/// session_id?, mode?, deadline_secs?, project?, workstream_id?,
/// no_delegate?, pm_model?, max_turns?) -> { session_id, status, mode }`.
///
/// Why: the single entry point that turns a request into a running
/// background execution.
///
/// **(Issue #3298, DOC-48 §4.1/§4.2) Workstream binding**, mirroring the
/// `project`/binding reconciliation immediately below: on the mint path
/// (`session_id` absent), resolves the EFFECTIVE workstream target —
/// `workstream_id` explicit param wins, else the daemon's active workstream
/// is the ambient default (§4.2), else the session stays projectless — and
/// persists it via `crate::workstreams::protocol::resolve_validate_bind`
/// (validated BEFORE the session is minted — see that helper's docs on the
/// PR #3354 phantom-session fix)
/// before this session is used further. On the reuse path (`session_id`
/// present), the session's own persisted `workstream_id` is authoritative;
/// `crate::workstreams::protocol::check_workstream_immutable` rejects a
/// `workstream_id` param that would silently redirect a session
/// `session.status`/`session.list` already reports as bound to a different
/// (or no) workstream — same rule, same rationale as the `project` mismatch
/// guard.
/// What: validates `task_description` is non-empty
/// (`-32003 invalid_argument`); resolves the EFFECTIVE binding for this call —
/// `project: Some(path)` resolves via [`ProjectBinding::resolve`] (mapping a
/// `BindingError` onto `-32003 invalid_argument`, exactly like
/// `session.create`); `project: None` keeps the process-boot-time `binding`
/// `register` closed over, so an existing caller that never sends `project`
/// observes no change (#3178 back-compat). Then resolves the target session —
/// an existing one by `session_id` (propagating `session_not_found` if it
/// doesn't exist) or a freshly `session.create`d one bound to the effective
/// binding; resolves the effective `HarnessMode` via `crate::mode::resolve_mode`
/// (#2059's three-tier precedence, rooted at the effective binding) and
/// persists it onto the session (`SessionRegistry::set_mode`) so it is
/// queryable afterward via `session.status`/`session.list` (`Session.mode`)
/// and `session.get_transcript` (`TranscriptRecord.mode`); builds the shared
/// LLM client (real or the #2056 offline mock, per `TCODE_MOCK_LLM`); and
/// calls `spawn_task_run`, which reserves the execution slot synchronously
/// (rejecting a second overlapping run) before handing off to the background
/// task. Returns immediately — the caller `session.attach`es to observe
/// progress, per the ticket's "must not block the request thread on the whole
/// LLM run" requirement. The response's own `mode`/`binding` fields are the
/// SAME resolved values, surfaced immediately rather than requiring a
/// follow-up `session.status` call.
///
/// **Invariant: a reused session's persisted binding is authoritative.** When
/// `session_id` is supplied, `project` may only RESTATE that session's own
/// `Session.binding` root (or be omitted); a `project` naming a DIFFERENT
/// root is rejected as `-32003 invalid_argument` rather than silently
/// executing the run against a project the session itself was never bound to
/// (`SessionRegistry` has no binding-update path — `create` sets it once).
/// Accepting the mismatch would let `session.status`/`session.list` report
/// the ORIGINAL binding forever while the run actually executed against a
/// different one — a silent audit/state divergence this validation exists to
/// prevent (code-critic HIGH finding, PR #3189).
///
/// **(#8184) Invariant: a reused session's agent SHAPE is authoritative too.**
/// A session minted solo (`session.create`'s default — see
/// `crate::session::protocol::create`) runs solo on every turn, whatever this
/// call's `no_delegate` says; the request can still ASK for the solo shape on
/// a delegating session, so the effective value is the OR of the two. The mint
/// path persists this call's own `no_delegate` onto the session it creates.
/// Test: `task::protocol::tests::task_run_rejects_empty_task_description`,
/// `task::protocol::tests::task_run_creates_session_when_none_given`,
/// `task::protocol::tests::task_run_sessionful_reuses_existing_session`,
/// `task::protocol::tests::task_run_unknown_session_id_errors`,
/// `task::protocol::tests::task_run_resolves_and_reports_mode`,
/// `task::protocol::tests::task_run_without_project_keeps_boot_binding`,
/// `task::protocol::tests::task_run_with_project_overrides_boot_binding`,
/// `task::protocol::tests::task_run_rejects_invalid_project`,
/// `task::protocol::tests::task_run_session_id_with_matching_project_succeeds`,
/// `task::protocol::tests::task_run_session_id_with_mismatched_project_is_rejected`,
/// `task::protocol::tests::task_run_on_a_default_session_never_delegates`,
/// `task::protocol::tests::task_run_on_a_delegating_session_still_delegates`,
/// `task::protocol::tests::task_run_minted_session_records_its_no_delegate`.
// #7948: production registers through `register_with_permissions`; this
// headless five-argument form is what the unit tests drive.
#[cfg(test)]
async fn task_run(
    registry: Arc<SessionRegistry>,
    params: Value,
    binding: ProjectBinding,
    agents_dir: PathBuf,
    workstreams: SharedWorkstreamStore,
) -> Result<Value, RpcError> {
    task_run_with_permissions(registry, params, binding, agents_dir, workstreams, None).await
}

/// [`task_run`] with the daemon's permission broker (#7948); `None` runs
/// headless. Called by [`register_with_permissions`].
/// Test: `task::protocol::tests::*` (through [`task_run`]).
async fn task_run_with_permissions(
    registry: Arc<SessionRegistry>,
    params: Value,
    binding: ProjectBinding,
    agents_dir: PathBuf,
    workstreams: SharedWorkstreamStore,
    permissions: Option<Arc<crate::permissions::PermissionBroker>>,
) -> Result<Value, RpcError> {
    let p: TaskRunRequestParams = serde_json::from_value(params)
        .map_err(|e| RpcError::invalid_params(format!("task.run: {e}")))?;
    if p.task_description.trim().is_empty() {
        return Err(RpcError::invalid_argument(
            "task_description must not be empty",
        ));
    }
    // #8128: a zero turn cap would run no turns at all and report the empty
    // result as a normal one — reject it before a session is ever minted.
    if p.max_turns == Some(0) {
        return Err(RpcError::invalid_argument(
            "max_turns must be at least 1 (0 would run no turns at all)",
        ));
    }
    // #3178: a per-call `project` overrides the boot-time binding for this
    // call only, resolved through the exact same helper `session.create` uses
    // — never a second, divergent implementation of "what is a project".
    let project_given = p.project.is_some();
    let binding = match p.project {
        Some(project) => ProjectBinding::resolve(Some(project))
            .map_err(|e| RpcError::invalid_argument(format!("task.run: {e}")))?,
        None => binding,
    };
    let agent_name = p
        .agent_name
        .unwrap_or_else(|| DEFAULT_TASK_RUN_AGENT_NAME.to_string());

    // #8184: a session's agent shape is the session's, not the call's — a
    // session minted solo (`session.create`'s default, what `tcode tui`
    // creates) must never be handed the delegating registry on a later turn.
    // `false` for the mint arm below: a freshly-minted session records THIS
    // call's `no_delegate`, so the OR below is already satisfied by it.
    let mut session_no_delegate = false;
    let session_id = match &p.session_id {
        Some(id) => {
            let existing = registry.status(id)?; // propagate session_not_found verbatim
            // HIGH finding (code-critic, PR #3189): a session's persisted
            // binding is authoritative once created — reject a per-call
            // `project` that would silently redirect the run to a DIFFERENT
            // root than `session.status`/`session.list` will keep reporting.
            if project_given && existing.binding.root() != binding.root() {
                return Err(RpcError::invalid_argument(format!(
                    "task.run: project `{}` does not match session `{id}`'s existing binding \
                     `{}` — a session's persisted binding is authoritative; project may only \
                     restate it, not change it",
                    binding
                        .label()
                        .unwrap_or_else(|| "<projectless>".to_string()),
                    existing
                        .binding
                        .label()
                        .unwrap_or_else(|| "<projectless>".to_string()),
                )));
            }
            // (Issue #3298, DOC-48 §4.1 AC-1.3) A REUSED session's own
            // persisted `workstream_id` is likewise authoritative — the
            // exact same rule as the `project` check just above, applied to
            // the sibling binding. `None` here never rejects: ambient
            // default-targeting (§4.2) applies only at a session's FIRST
            // binding, never re-applied on reuse.
            crate::workstreams::protocol::check_workstream_immutable(
                existing.workstream_id,
                p.workstream_id.as_deref(),
                "task.run",
            )?;
            // #8184: inherit the reused session's own shape.
            session_no_delegate = existing.no_delegate;
            id.clone()
        }
        None => {
            // (PR #3354 code-critic HIGH 2) Validate the workstream target
            // BEFORE minting: `SessionRegistry` has no delete/rollback, so
            // a rejected bind (malformed/unknown/closed `workstream_id`)
            // must fire while no session exists yet — the mint runs as a
            // closure inside `resolve_validate_bind`, only after validation
            // passes, under the same store lock (TOCTOU-free; see that
            // helper's docs).
            let (session_id, target) = crate::workstreams::protocol::resolve_validate_bind(
                &workstreams,
                p.workstream_id.as_deref(),
                "task.run",
                || {
                    // #8184: record THIS call's shape on the session it mints,
                    // so a later turn against the same session inherits it.
                    registry
                        .create_with_delegation(
                            p.task_description.clone(),
                            Some(agent_name.clone()),
                            binding.clone(),
                            p.no_delegate,
                        )
                        .id
                },
            )
            .await?;
            if let Some(ws_id) = target {
                registry.bind_workstream(&session_id, ws_id)?;
            }
            session_id
        }
    };

    let mode = crate::mode::resolve_mode(p.mode.as_deref(), binding.root());
    registry.set_mode(&session_id, mode)?;

    let llm = build_llm_client()?;
    let task_params = TaskRunParams {
        session_id: session_id.clone(),
        task: p.task_description,
        agent_name,
        binding: binding.clone(),
        agents_dir,
        model_override: p.model_override,
        // #8030/#8128: both carried straight through — the CLI already
        // applied its flag-then-env tiers before the request was built.
        pm_model: p.pm_model,
        max_turns: p.max_turns,
        mode,
        deadline_secs: p.deadline_secs,
        // #3902: no test-only override for this production call site.
        telemetry_data_dir: None,
        // #7948: an `ask` suspends against the daemon's broker when one is
        // registered. The mode comes from the daemon's environment, never the
        // request, so a caller cannot widen its own permissions.
        permission_broker: permissions,
        permission_mode: crate::permissions::PermissionMode::resolve(None),
        // #8031: a single-agent run. #8184 adds the ONE resolution tier this
        // has: a session minted solo stays solo for every turn, so the
        // request can ask for the solo shape but never revoke the session's.
        no_delegate: p.no_delegate || session_no_delegate,
    };
    spawn_task_run(registry, llm, task_params)?;

    // #4351: `result` rides alongside the pre-existing keys, never in place of
    // them. `task.run` returns the moment the run is spawned, so the only
    // truthful result at this instant is `pending` with no refs — the caller
    // polls `session.status` for the terminal one, which the executor fills in.
    Ok(json!({
        "session_id": session_id,
        "status": "running",
        "mode": mode.as_str(),
        "binding": binding.to_json(),
        "result": crate::session::TaskResult::pending(),
    }))
}

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;
