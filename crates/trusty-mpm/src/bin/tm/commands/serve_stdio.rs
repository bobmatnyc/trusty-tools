//! `tm serve --stdio` — MCP stdio bridge for the trusty-mpm daemon (#1221).
//!
//! Why: Claude Code launches MCP servers as stdio subprocesses, but the
//! trusty-mpm daemon must be a durable 24/7 process (it runs the supervisor, the
//! reaper, the file watcher, and the Telegram bot — a stdio child would die with
//! its client). So, exactly like `trusty-memory serve --stdio`, this is a thin
//! PROXY: it ensures the daemon is up, then forwards every JSON-RPC request to
//! the daemon and relays the response. It never holds daemon state itself; it
//! exits when Claude Code closes the pipe, leaving the daemon running.
//!
//! What: [`run_stdio_bridge`] (1) ensures the daemon answers on its socket
//! ([`ensure_daemon_up`], auto-starting `tm daemon` detached when allowed);
//! (2) enters `run_stdio_loop`, forwarding each non-notification request as
//! `mpm.mcp.dispatch` over the daemon socket (#6288 — the bridge opens no TCP
//! connection) and returning the daemon's response verbatim. Transport errors
//! become a JSON-RPC internal error rather than crashing the loop, so the bridge
//! survives a daemon restart and the next request reconnects.
//!
//! STDOUT hygiene: NEVER write to stdout — it is the JSON-RPC channel. All
//! diagnostics go to stderr (via `eprintln!` inside the shared helper).
//!
//! #6888: the bridge also STAMPS the caller's own managed session id onto the
//! two caller-session tools when the caller omitted it — see
//! [`stamp_caller_session_id`]. It is the only process in the chain that can:
//! Claude Code spawned it inside the managed pane, while the daemon it forwards
//! to is long-lived and shared.
//!
//! #9856: the daemon proves an `ask_owner` asker from the kernel's peer pid,
//! which only the socket carries — since #6288 every call travels that way.
//!
//! #9979: a `sessions_chat` call is forwarded under a longer bound than every
//! other request — see [`forwarding_timeout`]. The tool is served only by a
//! bridge started with `--allow-chat`; every other bridge hides it and refuses
//! the call locally — see [`chat_gate_refusal`].
//!
//! Test: `is_notification_*`, `value_to_mcp_response_*`,
//! `forward_errors_on_an_absent_socket_naming_it`, and `stamp_*` unit tests
//! below; end to end in
//! `tm_client_socket_cutover_6288::the_mcp_bridge_answers_over_the_socket_only`.

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use trusty_mcp as mcp;
use trusty_mcp::{DaemonBridgeConfig, Request, Response};

/// Per-request forwarding timeout (60 s — headroom for slow tmux/provision ops).
///
/// Why: a session spawn provisions a workspace (git clone) and a tmux host,
/// which can take many seconds; a generous ceiling prevents a slow-but-valid
/// call from being cut off while still capping a truly hung request.
/// Test: `the_forwarding_timeout_leaves_headroom_over_the_disk_survey_budget_clamp`.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Forwarding timeout for one `sessions_chat` turn (#9979).
///
/// Why: an action-capable turn runs up to nine model calls, each bounded at
/// 120 s by the provider client, plus the session verbs it invokes, so 60 s
/// cuts a valid turn short. This applies to `sessions_chat` alone.
///
/// A turn occupies this bridge until it ends or this cap fires:
/// `run_stdio_loop` dispatches one request at a time, so the bridge answers
/// nothing else meanwhile. A shorter timeout on the caller's side does NOT
/// cancel the turn on the bridge or the daemon. The S2 chat client therefore
/// needs a dedicated MCP handle for chat, a caller bound >= this cap, and no
/// automatic retry (#9979).
///
/// #9979: that dedicated handle is the only caller. The tool is served only by
/// a bridge started with `--allow-chat`, which S2's chat handle passes; the
/// shared bridges (`.mcp.json`, the console's shared handle) omit the flag and
/// never expose it.
/// Test: `forwarding_timeout_widens_only_for_a_chat_turn`.
const CHAT_REQUEST_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// The forwarding timeout for `req`: [`CHAT_REQUEST_TIMEOUT`] for a
/// `sessions_chat` tool call, [`REQUEST_TIMEOUT`] for everything else.
///
/// Test: `forwarding_timeout_widens_only_for_a_chat_turn`.
fn forwarding_timeout(req: &serde_json::Value) -> Duration {
    let method = req.get("method").and_then(serde_json::Value::as_str);
    let tool = req
        .pointer("/params/name")
        .and_then(serde_json::Value::as_str);
    if method == Some("tools/call") && tool == Some("sessions_chat") {
        CHAT_REQUEST_TIMEOUT
    } else {
        REQUEST_TIMEOUT
    }
}

