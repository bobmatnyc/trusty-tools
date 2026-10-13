//! `POST /api/v1/channels/ask-owner` and the `ask_owner` MCP tool's daemon
//! body (#9856, #8433 P4).
//!
//! Why: the daemon holds the bot token, the policy snapshot and the question
//! table, so only it may post a question. The asker must be the live managed
//! Architect session, proven from what the kernel reports about the calling
//! process, never from a field the caller writes (#9856 Q1): P5 delivers the
//! answer to the asker's pane, so a PM that could ask "as" the Architect could
//! steer a tainted answer into it.
//! What: [`router`] serves [`ASK_OWNER_ROUTE`]; [`ask_owner_op`] is the
//! shared body: [`architect_asker`] proves the asker, then
//! [`crate::channels::ask_owner::ask_owner`] runs on the blocking pool over
//! [`DaemonAskEnv`]. HTTP carries no kernel-verified caller, so the HTTP
//! route always refuses `asker_unproven`; the MCP tool over the daemon socket
//! (`mpm.mcp.dispatch`, which `tm serve --stdio` uses for `ask_owner`)
//! carries the peer pid.
//! Test: `ask_owner_tests.rs` (this directory).

use std::path::PathBuf;
use std::sync::Arc;

use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use serde_json::{Value, json};
use trusty_channels::policy::{Channel, DenyReason, MessageKind, Route};
use trusty_common::credentials::Secret;

use crate::channels::ask_owner::{
    ASK_KIND, ASK_OWNER_ROUTE, AskEnv, AskError, AskOwnerRequest, Asked, QuestionStore, ask_owner,
};
use crate::channels::audit_log::ChannelAudit;
use crate::channels::slack_bot::{self, Posted, SendFailure, SlackBot, SlackPost};
use crate::channels::snapshot::Refusal;
use crate::core::twin_identity::ClaudeProcess;
use crate::daemon::services::delegation_repair_caller::RepairPeer;
use crate::daemon::state::DaemonState;
use crate::session_manager::{ManagedSessionState, SessionKind, SessionRecord};

/// The `ask_owner` sub-router, merged by `managed_routes::channels_router`.
///
/// What: the HTTP body is an [`AskOwnerRequest`]; HTTP proves no caller, so
/// every request is refused `asker_unproven` (and audited).
/// Test: `http_route_refuses_ask_owner_as_unproven`.
pub fn router() -> Router<Arc<DaemonState>> {
    Router::new().route(
        ASK_OWNER_ROUTE,
        post(
            |State(state): State<Arc<DaemonState>>, Json(req): Json<AskOwnerRequest>| async move {
                reply(ask_owner_op(&state, RepairPeer::http(), req).await)
            },
        ),
    )
}

/// The HTTP status and JSON body for an ask's outcome.
pub fn reply(outcome: Result<Asked, AskError>) -> (StatusCode, Json<Value>) {
    match outcome {
        Ok(asked) => (StatusCode::OK, Json(json!(asked))),
        Err(e) => (
            status_of(&e),
            Json(json!({ "error": e.code(), "message": e.to_string() })),
        ),
    }
}

/// 403 for a rule refusal, 400 for a bad question, 503 for a channel that
/// is not ready, 502 once the post was attempted.
fn status_of(e: &AskError) -> StatusCode {
    match e {
        AskError::AskerUnproven(_)
        | AskError::RecipientNotUserId(_)
        | AskError::Policy(_)
        | AskError::UserTokenRefused(_) => StatusCode::FORBIDDEN,
        AskError::QuestionInvalid => StatusCode::BAD_REQUEST,
        AskError::NoBotRef(_)
        | AskError::TokenUnavailable(_)
        | AskError::TableUnavailable(_)
        | AskError::ReserveFailed(_) => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::BAD_GATEWAY,
    }
}

/// Ask the owner for the caller `peer`, the body the route and the MCP tool
/// share.
///
/// Why: one decision for both transports; see the module doc.
/// What: [`architect_asker`], then `ask_owner` on the blocking pool (it
/// reads the table, the Keychain and the audit file, and waits on Slack). A
/// task that does not finish is `ask_aborted`.
/// Test: `http_route_refuses_ask_owner_as_unproven`; the decision itself in
/// `channels/ask_owner_tests.rs`.
pub async fn ask_owner_op(
    state: &Arc<DaemonState>,
    peer: RepairPeer,
    req: AskOwnerRequest,
) -> Result<Asked, AskError> {
    let asker = architect_asker(state, peer).await;
    let env = DaemonAskEnv {
        state: Arc::clone(state),
        home: state.user_home(),
        bot: SlackBot::production(),
    };
    tokio::task::spawn_blocking(move || ask_owner(&env, asker, &req))
        .await
        .unwrap_or_else(|e| Err(AskError::Aborted(e.to_string())))
}

/// The daemon's [`AskEnv`]: its snapshot, host ceiling, credential
/// registry, question table, Slack bot and channel audit.
struct DaemonAskEnv {
    state: Arc<DaemonState>,
    home: Option<PathBuf>,
    bot: SlackBot,
}

