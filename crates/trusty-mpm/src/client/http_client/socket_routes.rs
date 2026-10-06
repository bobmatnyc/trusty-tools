//! HTTP route → daemon-socket JSON-RPC method, for [`super::transport`]
//! (#6288 step 1).
//!
//! Why: every `DaemonClient` method and CLI call site names an HTTP route. The
//! daemon serves the same bodies as JSON-RPC methods (slices 2-5), whose
//! `params` object is the HTTP body plus the path segments and query fields as
//! named keys. One table here is what lets a call written against a route run
//! over the socket without each call site learning a method name.
//! What: [`ROUTES`] pairs a verb and a path pattern with a method; a `{name}`
//! segment captures into `params.name`, so each placeholder is spelled as the
//! method's params field. [`resolve`] matches in table order (literal routes
//! sit before their `{id}` siblings). [`status_for_code`] is the inverse of the
//! daemon's `rpc_code_for_status`. A route absent from the table is refused by
//! the transport — never sent over TCP instead.
//! Test: `every_route_names_a_served_method` (`daemon/socket_tests.rs`) pins
//! the table against the daemon's router; `resolve_maps_routes_onto_methods`.

use reqwest::{Method, StatusCode};
use serde_json::{Map, Value};

/// JSON-RPC "method not found": the daemon predates the method.
pub(crate) const CODE_METHOD_NOT_FOUND: i64 = -32601;