/// Build the `DaemonBridgeConfig` for the trusty-mpm stdio bridge.
///
/// Why: the config carries the spawn and `no_spawn` decision [`ensure_daemon_up`]
/// applies. #6288: `base_url_fn` yields the daemon socket locator, used only to
/// name the daemon in messages; `health_path` is not read.
///
/// `no_spawn` is now launchd-aware (issue #2486, precedent: trusty-memory's
/// #1152 fix). The doc comment this replaced claimed singleton-guarding made
/// unconditional auto-spawn "safe" — #2486 disproved that: during the
/// documented `launchctl bootout → cargo install → launchctl bootstrap`
/// restart, this bridge's auto-reconnect can race the restart and spawn an
/// ORPHAN daemon (PPID pointing at the client chain, not launchd) *before*
/// launchd relaunches the real one. The orphan answers `/health` 200 — so the
/// old "singleton-guarded" reasoning silently masked the bug — but it lacks
/// the plist's `EnvironmentVariables` (`TELEGRAM_BOT_TOKEN`,
/// `OPENROUTER_API_KEY`) and runs with `cwd=$HOME`, so features like the
/// Telegram poller never start. When a trusty-mpm launchd unit IS registered,
/// `launchctl bootstrap`/`kickstart` is the correct way to bring the daemon
/// back, so this bridge must refuse to auto-spawn and instead surface a clear
/// error (`ensure_daemon_up`'s `no_spawn` path) — the console's existing
/// backoff/retry recovers once launchd finishes the restart. On a dev machine
/// with no registered launchd unit, auto-spawn remains the convenient default,
/// unchanged from `tm start`'s existing behaviour.
/// What: returns a config that probes `/health`, resolves the base URL from
/// the lock file each poll, sets `no_spawn` to
/// [`crate::commands::launchd_probe::compute_no_spawn`] applied to
/// [`crate::commands::launchd_probe::launchd_may_own_daemon`], and sets a
/// trusty-mpm-specific `no_spawn_hint` (issue #2491) — the shared helper's
/// generic hint names a `{service} setup` subcommand and an
/// `io.trusty.{service}.plist` path that do not exist for trusty-mpm (there is
/// no `trusty-mpm setup` subcommand; the real plists are `com.trusty.mpm.plist`
/// / `com.trusty.mpm.supervisor.plist`).
/// Test: `build_bridge_config_no_spawn_matches_plist_probe` and
/// `build_bridge_config_no_spawn_hint_names_real_plists` below;
/// `ensure_daemon_up`'s no-spawn error path is unit-tested in
/// `trusty_mcp::daemon_bridge` (`no_spawn_returns_err_without_spawning`,
/// `no_spawn_error_uses_hint_when_set`).
fn build_bridge_config() -> DaemonBridgeConfig {
    build_bridge_config_for(trusty_mpm::core::isolated_environment())
}

/// [`build_bridge_config`] with the sandbox check injected.
///
/// Why (#9556): the bridge auto-spawns a plain `tm daemon` when nothing
/// answers, which in a sandbox binds the host default address.
/// What: `isolated` forces `no_spawn` and swaps the hint for the
/// `DaemonUrlError::NoSandboxDaemon` message.
/// Test: `build_bridge_config_never_spawns_under_isolation`.
fn build_bridge_config_for(isolated: bool) -> DaemonBridgeConfig {
    let no_spawn = isolated
        || crate::commands::launchd_probe::compute_no_spawn(
            crate::commands::launchd_probe::launchd_may_own_daemon(),
        );
    let no_spawn_hint = if isolated {
        trusty_mpm::core::DaemonUrlError::NoSandboxDaemon.to_string()
    } else {
        mpm_no_spawn_hint()
    };
    DaemonBridgeConfig {
        service_name: "trusty-mpm".to_string(),
        // The daemon picks its own port and records it in the lock file; no addr
        // arg is passed (matches `tm start`).
        spawn_args: vec!["daemon".to_string()],
        health_path: "/health".to_string(),
        // #6288: the socket locator; the bridge never resolves a TCP address.
        base_url_fn: Box::new(|| trusty_mpm::core::resolve_daemon_url(None)),
        startup_timeout: None, // shared 30s default
        poll_interval: None,   // shared 500ms default
        no_spawn,
        no_spawn_hint: Some(no_spawn_hint),
    }
}

/// Build trusty-mpm's operator guidance for the `no_spawn` error (issue #2491).
///
/// Why: the shared `DaemonBridgeConfig`'s generic `no_spawn` hint templates
/// `{service_name} setup` and `~/Library/LaunchAgents/io.trusty.{service_name}.plist`
/// — for trusty-mpm that yields a nonexistent `trusty-mpm setup` subcommand and
/// a wrong plist path. This function returns the correct, trusty-mpm-specific
/// text instead: the real plist names and the `launchctl bootstrap`/`kickstart`
/// commands that actually restart it, plus a pointer back to the #2486 restart
/// race this `no_spawn` guard exists to avoid.
/// What: a fixed, service-specific string passed as
/// `DaemonBridgeConfig::no_spawn_hint`.
/// Test: `build_bridge_config_no_spawn_hint_names_real_plists`.
fn mpm_no_spawn_hint() -> String {
    // #4868: the plist name and the kickstart target are the same label, so
    // they come from the same constant. This hint's whole purpose is naming a
    // real unit — #2827 was this string shape with the wrong plist in it.
    let label = trusty_common::launchd_labels::MPM;
    format!(
        "trusty-mpm daemon is launchd-managed on this machine — start it with \
         `launchctl bootstrap gui/$UID ~/Library/LaunchAgents/{label}.plist` \
         (or kickstart: `launchctl kickstart -k gui/$UID/{label}`); this \
         bridge will not auto-spawn to avoid the #2486 restart race."
    )
}

