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

use super::executor::{TaskRunParams, spawn_task_run};
use super::mock_llm::build_llm_client;

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
pub fn register(
    router: &mut Router,
    registry: Arc<SessionRegistry>,
    binding: ProjectBinding,
    agents_dir: PathBuf,
) {
    router.register("task.run", move |params: Value, _ctx: ConnectionContext| {
        let registry = Arc::clone(&registry);
        let binding = binding.clone();
        let agents_dir = agents_dir.clone();
        async move { task_run(registry, params, binding, agents_dir).await }
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
}

/// `task.run(task_description, agent_name?, context?, model_override?,
/// session_id?, mode?, deadline_secs?, project?) -> { session_id, status, mode }`.
///
/// Why: the single entry point that turns a request into a running
/// background execution.
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
/// Test: `task::protocol::tests::task_run_rejects_empty_task_description`,
/// `task::protocol::tests::task_run_creates_session_when_none_given`,
/// `task::protocol::tests::task_run_sessionful_reuses_existing_session`,
/// `task::protocol::tests::task_run_unknown_session_id_errors`,
/// `task::protocol::tests::task_run_resolves_and_reports_mode`,
/// `task::protocol::tests::task_run_without_project_keeps_boot_binding`,
/// `task::protocol::tests::task_run_with_project_overrides_boot_binding`,
/// `task::protocol::tests::task_run_rejects_invalid_project`,
/// `task::protocol::tests::task_run_session_id_with_matching_project_succeeds`,
/// `task::protocol::tests::task_run_session_id_with_mismatched_project_is_rejected`.
async fn task_run(
    registry: Arc<SessionRegistry>,
    params: Value,
    binding: ProjectBinding,
    agents_dir: PathBuf,
) -> Result<Value, RpcError> {
    let p: TaskRunRequestParams = serde_json::from_value(params)
        .map_err(|e| RpcError::invalid_params(format!("task.run: {e}")))?;
    if p.task_description.trim().is_empty() {
        return Err(RpcError::invalid_argument(
            "task_description must not be empty",
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
    let agent_name = p.agent_name.unwrap_or_else(|| "pm".to_string());

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
            id.clone()
        }
        None => {
            let session = registry.create(
                p.task_description.clone(),
                Some(agent_name.clone()),
                binding.clone(),
            );
            session.id
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
        mode,
        deadline_secs: p.deadline_secs,
    };
    spawn_task_run(registry, llm, task_params)?;

    Ok(json!({
        "session_id": session_id,
        "status": "running",
        "mode": mode.as_str(),
        "binding": binding.to_json(),
    }))
}

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;
