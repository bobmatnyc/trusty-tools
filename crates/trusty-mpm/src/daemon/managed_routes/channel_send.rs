//! `POST /api/v1/channels/send` and the `channel_send` MCP tool's daemon
//! body (#10423, epic #10416 S1b).
//!
//! Why: the daemon owns every outward send (D9a). A send is only staged
//! here; nothing posts until the owner approves the exact text (Dd9).
//! What: [`router`] serves [`SEND_ROUTE`]; [`send_op`] is the body both
//! transports share. On the blocking pool, [`SendEnv::stage`] reads
//! `channels/orgs.toml`, loads the table if the slot is empty, stages the
//! text through the S1a gate with the daemon's [`JsonlSendAudit`] and
//! [`JsonSendStore`], then asks the owner ([`request_approval`]: the owner's
//! Slack DM, else the dashboard). [`SendEnv::decide`] is the Approve path
//! the authenticated inbound transport (S2) calls: it authenticates the
//! approver as the configured owner before it touches the record. Every
//! fault refuses and posts nothing: an unreadable config, a table that
//! cannot load, an audit or persist failure, an unauthenticated approver.
//! Test: `channel_send_tests.rs` (this directory).

use std::path::PathBuf;
use std::sync::Arc;

use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::channels::approval::{ApprovalVia, request_approval};
use crate::channels::orgs::OrgsConfig;
use crate::channels::secret_ref::StoreResolver;
use crate::channels::send_store::{
    JsonSendStore, JsonlSendAudit, load_table, now_secs, orgs_path, send_audit_path, table_path,
};
use crate::channels::slack_bot::{SlackBot, SlackPost};
use crate::channels::staged_send::{
    Decision, OwnerApproval, SecretResolver, SendError, SendIo, SendState, SlackDmSender,
    StagedSends,
};
use crate::daemon::state::DaemonState;

/// The route a send is staged through.
pub const SEND_ROUTE: &str = "/api/v1/channels/send";

/// A send request: a configured user, the exact text, and an optional org
/// pin (refused unless it is the user's own org).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendRequest {
    /// The configured user name (`users:` key).
    pub user: String,
    /// The exact text to send once approved.
    pub text: String,
    /// The org the caller expects; a different org is `cross_org`.
    #[serde(default)]
    pub via_org: Option<String>,
}

/// A staged send, as the caller sees it. Holds no message text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Staged {
    /// The staged id.
    pub id: u64,
    /// The SHA-256 digest the owner's Approve must carry.
    pub digest: String,
    /// The recipient's org.
    pub org: String,
    /// Where the approval request went.
    pub approval: ApprovalVia,
}

/// Why a send or a decision was refused. Nothing was posted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SendOpError {
    /// No home directory, so no config and no table.
    #[error("no_home: the daemon has no home directory")]
    NoHome,
    /// `orgs.toml` is missing, unreadable or invalid.
    #[error("config_unavailable: {0}")]
    Config(String),
    /// The staged-send table cannot be loaded or locked.
    #[error("table_unavailable: {0}")]
    Table(String),
    /// The gate refused.
    #[error(transparent)]
    Send(#[from] SendError),
    /// The blocking task did not finish.
    #[error("send_aborted")]
    Aborted,
}

impl SendOpError {
    /// The content-free rule code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoHome => "no_home",
            Self::Config(_) => "config_unavailable",
            Self::Table(_) => "table_unavailable",
            Self::Send(e) => e.code(),
            Self::Aborted => "send_aborted",
        }
    }

    /// 403 for a rule refusal, 400 for bad text, 404 for an unknown id,
    /// 409 for a decided one, 503 when the channel is not ready.
    fn status(&self) -> StatusCode {
        match self {
            Self::Send(SendError::TextInvalid) => StatusCode::BAD_REQUEST,
            Self::Send(SendError::UnknownId) => StatusCode::NOT_FOUND,
            Self::Send(SendError::AlreadyDecided) => StatusCode::CONFLICT,
            Self::Send(
                SendError::Route(_)
                | SendError::DigestMismatch
                | SendError::ApproverUnauthenticated
                | SendError::RouteChanged,
            ) => StatusCode::FORBIDDEN,
            _ => StatusCode::SERVICE_UNAVAILABLE,
        }
    }
}

/// The `channel_send` sub-router, merged by `managed_routes::channels_router`.
///
/// Test: `http_send_route_stages_and_refuses_without_config`.
pub fn router() -> Router<Arc<DaemonState>> {
    Router::new().route(
        SEND_ROUTE,
        post(
            |State(state): State<Arc<DaemonState>>, Json(req): Json<SendRequest>| async move {
                reply(send_op(&state, req).await)
            },
        ),
    )
}

/// The HTTP status and JSON body for a send's outcome.
pub fn reply(outcome: Result<Staged, SendOpError>) -> (StatusCode, Json<Value>) {
    match outcome {
        Ok(staged) => (StatusCode::OK, Json(json!(staged))),
        Err(e) => (
            e.status(),
            Json(json!({ "error": e.code(), "message": e.to_string() })),
        ),
    }
}