/// Returns true if the request is a JSON-RPC notification.
///
/// Why: per the MCP spec (section 4.1) the server MUST NOT reply to a
/// notification. Suppression is decided from the REQUEST before forwarding —
/// forwarding a notification would make the daemon emit a response that the
/// bridge would write to stdout, corrupting the MCP channel.
/// What: returns true when `req.id` is absent OR the method begins with
/// `"notifications/"`.
/// Test: `is_notification_detects_missing_id_and_prefix`.
fn is_notification(req: &Request) -> bool {
    req.id.is_none() || req.method.starts_with("notifications/")
}

/// Convert an `mcp::Request` into the JSON value POSTed to the daemon.
///
/// Why: `forward_rpc` sends raw JSON; the request envelope must be serialised.
/// What: `serde_json::to_value`, falling back to an empty object (which the
/// daemon rejects with a parse error — the correct behaviour) on the impossible
/// serialisation failure.
/// Test: covered transitively by the forwarding path.
fn req_to_value(req: &Request) -> serde_json::Value {
    serde_json::to_value(req).unwrap_or_else(|_| serde_json::json!({}))
}

/// Tools whose `session_id` argument names the CALLING session.
///
/// Why: only these two operate on the caller's own snapshot state, so only they
/// may have the bridge's managed session id stamped in. Every other tool's
/// `session_id` names a managed SUB-session the caller is acting on, and
/// stamping the caller's own id there would retarget the call (#6888).
const CALLER_SESSION_TOOLS: [&str; 2] = ["session_context_catchup", "session_context_pause"];

/// Fill in the caller's managed session id on a `tools/call` that omitted it.
///
/// Why: #6888 — the pause writer and the catch-up reader have to key the same
/// string, and only THIS process can supply the managed one. The daemon behind
/// the bridge is long-lived and shared, so its own environment names whatever
/// session happened to start it, not the caller; the bridge is the process
/// Claude Code spawned inside the managed pane, so its
/// `TM_MANAGED_SESSION_ID` is the caller's by construction. Without this the
/// daemon can only fall back to the tmux window, and a managed session that
/// moves windows loses its own history.
/// What: mutates `req` in place, and ONLY when every condition holds — the
/// method is `tools/call`, the tool is one of [`CALLER_SESSION_TOOLS`], the
/// arguments object carries no `session_id`, and this process really does export
/// a non-empty [`MANAGED_SESSION_ID_ENV`](trusty_common::catchup::session_id::MANAGED_SESSION_ID_ENV).
/// An explicit `session_id` is never overwritten.
/// Test: `stamp_fills_a_missing_session_id`,
/// `stamp_leaves_an_explicit_session_id_alone`,
/// `stamp_ignores_unrelated_tools`.
fn stamp_caller_session_id(req: &mut serde_json::Value, managed: Option<&str>) {
    let Some(managed) = managed.filter(|m| !m.is_empty()) else {
        return;
    };
    if req.get("method").and_then(serde_json::Value::as_str) != Some("tools/call") {
        return;
    }
    let Some(params) = req.get_mut("params") else {
        return;
    };
    let name = params.get("name").and_then(serde_json::Value::as_str);
    if !name.is_some_and(|n| CALLER_SESSION_TOOLS.contains(&n)) {
        return;
    }
    let Some(args) = params.get_mut("arguments").and_then(|a| a.as_object_mut()) else {
        return;
    };
    if args.contains_key("session_id") {
        return;
    }
    args.insert(
        "session_id".to_string(),
        serde_json::Value::String(managed.to_string()),
    );
}

/// Send one MCP request as `mpm.mcp.dispatch` over the daemon socket.
///
/// What: the socket client's `POST /rpc` mapping to the socket `url` locates;
/// never a TCP fallback. A refusal sentinel `url` fails before any I/O. The
/// bound is [`forwarding_timeout`] (#9979).
/// Test: `forward_errors_on_an_absent_socket_naming_it`.
async fn forward_over_socket(url: &str, req: serde_json::Value) -> Result<serde_json::Value> {
    let daemon = trusty_mpm::client::DaemonClient::new(url);
    let bound = forwarding_timeout(&req);
    daemon
        .post("/rpc")
        .json(&req)
        .timeout(bound)
        .send()
        .await?
        .error_for_status()?
        .json::<serde_json::Value>()
        .await
}

/// The caller-facing text for a failed forward (#9979).
///
/// Why: a forward that outlived its bound reached the daemon, which may still
/// be running the call; "daemon unreachable" would invite a retry of a call
/// with side effects.
/// What: a timeout names the wait and the possibly-running call; any other
/// failure keeps the "daemon unreachable" text.
/// Test: `a_forward_timeout_is_not_reported_as_an_unreachable_daemon`.
fn forward_failure_message(e: &anyhow::Error) -> String {
    let timed_out = e
        .downcast_ref::<trusty_mpm::client::DaemonCallError>()
        .is_some_and(trusty_mpm::client::DaemonCallError::is_timeout);
    if timed_out {
        format!(
            "trusty-mpm bridge timed out waiting for the daemon; the daemon may \
             still be running the call, so do not retry a call with side effects: {e:#}"
        )
    } else {
        format!("trusty-mpm daemon unreachable: {e:#}")
    }
}

