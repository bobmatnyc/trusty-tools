//! The daemon body of one inbound channel answer (#9873, #8433 P5).
//!
//! Why: the daemon holds the policy snapshot, the question table, the taint
//! directory and the asker's pane, so only it may resolve a question and
//! deliver its answer. No production source calls it before P6 (Socket
//! Mode); the sandbox route (`inbound_fake`) is its only caller (Q1).
//! What: [`inbound_op`] runs `channels::inbound::decide` on the blocking pool
//! over [`DaemonInboundEnv`] (table, audit and the asker's pane proof all
//! block), then `channels::deliver::deliver` over [`DaemonDeliverEnv`]: the
//! taint and the trace on the blocking pool, the send through
//! `SessionManager::inject`. [`reply`] maps the outcome to HTTP (#9873 Q6).
//! Test: `inbound_tests.rs` (this directory).

use std::path::PathBuf;
use std::sync::Arc;

use axum::{Json, http::StatusCode};
use serde_json::{Value, json};
use trusty_channels::policy::{Channel, DropReason, QuestionId, QuestionState, Route};

use crate::channels::audit_log::ChannelAudit;
use crate::channels::deliver::{AskerView, DeliverEnv, Delivery, check_asker, deliver};
use crate::channels::inbound::{
    AnswerTable, Dropped, InboundEnv, InboundEvent, RateBucket, RateTake, Verdict, decide,
};
use crate::channels::question_table::Question;
use crate::channels::snapshot::Refusal;
use crate::core::channels_delivery::{self, DeliveryLine};
use crate::core::channels_taint::{self, TaintMark};
use crate::core::sm::control::Submit;
use crate::daemon::state::DaemonState;
use crate::session_manager::{ManagedSessionId, SessionManager, SessionRecord};

/// The outcome of one inbound message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboundReply {
    /// Resolved, tainted and delivered.
    Delivered {
        /// The question it answered.
        question_id: QuestionId,
    },
    /// Dropped; nothing changed.
    Dropped(Dropped),
    /// Resolved, but not delivered or not confirmed; final (#8433 Q4).
    DeliveryFailed {
        /// The question it answered.
        question_id: QuestionId,
        /// The delivery step that failed.
        code: &'static str,
        /// Whether the send ran.
        sent: bool,
    },
    /// The decision's task did not finish; the question may be resolved.
    Aborted(String),
}

/// Judge, resolve and deliver `ev` for the daemon `state`.
///
/// Why: see the module doc.
/// What: lists the managed records (the asker preflight reads them), then
/// [`run_inbound`] over the daemon's two environments.
/// Test: `inbound_fake_route_status_map`; the steps in `run_inbound`'s tests.
pub async fn inbound_op(state: &Arc<DaemonState>, ev: InboundEvent) -> InboundReply {
    let mgr = state.session_manager().await;
    let records = mgr.list().await;
    let home = state.user_home();
    let inbound = DaemonInboundEnv {
        state: Arc::clone(state),
        home: home.clone(),
        records,
        mgr: Arc::clone(&mgr),
    };
    let deliverer = DaemonDeliverEnv {
        mgr,
        dir: home.map(|h| channels_taint::taint_dir(&h)),
    };
    run_inbound(Arc::new(inbound), &deliverer, ev).await
}

