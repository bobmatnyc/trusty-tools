//! The owner's approval request for a staged send (#10423, S1b).
//!
//! Why: #10423 acceptance 3 / D9a input 4: the approval request goes to the
//! owner's Slack DM for every target org, with the dashboard as fallback.
//! What: [`request_approval`] posts one DM to the owner's member id with the
//! owner org's [`SLACK_BOT_ROLE`] credential, carrying the staged text, its
//! digest and the reply forms. No owner, no bot ref, a resolver error or a
//! rejected post falls back to [`ApprovalVia::Dashboard`] with a content-free
//! reason; the send stays `Staged` either way. A post whose outcome is
//! unknown is `OwnerDmUnknown`, never re-posted.
//! Test: `approval_request_goes_to_the_owner_dm_for_every_org`,
//! `approval_falls_back_to_the_dashboard`.

use serde::Serialize;

use crate::channels::orgs::{ApprovalTarget, OrgsConfig};
use crate::channels::slack_bot::{SendFailure, SlackPost};
use crate::channels::staged_send::{SLACK_BOT_ROLE, SecretResolver, StagedSend};

/// Where the approval request for a staged send went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "via", content = "reason", rename_all = "snake_case")]
pub enum ApprovalVia {
    /// Posted to the owner's Slack DM.
    OwnerDm,
    /// The DM post may have landed; it is not re-posted.
    OwnerDmUnknown(String),
    /// Waiting on the dashboard; the reason is a fixed code.
    Dashboard(String),
}

/// The DM text the owner reads: the exact staged text and how to answer.
pub fn approval_text(id: u64, send: &StagedSend) -> String {
    format!(
        "Approve send #{id} to {} via org {}?\n\n{}\n\nReply `approve {id} {}` or `deny {id}`.",
        send.route.address,
        send.route.org,
        send.text(),
        send.digest
    )
}

/// Ask the owner to approve staged send `id`.
///
/// Why/What: see the module doc. Blocking.
/// Test: `approval_request_goes_to_the_owner_dm_for_every_org`,
/// `approval_falls_back_to_the_dashboard`.
pub fn request_approval(
    cfg: &OrgsConfig,
    id: u64,
    send: &StagedSend,
    poster: &impl SlackPost,
    resolver: &impl SecretResolver,
) -> ApprovalVia {
    let ApprovalTarget::OwnerSlackDm { org, user } = cfg.approval_target() else {
        return ApprovalVia::Dashboard("no_owner".into());
    };
    let Some((scope, secret_ref)) = cfg.org(&org).and_then(|o| {
        o.credentials
            .get(SLACK_BOT_ROLE)
            .map(|r| (o.secrets.as_deref(), r))
    }) else {
        return ApprovalVia::Dashboard("no_bot_ref".into());
    };
    let Ok(token) = resolver.resolve(scope, secret_ref) else {
        // #10423: the resolver's text is dropped; it may name the store.
        tracing::warn!(org = %org, "channel send: owner bot credential unavailable");
        return ApprovalVia::Dashboard("credential_unavailable".into());
    };
    match poster.post(&token, &user, &approval_text(id, send)) {
        Ok(_) | Err(SendFailure::Unrecorded(_)) => ApprovalVia::OwnerDm,
        Err(SendFailure::Unknown(code)) => ApprovalVia::OwnerDmUnknown(code),
        Err(SendFailure::Rejected(code)) => ApprovalVia::Dashboard(code),
    }
}