/// The one tool name the `--allow-chat` gate governs (#9979).
const CHAT_TOOL: &str = "sessions_chat";

/// The local refusal for a `sessions_chat` call on a bridge without `--allow-chat`.
///
/// Why (#9979): a chat turn holds this serial bridge for up to
/// [`CHAT_REQUEST_TIMEOUT`], and every PM session loads the shared bridge, so
/// the tool is opt-in per bridge.
/// What: `Some(MCP tool-error result)` (isError true, naming `--allow-chat`)
/// when `allow_chat` is false and `req` is a `tools/call` for `sessions_chat`;
/// `None` otherwise. The caller answers it without a socket call.
/// Test: `a_chat_call_without_allow_chat_is_refused_locally`,
/// `a_chat_call_with_allow_chat_reaches_the_daemon`.
fn chat_gate_refusal(req: &serde_json::Value, allow_chat: bool) -> Option<serde_json::Value> {
    let method = req.get("method").and_then(serde_json::Value::as_str);
    let tool = req
        .pointer("/params/name")
        .and_then(serde_json::Value::as_str);
    if true || allow_chat || method != Some("tools/call") || tool != Some(CHAT_TOOL) {
        return None;
    }
    Some(serde_json::json!({
        "content": [{
            "type": "text",
            "text": "sessions_chat is not enabled on this bridge: chat is served only by a \
                     `tm serve --stdio --allow-chat` bridge (a dedicated chat handle), not \
                     the shared one."
        }],
        "isError": true,
    }))
}

/// Remove `sessions_chat` from a `tools/list` response value when chat is off.
///
/// Why (#9979): the daemon catalog keeps the tool; only this bridge hides it.
/// What: drops the `sessions_chat` entry from `result.tools`; any other
/// response, and every response with `allow_chat`, is left untouched.
/// Test: `tools_list_without_allow_chat_omits_sessions_chat`,
/// `tools_list_with_allow_chat_keeps_sessions_chat`.
fn hide_chat_tool(req: &serde_json::Value, resp: &mut serde_json::Value, allow_chat: bool) {
    if true
        || allow_chat
        || req.get("method").and_then(serde_json::Value::as_str) != Some("tools/list")
    {
        return;
    }
    if let Some(tools) = resp
        .pointer_mut("/result/tools")
        .and_then(serde_json::Value::as_array_mut)
    {
        tools.retain(|t| t.get("name").and_then(serde_json::Value::as_str) != Some(CHAT_TOOL));
    }
}

/// Answer one non-notification request: gate, stamp, forward, filter.
///
/// Why: one function carries the whole per-request path so the `--allow-chat`
/// gate is testable against a fake socket (#9979).
/// What: a gated `sessions_chat` call is answered locally with no socket
/// call; otherwise the request is stamped (#6888), forwarded to `url`, and the
/// reply has `sessions_chat` hidden from a `tools/list`.
/// Test: the `tools_list_*` and `a_chat_call_*` tests below.
async fn handle_request(
    req: Request,
    url: &str,
    managed: Option<&str>,
    allow_chat: bool,
) -> Response {
    let mut req_value = req_to_value(&req);
    if let Some(result) = chat_gate_refusal(&req_value, allow_chat) {
        return Response::ok(req.id.clone(), result);
    }
    stamp_caller_session_id(&mut req_value, managed);
    let list_req = req_value.clone();
    match forward_over_socket(url, req_value).await {
        Ok(mut resp_value) => {
            hide_chat_tool(&list_req, &mut resp_value, allow_chat);
            value_to_mcp_response(resp_value)
        }
        Err(e) => {
            tracing::warn!("trusty-mpm stdio bridge: transport error: {e:#}");
            Response::err(
                req.id.clone(),
                mcp::error_codes::INTERNAL_ERROR,
                forward_failure_message(&e),
            )
        }
    }
}

/// Convert the daemon's JSON-RPC response value into an `mcp::Response`.
///
/// Why: `run_stdio_loop` expects `mcp::Response`; the daemon returns a raw value
/// in the standard `{jsonrpc, id, result | error}` shape.
/// What: extracts `id`, returns `ok` when `result` is present, `err` when `error`
/// is present, or an internal error when the response is malformed.
/// Test: `value_to_mcp_response_maps_ok_err_and_malformed`.
fn value_to_mcp_response(v: serde_json::Value) -> Response {
    let id = v.get("id").cloned().filter(|id| !id.is_null());

    if let Some(result) = v.get("result").cloned() {
        return Response::ok(id, result);
    }
    if let Some(err) = v.get("error") {
        let code = err
            .get("code")
            .and_then(|c| c.as_i64())
            .map(|c| c as i32)
            .unwrap_or(mcp::error_codes::INTERNAL_ERROR);
        let message = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown daemon error")
            .to_string();
        return Response::err(id, code, message);
    }
    Response::err(
        id,
        mcp::error_codes::INTERNAL_ERROR,
        "daemon returned a response with neither result nor error",
    )
}