/// Decide `ev` over `inbound` on the blocking pool, then deliver a resolved
/// answer over `env`.
///
/// Why: the decision blocks (table fsync, audit, tmux pane proof); the
/// delivery awaits the session manager. Only a `Resolved` verdict, which
/// only the CAS winner holds, reaches `deliver`, so a question is delivered
/// at most once.
/// What: a decision task that does not finish (a panic after `resolve`
/// included) is [`InboundReply::Aborted`], logged at ERROR; the question may
/// be resolved with no delivery record, which `tm doctor` reports (Q4a).
/// Test: `concurrent_inbound_answers_deliver_once`,
/// `aborted_task_after_resolve_is_reported_not_ok`.
pub(crate) async fn run_inbound(
    inbound: Arc<dyn InboundEnv + Send + Sync>,
    env: &impl DeliverEnv,
    ev: InboundEvent,
) -> InboundReply {
    let verdict = match tokio::task::spawn_blocking(move || decide(&*inbound, &ev)).await {
        Ok(verdict) => verdict,
        Err(e) => {
            // #9873 (F16): never reported ok; the doctor row decides.
            tracing::error!(error = %e, "inbound answer: the decision task did not finish");
            return InboundReply::Aborted(e.to_string());
        }
    };
    let resolved = match verdict {
        Verdict::Dropped(dropped) => return InboundReply::Dropped(dropped),
        Verdict::Resolved(resolved) => resolved,
    };
    let question_id = resolved.question().id;
    match deliver(env, &resolved).await {
        Delivery::Delivered => InboundReply::Delivered { question_id },
        Delivery::Failed { code, sent } => InboundReply::DeliveryFailed {
            question_id,
            code,
            sent,
        },
    }
}

/// The HTTP status and JSON body for `outcome` (#9873 Q6, AC3).
///
/// What: 200 delivered; a drop by [`status_of`] its code, with
/// `already_resolved` carrying the first answer's answerer and time; 502
/// with `resolved: true` for a delivery failure after `resolve`; 500
/// `inbound_aborted`.
/// Test: `inbound_fake_route_status_map`.
pub fn reply(outcome: &InboundReply) -> (StatusCode, Json<Value>) {
    match outcome {
        InboundReply::Delivered { question_id } => (
            StatusCode::OK,
            Json(json!({ "resolved": true, "delivered": true, "question_id": question_id })),
        ),
        InboundReply::Dropped(Dropped {
            code,
            first: Some(first),
        }) => (
            status_of(code),
            Json(json!({
                "error": code,
                "resolved": true,
                "already_answered_by": first.answerer,
                "at": first.answered_at,
            })),
        ),
        InboundReply::Dropped(Dropped { code, first: None }) => (
            status_of(code),
            Json(json!({ "error": code, "resolved": false })),
        ),
        InboundReply::DeliveryFailed {
            question_id,
            code,
            sent,
        } => (
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "error": code,
                "resolved": true,
                "delivered": false,
                "sent": sent,
                "question_id": question_id,
            })),
        ),
        InboundReply::Aborted(why) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "inbound_aborted", "message": why })),
        ),
    }
}