/// `(verb, path pattern, JSON-RPC method)`.
pub(crate) const ROUTES: &[(&str, &str, &str)] = &[
    // ---- core (slice 2) ----
    ("GET", "/health", "mpm.health"),
    ("GET", "/api/v1/doctor", "mpm.doctor"),
    ("GET", "/api/v1/errors", "mpm.errors.list"),
    ("POST", "/api/v1/report-bug", "mpm.report_bug"),
    ("GET", "/breakers", "mpm.breakers"),
    ("GET", "/optimizer", "mpm.optimizer"),
    ("GET", "/overseer", "mpm.overseer"),
    ("POST", "/llm/chat", "mpm.llm.chat"),
    ("GET", "/tmux/sessions", "mpm.tmux.sessions"),
    ("GET", "/tmux/sessions/{name}/snapshot", "mpm.tmux.snapshot"),
    ("POST", "/tmux/adopt", "mpm.tmux.adopt"),
    ("GET", "/claude-config", "mpm.claude_config.get"),
    ("POST", "/claude-config/apply", "mpm.claude_config.apply"),
    (
        "GET",
        "/claude-config/checkpoints",
        "mpm.claude_config.checkpoints.list",
    ),
    (
        "POST",
        "/claude-config/checkpoints",
        "mpm.claude_config.checkpoints.create",
    ),
    (
        "DELETE",
        "/claude-config/checkpoints/{id}",
        "mpm.claude_config.checkpoints.delete",
    ),
    (
        "POST",
        "/claude-config/restore",
        "mpm.claude_config.restore",
    ),
    (
        "GET",
        "/claude-config/profiles",
        "mpm.claude_config.profiles",
    ),
    ("POST", "/claude-config/deploy", "mpm.claude_config.deploy"),
    (
        "POST",
        "/claude-config/restart",
        "mpm.claude_config.restart",
    ),
    // ---- legacy sessions, hooks, polled feeds (slice 3) ----
    ("GET", "/sessions", "mpm.sessions.list"),
    ("POST", "/sessions", "mpm.sessions.register"),
    ("POST", "/api/v1/sessions/connect", "mpm.sessions.connect"),
    ("POST", "/sessions/discover", "mpm.sessions.discover"),
    ("DELETE", "/sessions/dead", "mpm.sessions.reap"),
    ("GET", "/sessions/{id}", "mpm.sessions.get"),
    ("DELETE", "/sessions/{id}", "mpm.sessions.delete"),
    ("POST", "/sessions/{id}/pause", "mpm.sessions.pause"),
    ("POST", "/sessions/{id}/resume", "mpm.sessions.resume"),
    ("POST", "/sessions/{id}/command", "mpm.sessions.command"),
    ("GET", "/sessions/{id}/output", "mpm.sessions.output"),
    ("GET", "/sessions/{id}/pane", "mpm.sessions.pane"),
    ("PATCH", "/sessions/{id}/pid", "mpm.sessions.set_pid"),
    (
        "GET",
        "/sessions/{id}/events/poll",
        "mpm.sessions.events_poll",
    ),
    ("GET", "/events/poll", "mpm.events.poll"),
    ("POST", "/hooks", "mpm.hooks.ingest"),
    // ---- managed sessions (slice 4), fleet-wide first ----
    ("POST", "/api/v1/sessions/managed", "mpm.managed.spawn"),
    ("GET", "/api/v1/sessions/managed", "mpm.managed.list"),
    (
        "POST",
        "/api/v1/sessions/managed/adopt",
        "mpm.managed.adopt",
    ),
    (
        "POST",
        "/api/v1/sessions/managed/adopt-worktree",
        "mpm.managed.adopt_worktree",
    ),
    (
        "POST",
        "/api/v1/sessions/managed/prune",
        "mpm.managed.prune",
    ),
    (
        "POST",
        "/api/v1/sessions/managed/decommission-ephemeral",
        "mpm.managed.decommission_ephemeral",
    ),
    (
        "POST",
        "/api/v1/sessions/managed/prune-worktrees",
        "mpm.managed.prune_worktrees",
    ),
    (
        "GET",
        "/api/v1/sessions/managed/reconcile-worktrees",
        "mpm.managed.reconcile_worktrees",
    ),
    ("GET", "/api/v1/sessions/managed/fleet", "mpm.managed.fleet"),
    (
        "POST",
        "/api/v1/sessions/managed/supervisor",
        "mpm.managed.register_supervisor",
    ),
    (
        "POST",
        "/api/v1/sessions/managed/sync-assets",
        "mpm.managed.sync_assets_all",
    ),
    ("GET", "/api/v1/sessions/managed/{id}", "mpm.managed.get"),
    (
        "DELETE",
        "/api/v1/sessions/managed/{id}",
        "mpm.managed.stop",
    ),
    (
        "PATCH",
        "/api/v1/sessions/managed/{id}",
        "mpm.managed.rename",
    ),
    (
        "POST",
        "/api/v1/sessions/managed/{id}/send",
        "mpm.managed.send",
    ),
    (
        "GET",
        "/api/v1/sessions/managed/{id}/provision-status",
        "mpm.managed.provision_status",
    ),
    (
        "POST",
        "/api/v1/sessions/managed/{id}/sync-assets",
        "mpm.managed.sync_assets",
    ),
    (
        "POST",
        "/api/v1/sessions/managed/{id}/answer",
        "mpm.managed.answer",
    ),
    (
        "GET",
        "/api/v1/sessions/managed/{id}/attach-cmd",
        "mpm.managed.attach_cmd",
    ),
    (
        "GET",
        "/api/v1/sessions/managed/{id}/activity",
        "mpm.managed.activity",
    ),
    (
        "POST",
        "/api/v1/sessions/managed/{id}/runtime-stop",
        "mpm.managed.runtime_stop",
    ),
    (
        "POST",
        "/api/v1/sessions/managed/{id}/resume",
        "mpm.managed.resume",
    ),
    (
        "POST",
        "/api/v1/sessions/managed/{id}/reactivate",
        "mpm.managed.reactivate",
    ),
    (
        "POST",
        "/api/v1/sessions/managed/{id}/decommission",
        "mpm.managed.decommission",
    ),
    (
        "POST",
        "/api/v1/sessions/managed/{id}/delete",
        "mpm.managed.delete",
    ),
    // #9313: `tm sessions rebind`.
    (
        "POST",
        "/api/v1/sessions/managed/{id}/rebind",
        "mpm.managed.rebind",
    ),
    (
        "POST",
        "/api/v1/sessions/managed/rebind",
        "mpm.managed.rebind_all",
    ),
    // ---- registry, deliverables, manager, pairing, delegation (slice 5) ----
    ("GET", "/projects", "mpm.projects.list"),
    ("POST", "/projects", "mpm.projects.register"),
    ("GET", "/projects/current", "mpm.projects.current"),
    ("GET", "/projects/discover", "mpm.projects.discover"),
    ("GET", "/api/v1/projects", "mpm.projects.registry.list"),
    ("POST", "/api/v1/projects", "mpm.projects.registry.register"),
    (
        "GET",
        "/api/v1/projects/{name}",
        "mpm.projects.registry.get",
    ),
    (
        "PATCH",
        "/api/v1/projects/{name}",
        "mpm.projects.registry.patch",
    ),
    (
        "GET",
        "/api/v1/projects/{name}/status",
        "mpm.projects.status",
    ),
    (
        "POST",
        "/api/v1/projects/{project}/deliverables",
        "mpm.deliverables.create",
    ),
    (
        "GET",
        "/api/v1/projects/{project}/deliverables",
        "mpm.deliverables.list",
    ),
    (
        "GET",
        "/api/v1/projects/{project}/deliverables/{id}",
        "mpm.deliverables.get",
    ),
    (
        "PATCH",
        "/api/v1/projects/{project}/deliverables/{id}",
        "mpm.deliverables.patch",
    ),
    (
        "POST",
        "/api/v1/projects/{project}/milestones",
        "mpm.milestones.create",
    ),
    (
        "GET",
        "/api/v1/projects/{project}/milestones",
        "mpm.milestones.list",
    ),
    (
        "GET",
        "/api/v1/projects/{project}/milestones/{id}",
        "mpm.milestones.get",
    ),
    (
        "PATCH",
        "/api/v1/projects/{project}/milestones/{id}",
        "mpm.milestones.patch",
    ),
    ("GET", "/api/v1/manager/version", "mpm.manager.version"),
    ("GET", "/api/v1/manager/status", "mpm.manager.status"),
    ("GET", "/api/v1/manager/digest", "mpm.manager.digest"),
    ("POST", "/api/v1/manager/chat", "mpm.manager.chat"),
    (
        "POST",
        "/api/v1/manager/route-task",
        "mpm.manager.route_task",
    ),
    ("POST", "/api/v1/manager/act", "mpm.manager.act"),
    ("POST", "/pair/request", "mpm.pair.request"),
    ("POST", "/pair/confirm", "mpm.pair.confirm"),
    ("GET", "/pair/status", "mpm.pair.status"),
    ("POST", "/pair/reset", "mpm.pair.reset"),
    (
        "POST",
        "/api/v1/sessions/{id}/delegations/shared-tree-dispatch",
        "mpm.delegation.shared_tree_dispatch",
    ),
    (
        "POST",
        "/api/v1/sessions/{id}/delegations/granted-worktree",
        "mpm.delegation.granted_worktree",
    ),
    // ---- #6288 step 1: build lease and the retired builder-slot routes ----
    (
        "POST",
        "/api/v1/build-lease/decisions",
        "mpm.build_lease.decision",
    ),
    (
        "POST",
        "/api/v1/sessions/{id}/delegations/builder-slot",
        "mpm.builder_slot.claim",
    ),
    ("GET", "/api/v1/builder-slots", "mpm.builder_slot.list"),
    // ---- #6288 step 2a: rows for methods slices 4-5 served without one, and
    // for the routes step 2a moved across. `mpm.control.connect` has no row:
    // it is a stream, and `send` reads one frame. ----
    ("GET", "/api/v1/control/sessions", "mpm.control.list"),
    ("POST", "/api/v1/control/sessions/run", "mpm.control.run"),
    (
        "POST",
        "/api/v1/control/sessions/{id}/stop",
        "mpm.control.stop",
    ),
    (
        "GET",
        "/api/v1/control/sessions/{id}/auth",
        "mpm.control.auth",
    ),
    ("POST", "/api/v1/sessions/proxy/focus", "mpm.proxy.focus"),
    (
        "GET",
        "/api/v1/sessions/proxy/focus/{conversation_key}",
        "mpm.proxy.get_focus",
    ),
    (
        "POST",
        "/api/v1/sessions/proxy/unfocus",
        "mpm.proxy.unfocus",
    ),
    (
        "POST",
        "/api/v1/sessions/proxy/message",
        "mpm.proxy.message",
    ),
    (
        "GET",
        "/api/v1/sessions/proxy/summary/{conversation_key}",
        "mpm.proxy.summary",
    ),
    ("GET", "/api/v1/delegations", "mpm.delegation.list"),
    (
        "POST",
        "/api/v1/delegations/by-id/{delegation_id}/repair",
        "mpm.delegation.repair_by_id",
    ),
    (
        "POST",
        "/api/v1/delegations/{agent_id}/repair",
        "mpm.delegation.repair",
    ),
    ("GET", "/api/v1/sessions/context", "mpm.sessions.context"),
    (
        "GET",
        "/api/v1/session-manager/context",
        "mpm.sessions.context",
    ),
    ("POST", "/api/v1/sessions/chat", "mpm.sessions.chat"),
    ("POST", "/api/v1/session-manager/chat", "mpm.sessions.chat"),
    ("POST", "/rpc", "mpm.mcp.dispatch"),
];