/// Ensure the daemon answers on its socket, spawning it when allowed.
///
/// Why: `trusty_mcp::ensure_daemon_up` probes `{base_url}/health` over HTTP,
/// and the bridge reaches the daemon over its socket only (#6288).
/// What: resolves the socket locator (the #9556 sandbox refusal is an error),
/// returns at once when `mpm.health` answers; with `config.no_spawn` set it
/// errors naming the socket and the config's hint; otherwise it spawns
/// `current_exe() + config.spawn_args` detached and polls the socket until
/// healthy or the startup timeout (default 30 s) passes.
/// Test: `tm_client_socket_cutover_6288::the_mcp_bridge_answers_over_the_socket_only`;
/// the refusal by `the_mcp_bridge_refuses_a_missing_socket_without_spawning`.
async fn ensure_daemon_up(config: &DaemonBridgeConfig) -> Result<()> {
    let url = trusty_mpm::core::try_resolve_daemon_url(None).map_err(|e| anyhow!("{e}"))?;
    let daemon = trusty_mpm::client::DaemonClient::new(url.clone());
    if daemon.is_healthy().await {
        return Ok(());
    }
    if config.no_spawn {
        let hint = config.no_spawn_hint.clone().unwrap_or_default();
        return Err(anyhow!(
            "{} daemon is not reachable at {url} — {hint}",
            config.service_name
        ));
    }
    let exe = std::env::current_exe().context("resolve the current executable")?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(&config.spawn_args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    trusty_common::daemon_guard::start_in_new_session(&mut cmd)
        .spawn()
        .context("spawn the trusty-mpm daemon")?;
    let timeout = config.startup_timeout.unwrap_or(Duration::from_secs(30));
    let poll = config.poll_interval.unwrap_or(Duration::from_millis(500));
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        tokio::time::sleep(poll).await;
        if daemon.is_healthy().await {
            return Ok(());
        }
    }
    Err(anyhow!(
        "{} daemon did not answer on {url} within {timeout:?}",
        config.service_name
    ))
}