/// The status of a drop code (#9873 plan §4 status map).
///
/// What: 403 a sender or route rule; 404 no such question; 409 already
/// answered; 422 a message that cannot bind or is malformed; 429 the rate
/// window; 503 the channel or asker not ready; any other code 500.
/// Test: `inbound_fake_route_status_map`.
pub fn status_of(code: &str) -> StatusCode {
    match code {
        "unknown_sender" | "not_recipient" | "no_question_kind" | "not_owned_by_route" => {
            StatusCode::FORBIDDEN
        }
        "no_open_question" | "unknown_question" => StatusCode::NOT_FOUND,
        "already_resolved" => StatusCode::CONFLICT,
        "no_binding" | "ambiguous_binding" | "answer_too_long" | "answer_empty"
        | "channel_not_served" => StatusCode::UNPROCESSABLE_ENTITY,
        "rate_limited" => StatusCode::TOO_MANY_REQUESTS,
        "table_unavailable" | "table_poisoned" | "rate_limiter_poisoned" | "asker_unavailable" => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        code if code.starts_with("snapshot_") => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// The daemon's [`InboundEnv`]: its snapshot, question table, rate windows,
/// channel audit, and the managed records listed before the decision.
struct DaemonInboundEnv {
    state: Arc<DaemonState>,
    home: Option<PathBuf>,
    records: Vec<SessionRecord>,
    mgr: Arc<SessionManager>,
}

impl InboundEnv for DaemonInboundEnv {
    fn check_inbound(
        &self,
        channel: Channel,
        sender: &str,
        binding: &[QuestionId],
        questions: &[QuestionState],
    ) -> Result<(Route, QuestionId), Refusal<DropReason>> {
        self.state
            .channel_policy()
            .check_inbound(channel, sender, binding, questions)
    }

    fn table(&self) -> Result<Arc<dyn AnswerTable>, String> {
        match self.state.question_table() {
            Ok(table) => Ok(table),
            Err(e) => Err(e.to_string()),
        }
    }

    fn take_rate(&self, bucket: RateBucket<'_>) -> RateTake {
        self.state.inbound_limiter().take(bucket)
    }

    fn preflight(&self, question: &Question) -> Result<(), String> {
        // #9873 Q4: the record, its id and workspace, and its pane, before
        // `resolve`; the deliverer checks them again after it.
        let asker = &question.ask.asker_managed_session;
        let record = self
            .records
            .iter()
            .find(|r| r.id.to_string() == *asker)
            .ok_or_else(|| format!("the asker {asker} has no managed record"))?;
        check_asker(&AskerView::of(record)).map_err(str::to_owned)?;
        self.mgr
            .owned_pane(&record.id, record)
            .map(drop)
            .map_err(|e| e.to_string())
    }

    fn audit(
        &self,
        ev: &InboundEvent,
        code: &'static str,
        rate_limited: bool,
    ) -> Result<(), String> {
        let audit = ChannelAudit::for_home(self.home.as_deref());
        let written = if rate_limited {
            audit.rate_limited(ev.channel, &ev.sender, ev.text.len(), code)
        } else {
            audit.inbound_dropped(ev.channel, &ev.sender, ev.text.len(), code)
        };
        written.map_err(|e| e.to_string())
    }
}

/// The daemon's [`DeliverEnv`]: the session manager and the taint directory
/// (`None` with no home, so every taint and trace write fails).
struct DaemonDeliverEnv {
    mgr: Arc<SessionManager>,
    dir: Option<PathBuf>,
}

impl DaemonDeliverEnv {
    fn dir(&self) -> Result<PathBuf, String> {
        self.dir
            .clone()
            .ok_or_else(|| "no home directory, so no taint directory".to_owned())
    }
}

/// `managed` as a managed session id.
fn managed_id(managed: &str) -> Result<ManagedSessionId, String> {
    uuid::Uuid::parse_str(managed)
        .map(ManagedSessionId)
        .map_err(|e| format!("{managed:?} is not a managed session id: {e}"))
}

impl DeliverEnv for DaemonDeliverEnv {
    async fn asker(&self, managed: &str) -> Result<AskerView, String> {
        let id = managed_id(managed)?;
        let record = self.mgr.get(&id).await.map_err(|e| e.to_string())?;
        Ok(AskerView::of(&record))
    }

    async fn taint(
        &self,
        managed: &str,
        claude_session_id: &str,
        mark: &TaintMark,
    ) -> Result<(), String> {
        let dir = self.dir()?;
        let (managed, claude, mark) = (
            managed.to_owned(),
            claude_session_id.to_owned(),
            mark.clone(),
        );
        tokio::task::spawn_blocking(move || {
            channels_taint::taint_managed_session(&dir, &managed, &claude, &mark)
        })
        .await
        .map_err(|e| format!("the taint write did not finish: {e}"))?
        .map_err(|e| e.to_string())
    }

    async fn record(&self, line: &DeliveryLine) -> Result<(), String> {
        let dir = self.dir()?;
        let line = line.clone();
        tokio::task::spawn_blocking(move || channels_delivery::record_delivery(&dir, &line))
            .await
            .map_err(|e| format!("the trace write did not finish: {e}"))?
            .map_err(|e| e.to_string())
    }

    async fn send(&self, managed: &str, line: &str) -> Result<(), String> {
        let id = managed_id(managed)?;
        // #9873 Q3: `inject` refuses a stopped record and proves the pane.
        self.mgr
            .inject(&id, line, Submit::Enter)
            .await
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
#[path = "inbound_tests.rs"]
mod tests;

// #9877: a lost SessionEnd never lets an answer reach an untainted id.
#[cfg(test)]
#[path = "inbound_seen_tests.rs"]
mod seen_tests;