/// The method and path captures for `method path`, or `None` when the socket
/// serves no such route. A query string on `path` is ignored for matching.
pub(crate) fn resolve(method: &Method, path: &str) -> Option<(&'static str, Map<String, Value>)> {
    let path = path.split('?').next().unwrap_or(path);
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    ROUTES.iter().find_map(|(verb, pattern, rpc)| {
        if *verb != method.as_str() {
            return None;
        }
        let want: Vec<&str> = pattern.trim_matches('/').split('/').collect();
        if want.len() != segments.len() {
            return None;
        }
        let mut captures = Map::new();
        for (w, got) in want.iter().zip(&segments) {
            match w.strip_prefix('{').and_then(|w| w.strip_suffix('}')) {
                Some(name) if !got.is_empty() => {
                    captures.insert(name.to_string(), Value::String(decode(got)));
                }
                Some(_) => return None,
                None if w == got => {}
                None => return None,
            }
        }
        Some((*rpc, captures))
    })
}

/// Undo percent-encoding a caller applied to a path segment.
fn decode(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The `x-trusty-resume-reason` value a resume refusal's code stands for.
pub(crate) fn resume_reason_for_code(code: i64) -> Option<&'static str> {
    match code {
        -32023 => Some("workspace_missing"),
        -32024 => Some("pane_gone"),
        _ => None,
    }
}

/// The HTTP status a daemon RPC error code stands for.
///
/// The inverse of `daemon::error::rpc_code_for_status`, plus the three codes
/// only some routes use (410, and the two 422 resume refusals). Anything else
/// is a 500, as the daemon maps an unlisted status to `CODE_INTERNAL_ERROR`.
/// Test: `status_for_code_inverts_the_daemon_table`.
pub(crate) fn status_for_code(code: i64) -> StatusCode {
    match code {
        -32602 => StatusCode::BAD_REQUEST,
        -32003 => StatusCode::FORBIDDEN,
        -32004 => StatusCode::NOT_FOUND,
        -32008 => StatusCode::GONE,
        -32009 => StatusCode::CONFLICT,
        -32006 | -32023 | -32024 => StatusCode::UNPROCESSABLE_ENTITY,
        -32007 => StatusCode::BAD_GATEWAY,
        -32002 => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