/// Run the trusty-mpm MCP stdio bridge.
///
/// Why: top-level entry point for `tm serve --stdio` (#1221). Mirrors the proven
/// `trusty-memory serve --stdio` pattern so the three daemons share one mental
/// model and the driver skill can call typed MCP tools instead of scraping CLI
/// output.
/// What: (1) ensures the daemon answers on its socket ([`ensure_daemon_up`]);
/// (2) enters `run_stdio_loop`, suppressing notifications and forwarding every
/// other request as `mpm.mcp.dispatch` over the socket, re-resolved per request.
/// A transport error returns a JSON-RPC internal error rather than crashing the
/// loop — the next request reconnects.
///
/// #9979: `allow_chat` (`--allow-chat`) is off by default; off, the bridge
/// hides `sessions_chat` from `tools/list` and refuses the call locally.
/// Test: the helpers are unit-tested below; the end-to-end forward path is
/// covered by the daemon's `rpc_*` integration tests.
pub(crate) async fn run_stdio_bridge(allow_chat: bool) -> Result<()> {
    // Step 1: ensure the daemon is reachable (auto-start if needed). Hard error
    // on failure — no silent fallback. All output goes to stderr.
    let config = build_bridge_config();
    ensure_daemon_up(&config).await?;

    // #6888: this process, not the daemon, is the one Claude Code spawned inside
    // the managed pane, so it is the only place the caller's own managed session
    // id can be read. Snapshot it once — the pane's environment cannot change
    // under a running bridge.
    let managed_session_id = trusty_common::catchup::session_id::managed_session_id_from_env();

    // Step 2: forward loop, over the socket re-resolved per request.
    mcp::run_stdio_loop(move |req| {
        let managed_session_id = managed_session_id.clone();
        async move {
            if is_notification(&req) {
                return Response::suppressed();
            }
            let url = trusty_mpm::core::resolve_daemon_url(None);
            handle_request(req, &url, managed_session_id.as_deref(), allow_chat).await
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// This bridge's timeout must stay clear of `disk_survey`'s budget clamp.
    ///
    /// Why the two numbers need a test between them (#7313): they live in
    /// different modules and neither reads the other, but they are one
    /// contract — a survey allowed to run for longer than this bridge will wait
    /// produces a transport error instead of an answer, which is exactly what
    /// `budget_seconds: 120` did. Dropping `REQUEST_TIMEOUT` below the clamp
    /// plus headroom reintroduces that silently; this fails instead.
    #[test]
    fn the_forwarding_timeout_leaves_headroom_over_the_disk_survey_budget_clamp() {
        let clamp = Duration::from_secs(trusty_mpm::daemon::mcp_disk::MAX_BUDGET_SECONDS);
        let headroom = Duration::from_secs(5);
        assert!(
            REQUEST_TIMEOUT >= clamp + headroom,
            "a {clamp:?} survey plus {headroom:?} of serialization and round trip \
             does not fit in a {REQUEST_TIMEOUT:?} forwarding timeout — either \
             raise this timeout or lower `mcp_disk::MAX_BUDGET_SECONDS`"
        );
    }

    /// Why (#2486): `build_bridge_config`'s `no_spawn` must track whether a
    /// trusty-mpm launchd unit is registered on this host — this test compares
    /// against `launchd_may_own_daemon()` directly (not via
    /// `compute_no_spawn`, since `compute_no_spawn` is currently the identity
    /// function over that boolean) so the assertion pins the actual invariant
    /// rather than restating `build_bridge_config`'s own call chain. The pure
    /// decision table itself is covered by `commands::launchd_probe::tests`.
    #[test]
    fn build_bridge_config_no_spawn_matches_plist_probe() {
        let plist_exists = crate::commands::launchd_probe::launchd_may_own_daemon();
        let cfg = build_bridge_config();
        assert_eq!(
            cfg.no_spawn, plist_exists,
            "no_spawn must track the live plist probe"
        );
    }

    /// Why (#9556): a sandboxed bridge must fail closed with the sandbox
    /// message instead of spawning on the host default.
    /// Test: itself.
    #[test]
    fn build_bridge_config_never_spawns_under_isolation() {
        let cfg = build_bridge_config_for(true);
        assert!(cfg.no_spawn, "a sandboxed bridge never spawns a daemon");
        let hint = cfg.no_spawn_hint.unwrap_or_default();
        assert!(hint.contains("no daemon for this sandbox"), "{hint}");
    }

    /// Why (#2491 review): the shared helper's generic `no_spawn` hint names a
    /// `{service} setup` subcommand and an `io.trusty.{service}.plist` path
    /// that do not exist for trusty-mpm. `build_bridge_config` must override
    /// that with the real plist names and a working `launchctl` recipe.
    /// What: asserts the built config's `no_spawn_hint` contains both real
    /// plist basenames and does NOT contain the wrong generic wording.
    /// Test: this test.
    #[test]
    fn build_bridge_config_no_spawn_hint_names_real_plists() {
        let cfg = build_bridge_config();
        let hint = cfg
            .no_spawn_hint
            .expect("trusty-mpm must set a no_spawn_hint");
        assert!(
            hint.contains("com.trusty.mpm.plist"),
            "hint must name the real plist; got: {hint}"
        );
        assert!(
            hint.contains("launchctl bootstrap") || hint.contains("launchctl kickstart"),
            "hint must give a working launchctl recipe; got: {hint}"
        );
        assert!(
            !hint.contains("trusty-mpm setup"),
            "hint must NOT reference the nonexistent `trusty-mpm setup` subcommand; got: {hint}"
        );
        assert!(
            !hint.contains("io.trusty."),
            "hint must NOT reference the wrong io.trusty.* plist path; got: {hint}"
        );
    }

    #[test]
    fn is_notification_detects_missing_id_and_prefix() {
        let normal = Request {
            jsonrpc: Some("2.0".into()),
            id: Some(json!(1)),
            method: "tools/list".into(),
            params: None,
        };
        assert!(!is_notification(&normal));

        let no_id = Request {
            jsonrpc: Some("2.0".into()),
            id: None,
            method: "tools/list".into(),
            params: None,
        };
        assert!(is_notification(&no_id));

        let prefixed = Request {
            jsonrpc: Some("2.0".into()),
            id: Some(json!(9)),
            method: "notifications/initialized".into(),
            params: None,
        };
        assert!(is_notification(&prefixed));
    }

    #[test]
    fn value_to_mcp_response_maps_ok_err_and_malformed() {
        let ok = value_to_mcp_response(json!({"jsonrpc":"2.0","id":1,"result":{"tools":[]}}));
        assert!(ok.error.is_none());
        assert_eq!(ok.id, Some(json!(1)));

        let err = value_to_mcp_response(
            json!({"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"x"}}),
        );
        assert_eq!(err.error.unwrap().code, -32601);

        let bad = value_to_mcp_response(json!({"jsonrpc":"2.0","id":3}));
        assert_eq!(bad.error.unwrap().code, mcp::error_codes::INTERNAL_ERROR);

        let null_id = value_to_mcp_response(json!({"jsonrpc":"2.0","id":null,"result":{}}));
        assert_eq!(null_id.id, None);
    }

    #[test]
    fn req_to_value_round_trips_method() {
        let req = Request {
            jsonrpc: Some("2.0".into()),
            id: Some(json!(5)),
            method: "session_list".into(),
            params: None,
        };
        let v = req_to_value(&req);
        assert_eq!(v["method"], "session_list");
        assert_eq!(v["id"], 5);
    }

    /// #9979: only a `sessions_chat` call gets the chat bound; every other
    /// call, including another tool and a non-call method, keeps 60 s.
    #[test]
    fn forwarding_timeout_widens_only_for_a_chat_turn() {
        assert_eq!(REQUEST_TIMEOUT, Duration::from_secs(60));
        assert!(CHAT_REQUEST_TIMEOUT > REQUEST_TIMEOUT);
        assert_eq!(
            forwarding_timeout(&call("sessions_chat", json!({"message": "hi"}))),
            CHAT_REQUEST_TIMEOUT
        );
        for req in [
            call("session_list", json!({})),
            call("disk_survey", json!({})),
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            json!({"jsonrpc": "2.0", "id": 1, "method": "sessions_chat"}),
        ] {
            assert_eq!(forwarding_timeout(&req), REQUEST_TIMEOUT, "{req}");
        }
    }

    /// A daemon socket (0600 in a 0700 directory, as the client dial requires)
    /// that reads one request frame, then answers once the returned sender
    /// fires. It arms no timer of its own, so the paused clock only ever
    /// advances to the client's bound or to the test's own sleep.
    fn slow_socket() -> (String, tempfile::TempDir, tokio::sync::oneshot::Sender<()>) {
        use std::os::unix::fs::PermissionsExt as _;
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
        let dir = tempfile::tempdir().expect("tempdir");
        let sock_dir = dir.path().join("s");
        std::fs::create_dir(&sock_dir).expect("socket dir");
        std::fs::set_permissions(&sock_dir, std::fs::Permissions::from_mode(0o700)).expect("0700");
        let path = sock_dir.join("mpm.sock");
        let std_listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("0600");
        std_listener.set_nonblocking(true).expect("nonblocking");
        let listener = tokio::net::UnixListener::from_std(std_listener).expect("tokio listener");
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            let mut reader = BufReader::new(socket);
            let mut line = String::new();
            let _ = reader.read_line(&mut line).await;
            if released.await.is_err() {
                return;
            }
            let frame = "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n";
            let _ = reader.get_mut().write_all(frame.as_bytes()).await;
        });
        (trusty_mpm::core::socket_locator(&path), dir, release)
    }

    /// #9979: `forward_over_socket` applies `forwarding_timeout`, not a fixed
    /// bound. A daemon that answers after 90 s of paused tokio time is
    /// outlived by the 60 s default but not by the chat bound.
    /// Red if `.timeout(bound)` reverts to `.timeout(REQUEST_TIMEOUT)`.
    #[tokio::test(start_paused = true)]
    async fn forward_over_socket_applies_the_per_request_bound() {
        let (url, _dir, release) = slow_socket();
        let chat = tokio::spawn(async move {
            forward_over_socket(&url, call("sessions_chat", json!({"message": "hi"}))).await
        });
        tokio::time::sleep(Duration::from_secs(90)).await;
        release.send(()).expect("server still waiting");
        let reply = chat
            .await
            .expect("join")
            .expect("a chat turn outlasts 60 s inside the chat bound");
        assert_eq!(reply["ok"], true);

        let (url, _dir, _release) = slow_socket();
        let err = forward_over_socket(&url, call("session_list", json!({})))
            .await
            .expect_err("any other call is cut at 60 s");
        assert!(format!("{err:#}").contains("within 60s"), "{err:#}");
    }

    /// #9979: a forward that times out is reported as a wait that may have
    /// left the call running, not as an unreachable daemon.
    #[tokio::test(start_paused = true)]
    async fn a_forward_timeout_is_not_reported_as_an_unreachable_daemon() {
        let (url, _dir, _release) = slow_socket();
        let err = forward_over_socket(&url, call("session_list", json!({})))
            .await
            .expect_err("times out at 60 s");
        let msg = forward_failure_message(&err);
        assert!(msg.contains("timed out waiting for the daemon"), "{msg}");
        assert!(msg.contains("may still be running"), "{msg}");
        assert!(!msg.contains("unreachable"), "{msg}");

        let dir = tempfile::tempdir().expect("tempdir");
        let absent = trusty_mpm::core::socket_locator(&dir.path().join("absent.sock"));
        let err = forward_over_socket(&absent, json!({"method": "ping"}))
            .await
            .expect_err("nothing listens");
        assert!(forward_failure_message(&err).starts_with("trusty-mpm daemon unreachable"));
    }

    /// A counting fake daemon socket (0600 in a 0700 dir). Every connection is
    /// one request frame answered with `reply` (the JSON-RPC response the
    /// daemon returns, wrapped as the frame's `result`); the counter is the number of
    /// socket calls the bridge made.
    fn counting_socket(
        reply: serde_json::Value,
    ) -> (
        String,
        tempfile::TempDir,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) {
        use std::os::unix::fs::PermissionsExt as _;
        use std::sync::atomic::Ordering;
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
        let dir = tempfile::tempdir().expect("tempdir");
        let sock_dir = dir.path().join("s");
        std::fs::create_dir(&sock_dir).expect("socket dir");
        std::fs::set_permissions(&sock_dir, std::fs::Permissions::from_mode(0o700)).expect("0700");
        let path = sock_dir.join("mpm.sock");
        let std_listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("0600");
        std_listener.set_nonblocking(true).expect("nonblocking");
        let listener = tokio::net::UnixListener::from_std(std_listener).expect("tokio listener");
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let mut reader = BufReader::new(socket);
                let mut line = String::new();
                let _ = reader.read_line(&mut line).await;
                seen.fetch_add(1, Ordering::SeqCst);
                // The socket frame wraps the daemon's JSON-RPC reply as `result`.
                let frame = format!("{}\n", json!({"jsonrpc": "2.0", "id": 1, "result": reply}));
                let _ = reader.get_mut().write_all(frame.as_bytes()).await;
            }
        });
        (trusty_mpm::core::socket_locator(&path), dir, calls)
    }

    fn list_request() -> Request {
        serde_json::from_value(json!({"jsonrpc": "2.0", "id": 7, "method": "tools/list"}))
            .expect("request")
    }

    fn chat_request() -> Request {
        serde_json::from_value(call("sessions_chat", json!({"message": "hi"}))).expect("request")
    }

    fn catalog_reply() -> serde_json::Value {
        json!({"jsonrpc": "2.0", "id": 7, "result": {"tools": [
            {"name": "session_list"}, {"name": "sessions_chat"}, {"name": "disk_survey"}
        ]}})
    }

    fn tool_names(resp: &Response) -> Vec<String> {
        resp.result.as_ref().expect("result")["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .map(|t| t["name"].as_str().expect("name").to_string())
            .collect()
    }

    /// #9979: without `--allow-chat`, `tools/list` omits `sessions_chat` and
    /// keeps the other tools.
    #[tokio::test]
    async fn tools_list_without_allow_chat_omits_sessions_chat() {
        let (url, _dir, _calls) = counting_socket(catalog_reply());
        let resp = handle_request(list_request(), &url, None, false).await;
        assert_eq!(tool_names(&resp), ["session_list", "disk_survey"]);
    }

    /// #9979: with `--allow-chat`, `tools/list` passes through unchanged.
    #[tokio::test]
    async fn tools_list_with_allow_chat_keeps_sessions_chat() {
        let (url, _dir, _calls) = counting_socket(catalog_reply());
        let resp = handle_request(list_request(), &url, None, true).await;
        assert_eq!(
            tool_names(&resp),
            ["session_list", "sessions_chat", "disk_survey"]
        );
    }

    /// #9979: without `--allow-chat`, a `sessions_chat` call is answered
    /// locally with an MCP tool error naming the flag; the daemon is never dialled.
    #[tokio::test]
    async fn a_chat_call_without_allow_chat_is_refused_locally() {
        let (url, _dir, calls) = counting_socket(json!({"jsonrpc": "2.0", "id": 1, "result": {}}));
        let resp = handle_request(chat_request(), &url, None, false).await;
        let result = resp.result.expect("a tool error is a JSON-RPC result");
        assert_eq!(result["isError"], true, "{result}");
        let text = result["content"][0]["text"].as_str().expect("text");
        assert!(
            text.contains("--allow-chat") && text.contains("not enabled"),
            "{text}"
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    /// #9979: with `--allow-chat`, the call reaches the daemon and its reply
    /// comes back verbatim.
    #[tokio::test]
    async fn a_chat_call_with_allow_chat_reaches_the_daemon() {
        let (url, _dir, calls) =
            counting_socket(json!({"jsonrpc": "2.0", "id": 1, "result": {"reply": "hello"}}));
        let resp = handle_request(chat_request(), &url, None, true).await;
        assert_eq!(resp.result.expect("result")["reply"], "hello");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// #6288: with no daemon socket the forward fails naming the socket, and
    /// an HTTP address is never dialled.
    #[tokio::test]
    async fn forward_errors_on_an_absent_socket_naming_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("absent.sock");
        let url = trusty_mpm::core::socket_locator(&socket);
        let err = forward_over_socket(&url, json!({"method": "ping"}))
            .await
            .expect_err("nothing listens");
        assert!(format!("{err:#}").contains("absent.sock"), "{err:#}");
    }

    fn call(tool: &str, args: serde_json::Value) -> serde_json::Value {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": tool, "arguments": args },
        })
    }

    /// Why: #6888 — the daemon cannot read the caller's `TM_MANAGED_SESSION_ID`;
    /// this process can, and stamping it is what lets a managed session's pause
    /// and its next catch-up key the same string.
    /// What: both caller-session tools get the id filled in when they omit it.
    /// Test: itself.
    #[test]
    fn stamp_fills_a_missing_session_id() {
        for tool in CALLER_SESSION_TOOLS {
            let mut req = call(tool, json!({ "project_dir": "/tmp/p" }));
            stamp_caller_session_id(&mut req, Some("mgr-1"));
            assert_eq!(
                req["params"]["arguments"]["session_id"], "mgr-1",
                "{tool} must carry the caller's managed id"
            );
        }
    }

    /// Why: #6888 keeps the explicit override working — naming another
    /// session's id is the deliberate opt-in #5272 defined, and the bridge must
    /// never silently retarget it.
    /// What: an argument object that already has `session_id` is untouched, and
    /// an empty environment stamps nothing.
    /// Test: itself.
    #[test]
    fn stamp_leaves_an_explicit_session_id_alone() {
        let mut req = call(
            "session_context_pause",
            json!({ "project_dir": "/tmp/p", "session_id": "explicit" }),
        );
        stamp_caller_session_id(&mut req, Some("mgr-1"));
        assert_eq!(req["params"]["arguments"]["session_id"], "explicit");

        let mut unmanaged = call("session_context_pause", json!({ "project_dir": "/tmp/p" }));
        stamp_caller_session_id(&mut unmanaged, None);
        assert!(unmanaged["params"]["arguments"]["session_id"].is_null());
        stamp_caller_session_id(&mut unmanaged, Some(""));
        assert!(unmanaged["params"]["arguments"]["session_id"].is_null());
    }

    /// Why: every OTHER tool's `session_id` names a managed SUB-session, so
    /// stamping the caller's own id there would retarget the call — a stop on a
    /// sibling session becomes a stop on yourself.
    /// What: an unrelated tool, and a non-`tools/call` method, are both left
    /// exactly as they arrived.
    /// Test: itself.
    #[test]
    fn stamp_ignores_unrelated_tools() {
        let mut req = call("session_stop", json!({}));
        stamp_caller_session_id(&mut req, Some("mgr-1"));
        assert!(req["params"]["arguments"]["session_id"].is_null());

        let mut ping = json!({"jsonrpc": "2.0", "id": 2, "method": "ping"});
        let before = ping.clone();
        stamp_caller_session_id(&mut ping, Some("mgr-1"));
        assert_eq!(ping, before);
    }
}