impl AskEnv for DaemonAskEnv {
    fn check_egress(
        &self,
        recipient: &str,
        kind: MessageKind,
    ) -> Result<Route, Refusal<DenyReason>> {
        self.state
            .channel_policy()
            .check_egress(Channel::Slack, recipient, kind)
    }

    fn bot_ref(&self) -> Result<String, String> {
        slack_bot::bot_ref(self.home.as_deref())
    }

    fn bot_token(&self, bot_ref: &str) -> Result<Secret<String>, String> {
        slack_bot::resolve_bot_token(bot_ref)
    }

    fn table(&self) -> Result<Arc<dyn QuestionStore>, String> {
        match self.state.question_table() {
            Ok(table) => Ok(table),
            Err(e) => Err(e.to_string()),
        }
    }

    fn post(
        &self,
        token: &Secret<String>,
        recipient: &str,
        text: &str,
    ) -> Result<Posted, SendFailure> {
        self.bot.post(token, recipient, text)
    }

    fn audit(&self, to: &str, reason: &'static str) -> Result<(), String> {
        ChannelAudit::for_home(self.home.as_deref())
            .send_refused(Channel::Slack, to, ASK_KIND, reason)
            .map_err(|e| e.to_string())
    }
}

/// The managed session id of the live Architect that sent this request, or
/// why none is proven (#9856 Q1).
///
/// Why: see the module doc. The proof is the #8531 one: the kernel's peer
/// pid, walked to its nearest `claude`, compared by pid AND start time.
/// What: `Err` for an unproven peer (HTTP, or no pid). Otherwise lists the
/// managed records and, on the blocking pool, runs [`architect_asker_with`]
/// over the peer walk (`SessionClaudes::peer_claude`) and each Architect
/// record's own pane (`SessionManager::owned_pane`, then the `claude` in that
/// pane with its start time).
/// Test: `architect_asker_with`'s cases; `http_route_refuses_ask_owner_as_unproven`.
pub async fn architect_asker(state: &Arc<DaemonState>, peer: RepairPeer) -> Result<String, String> {
    let (pid, seen_at) = match peer {
        RepairPeer::Kernel { pid, seen_at } => (pid, seen_at),
        RepairPeer::Unproven(why) => return Err(why),
    };
    let mgr = state.session_manager().await;
    let records = mgr.list().await;
    let held = Arc::clone(state);
    tokio::task::spawn_blocking(move || {
        architect_asker_with(
            pid,
            seen_at,
            &records,
            |pid, seen_at| held.session_claudes().peer_claude(pid, seen_at),
            |record| {
                let pane = mgr
                    .owned_pane(&record.id, record)
                    .map_err(|e| e.to_string())?;
                let grant = crate::daemon::state::session_claudes::ResumeGrant {
                    tmux_name: record.tmux_name.clone(),
                    pane_id: Some(pane),
                    issued_at: 0,
                };
                crate::daemon::state::session_claude_resume::pane_claude(&grant)
            },
        )
    })
    .await
    .map_err(|e| format!("the caller check did not finish: {e}"))?
}

/// [`architect_asker`] over an injected peer walk and pane lookup.
///
/// Why: every refusal arm must be testable without a live session: a peer
/// with no `claude` above it, a PM's `claude`, a reused pid, a stopped or
/// missing Architect record.
/// What: the caller is `peer_claude(pid, seen_at)`. Only records of kind
/// `Architect` in state `Active` are candidates; `Ok(id)` for the first whose
/// pane `claude` equals the caller (pid and start time). Otherwise `Err`
/// naming the failed step and every pane lookup error.
/// Test: `the_live_architects_claude_is_the_asker`,
/// `a_pm_sessions_claude_is_not_the_asker`,
/// `a_reused_pid_with_another_start_time_is_not_the_asker`,
/// `a_stopped_architect_record_proves_nothing`,
/// `a_failed_peer_walk_is_refused`.
pub(crate) fn architect_asker_with(
    pid: u32,
    seen_at: u64,
    records: &[SessionRecord],
    peer_claude: impl Fn(u32, u64) -> Result<ClaudeProcess, String>,
    pane_claude: impl Fn(&SessionRecord) -> Result<ClaudeProcess, String>,
) -> Result<String, String> {
    let caller = peer_claude(pid, seen_at)?;
    let live = records
        .iter()
        .filter(|r| r.kind == SessionKind::Architect && r.state == ManagedSessionState::Active);
    let mut failures = Vec::new();
    let mut candidates = 0usize;
    for record in live {
        candidates += 1;
        match pane_claude(record) {
            // #9856 Q1: pid AND start time — a reused pid is another process.
            Ok(claude) if claude == caller => return Ok(record.id.to_string()),
            Ok(_) => {}
            Err(e) => failures.push(format!("{}: {e}", record.tmux_name)),
        }
    }
    if candidates == 0 {
        return Err("no live managed Architect session is registered".to_owned());
    }
    let mut why = format!(
        "the calling process runs under claude pid {}, which is not the live Architect \
         session's claude",
        caller.pid
    );
    if !failures.is_empty() {
        why.push_str(&format!("; {}", failures.join("; ")));
    }
    Err(why)
}

#[cfg(test)]
#[path = "ask_owner_tests.rs"]
mod tests;