/// Stage `req` for the owner's Approve; the body the route and the MCP
/// tool share.
///
/// What: [`SendEnv::stage`] on the blocking pool with the daemon's home,
/// Slack bot and trusty-secrets resolver.
/// Test: `http_send_route_stages_and_refuses_without_config`.
pub async fn send_op(state: &Arc<DaemonState>, req: SendRequest) -> Result<Staged, SendOpError> {
    let state = Arc::clone(state);
    tokio::task::spawn_blocking(move || {
        let home = state.user_home().ok_or(SendOpError::NoHome)?;
        let env = SendEnv {
            home,
            poster: SlackBot::production(),
            resolver: DaemonResolver,
        };
        let mut slot = state.send_table().map_err(SendOpError::Table)?;
        env.stage(&mut slot, &req, now_secs())
    })
    .await
    .unwrap_or(Err(SendOpError::Aborted))
}

/// The daemon's resolver: opened per call, so a missing index is a
/// credential fault on that send, never a daemon fault.
pub struct DaemonResolver;

impl SecretResolver for DaemonResolver {
    fn resolve(
        &self,
        scope: Option<&str>,
        secret_ref: &str,
    ) -> Result<trusty_common::credentials::Secret<String>, String> {
        StoreResolver::production()?.resolve(scope, secret_ref)
    }
}

/// The paths, poster and resolver a send runs against.
pub struct SendEnv<P: SlackPost, R: SecretResolver> {
    /// The user home the `channels/` files live under.
    pub home: PathBuf,
    /// The Slack poster (approval DMs and Slack sends).
    pub poster: P,
    /// The `secret://` resolver.
    pub resolver: R,
}

impl<P: SlackPost, R: SecretResolver> SendEnv<P, R> {
    /// Read and validate `channels/orgs.toml`.
    ///
    /// Test: `missing_config_refuses_and_posts_nothing`.
    pub fn config(&self) -> Result<OrgsConfig, SendOpError> {
        let path = orgs_path(&self.home);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| SendOpError::Config(format!("{}: {}", path.display(), e.kind())))?;
        OrgsConfig::parse_toml(&text).map_err(|e| SendOpError::Config(e.to_string()))
    }

    /// The table in `slot`, loading it when the slot is empty.
    ///
    /// Why: #10423 Fail-Open: a table that cannot load refuses the send;
    /// the failure is not cached, so the next call retries the load.
    /// Test: `table_load_failure_refuses_and_posts_nothing`.
    fn table<'s>(
        &self,
        slot: &'s mut Option<StagedSends>,
    ) -> Result<&'s mut StagedSends, SendOpError> {
        if slot.is_none() {
            let table = load_table(&table_path(&self.home)).map_err(|e| SendOpError::Table(e.0))?;
            *slot = Some(table);
        }
        slot.as_mut()
            .ok_or_else(|| SendOpError::Table("not loaded".into()))
    }

    /// The daemon's audit sink and store under this home.
    fn sinks(&self) -> (JsonlSendAudit, JsonSendStore) {
        (
            JsonlSendAudit {
                path: send_audit_path(&self.home),
            },
            JsonSendStore {
                path: table_path(&self.home),
            },
        )
    }

    /// Stage `req` and ask the owner. Posts only the approval request.
    ///
    /// Test: `send_stages_and_asks_the_owner_dm_then_posts_once_on_approve`,
    /// `table_load_failure_refuses_and_posts_nothing`,
    /// `audit_failure_refuses_and_posts_nothing`.
    pub fn stage(
        &self,
        slot: &mut Option<StagedSends>,
        req: &SendRequest,
        now: u64,
    ) -> Result<Staged, SendOpError> {
        let cfg = self.config()?;
        let (audit, store) = self.sinks();
        let io = SendIo {
            audit: &audit,
            store: &store,
        };
        let table = self.table(slot)?;
        let id = table.stage(&cfg, &req.user, req.via_org.as_deref(), &req.text, now, &io)?;
        let send = table.get(id).ok_or(SendError::UnknownId)?;
        let approval = request_approval(&cfg, id, send, &self.poster, &self.resolver);
        Ok(Staged {
            id,
            digest: send.digest.clone(),
            org: send.route.org.clone(),
            approval,
        })
    }

    /// Apply `decision` from the identity `(org, user)` an authenticated
    /// transport reported.
    ///
    /// Why: #10423: only the configured owner may decide, not anyone who
    /// holds the digest.
    /// What: [`OwnerApproval::authenticate`] first (refused before the table
    /// is read). An Approve then needs the route rebuilt from the current
    /// `orgs.toml` to equal the staged one (`StagedSends::check_route`,
    /// #10423), then `StagedSends::decide` with a [`SlackDmSender`].
    /// Test: `unauthenticated_approve_posts_nothing_through_the_daemon`,
    /// `approve_after_scope_change_is_refused_and_posts_nothing`,
    /// `resolve_failure_posts_nothing_through_the_daemon`.
    pub fn decide(
        &self,
        slot: &mut Option<StagedSends>,
        identity: (&str, &str),
        id: u64,
        decision: &Decision,
        now: u64,
    ) -> Result<SendState, SendOpError> {
        let cfg = self.config()?;
        let approver = OwnerApproval::authenticate(&cfg, identity.0, identity.1)?;
        let (audit, store) = self.sinks();
        let io = SendIo {
            audit: &audit,
            store: &store,
        };
        let sender = SlackDmSender {
            poster: &self.poster,
            resolver: &self.resolver,
        };
        let table = self.table(slot)?;
        if matches!(decision, Decision::Approve { .. }) {
            table.check_route(id, &cfg, &io)?;
        }
        Ok(table.decide(id, &approver, decision, now, &sender, &io)?)
    }
}
