//! Staged outbound sends and the owner's Approve gate (#10417, epic #10416 S1).
//!
//! Why: Dd9: every outward send waits for the owner's Approve on the exact
//! text. This is a Fail-Open Check site: no refusal, timeout, credential
//! fault or unknown outcome may post, and a send whose outcome is unknown is
//! never reissued.
//! What: [`StagedSends::stage`] resolves the recipient's org route
//! ([`crate::channels::orgs::OrgsConfig::route`]) and records the text with
//! its SHA-256 digest. [`StagedSends::decide`] takes the owner's
//! [`Decision`]: only `Approve` carrying the staged digest, inside the TTL,
//! on a still-`Staged` record posts, exactly once, through a
//! [`ChannelSender`]. The state moves to `Sending` before the send, so no
//! later call can post it again. [`SlackDmSender`] posts with
//! `chat.postMessage` and `channel = <member id>` (chat:write only, no
//! `conversations.open`). Every step writes one [`AuditLine`] that holds no
//! message text and no credential value.
//! Test: `staged_send_tests.rs`.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use trusty_common::credentials::Secret;

use crate::channels::orgs::{ChannelKind, OrgRoute, OrgsConfig, RouteError};
use crate::channels::slack_bot::{SendFailure, SlackPost};

/// How long a staged send waits for Approve before it expires.
pub const DEFAULT_APPROVE_TTL: Duration = Duration::from_secs(15 * 60);

/// The credential role a Slack org's bot token is stored under.
pub const SLACK_BOT_ROLE: &str = "bot_token";

/// The longest text a send may carry, in bytes.
pub const MAX_SEND_BYTES: usize = 4000;

/// The state of one staged send. Every state but `Staged` is final.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendState {
    /// Waiting for the owner's decision.
    Staged,
    /// The owner denied it; nothing was posted.
    Denied,
    /// The TTL passed; nothing was posted.
    Expired,
    /// The send started; held while the channel answers.
    Sending,
    /// The channel took it.
    Sent,
    /// Nothing was posted (a refusal, a credential fault, a rejection).
    NotSent(String),
    /// It may have been posted; never reissued.
    Unknown(String),
}

/// The owner's reply to an approval request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Approve the text whose SHA-256 hex digest is given.
    Approve {
        /// The digest of the text the owner saw.
        digest: String,
    },
    /// Deny the send.
    Deny,
    /// Any other reply; it is not an approval.
    FreeText,
}

/// One audit line. It never carries message text or a credential value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditLine {
    /// The staged id, when one was assigned.
    pub id: Option<u64>,
    /// The step: `staged`, `refused`, `denied`, `expired`, `sent`, ...
    pub event: &'static str,
    /// The org, when a route resolved.
    pub org: Option<String>,
    /// A content-free code.
    pub code: String,
}

/// Where audit lines go. The daemon's writes the channel audit log.
pub trait SendAudit {
    /// Record `line`.
    fn record(&self, line: AuditLine);
}

/// Posts one approved text to one route. Blocking.
pub trait ChannelSender {
    /// Send `text` to `route`'s address on its channel.
    fn send(&self, route: &OrgRoute, text: &str) -> Result<(), SendFailure>;
}

/// One staged send.
#[derive(Clone)]
pub struct StagedSend {
    /// The route resolved at stage time.
    pub route: OrgRoute,
    text: String,
    /// SHA-256 hex digest of the text; Approve must carry it.
    pub digest: String,
    staged_at: Instant,
    /// The current state.
    pub state: SendState,
}

/// Why: a `{:?}` log must never print the staged message text (#10417).
/// What: shows the route, the text length, a 12-hex digest prefix and the
/// state; the text appears as `<redacted>`.
/// Test: `debug_output_redacts_the_staged_text`.
impl std::fmt::Debug for StagedSend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // #10417: never print `text`; the digest prefix identifies it.
        f.debug_struct("StagedSend")
            .field("route", &self.route)
            .field("text", &"<redacted>")
            .field("text_len", &self.text.len())
            .field("digest", &self.digest.get(..12).unwrap_or(&self.digest))
            .field("staged_at", &self.staged_at)
            .field("state", &self.state)
            .finish()
    }
}

/// Why a stage or a decision was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SendError {
    /// No route, or a cross-org send.
    #[error(transparent)]
    Route(#[from] RouteError),
    /// Empty text or text over [`MAX_SEND_BYTES`].
    #[error("text_invalid: the text must be 1..={MAX_SEND_BYTES} bytes")]
    TextInvalid,
    /// No staged send has that id.
    #[error("unknown_id")]
    UnknownId,
    /// The send was already decided; nothing is posted again.
    #[error("already_decided")]
    AlreadyDecided,
    /// The Approve digest does not match the staged text.
    #[error("digest_mismatch")]
    DigestMismatch,
}

/// The table of staged sends.
#[derive(Debug)]
pub struct StagedSends {
    next: u64,
    ttl: Duration,
    entries: BTreeMap<u64, StagedSend>,
}

impl Default for StagedSends {
    fn default() -> Self {
        Self::new(DEFAULT_APPROVE_TTL)
    }
}

impl StagedSends {
    /// An empty table whose sends expire after `ttl`.
    pub fn new(ttl: Duration) -> Self {
        Self {
            next: 1,
            ttl,
            entries: BTreeMap::new(),
        }
    }

    /// The staged send `id`.
    pub fn get(&self, id: u64) -> Option<&StagedSend> {
        self.entries.get(&id)
    }

    /// Stage `text` to `user`, optionally pinned to `via_org`.
    ///
    /// Why: acceptance 4 and 8: a send with no route, or through another
    /// org, is refused before anything is recorded.
    /// What: checks the text, resolves the route, records it `Staged` and
    /// returns its id. Posts nothing.
    /// Test: `cross_org_stage_is_refused_and_audited_without_text`,
    /// `no_route_stage_is_denied`.
    pub fn stage(
        &mut self,
        cfg: &OrgsConfig,
        user: &str,
        via_org: Option<&str>,
        text: &str,
        now: Instant,
        audit: &impl SendAudit,
    ) -> Result<u64, SendError> {
        if text.is_empty() || text.len() > MAX_SEND_BYTES {
            audit.record(line(None, "refused", None, "text_invalid"));
            return Err(SendError::TextInvalid);
        }
        let route = match cfg.route(user, via_org) {
            Ok(route) => route,
            Err(e) => {
                audit.record(line(None, "refused", None, e.code()));
                return Err(e.into());
            }
        };
        let id = self.next;
        self.next += 1;
        audit.record(line(
            Some(id),
            "staged",
            Some(&route.org),
            "awaiting_approve",
        ));
        self.entries.insert(
            id,
            StagedSend {
                route,
                digest: digest_of(text),
                text: text.to_owned(),
                staged_at: now,
                state: SendState::Staged,
            },
        );
        Ok(id)
    }

    /// Apply the owner's `decision` to staged send `id`.
    ///
    /// Why: acceptance 1, 2, 3 and 6.
    /// What: a decided id is `AlreadyDecided`; a record past the TTL becomes
    /// `Expired`; `Deny` becomes `Denied`; `FreeText` leaves it `Staged`;
    /// `Approve` with a wrong digest is `DigestMismatch` and stays `Staged`.
    /// A matching Approve moves it to `Sending`, then sends the stored text
    /// once and settles `Sent`, `NotSent` or `Unknown`. Never retries.
    /// Test: `approve_posts_exact_text_once`, `deny_timeout_and_free_text_post_nothing`,
    /// `unknown_outcome_is_never_reissued`, `approve_with_wrong_digest_posts_nothing`.
    pub fn decide(
        &mut self,
        id: u64,
        decision: &Decision,
        now: Instant,
        sender: &impl ChannelSender,
        audit: &impl SendAudit,
    ) -> Result<SendState, SendError> {
        let ttl = self.ttl;
        let entry = self.entries.get_mut(&id).ok_or(SendError::UnknownId)?;
        let org = entry.route.org.clone();
        if entry.state != SendState::Staged {
            audit.record(line(Some(id), "refused", Some(&org), "already_decided"));
            return Err(SendError::AlreadyDecided);
        }
        if now.saturating_duration_since(entry.staged_at) > ttl {
            entry.state = SendState::Expired;
            audit.record(line(Some(id), "expired", Some(&org), "ttl"));
            return Ok(SendState::Expired);
        }
        match decision {
            Decision::Deny => {
                entry.state = SendState::Denied;
                audit.record(line(Some(id), "denied", Some(&org), "owner_deny"));
                return Ok(SendState::Denied);
            }
            Decision::FreeText => {
                audit.record(line(Some(id), "held", Some(&org), "not_an_approve"));
                return Ok(SendState::Staged);
            }
            Decision::Approve { digest } if *digest != entry.digest => {
                audit.record(line(Some(id), "refused", Some(&org), "digest_mismatch"));
                return Err(SendError::DigestMismatch);
            }
            Decision::Approve { .. } => {}
        }
        // #10417 (AC6): `Sending` first, so no later call can post it again.
        entry.state = SendState::Sending;
        audit.record(line(Some(id), "sending", Some(&org), "approved"));
        let state = match sender.send(&entry.route, &entry.text) {
            Ok(()) => SendState::Sent,
            Err(SendFailure::Rejected(code)) => SendState::NotSent(code),
            Err(SendFailure::Unknown(code)) => SendState::Unknown(code),
            // Slack took it; only the thread is unrecorded.
            Err(SendFailure::Unrecorded(_)) => SendState::Sent,
        };
        let (event, code) = match &state {
            SendState::Sent => ("sent", "ok".to_owned()),
            SendState::NotSent(c) => ("not_sent", c.clone()),
            SendState::Unknown(c) => ("unknown", c.clone()),
            _ => ("sent", "ok".to_owned()),
        };
        audit.record(line(Some(id), event, Some(&org), &code));
        entry.state = state.clone();
        Ok(state)
    }
}

/// Resolves a `secret://` credential ref to its value. Errors name no value.
pub trait SecretResolver {
    /// The value `secret_ref` names.
    fn resolve(&self, secret_ref: &str) -> Result<Secret<String>, String>;
}

/// The S1 sender: Slack DMs via `chat.postMessage`, `channel = <member id>`.
///
/// Why: #10417: chat:write only, so no `conversations.open` / im:write; a
/// member id as `channel` opens the bot's DM.
/// What: resolves the org's [`SLACK_BOT_ROLE`] ref, then one post. A Google
/// Chat route is `NotSent` (`gchat_send_is_s4`); a missing ref or a resolver
/// error is `NotSent` (`credential_unavailable`) and posts nothing.
/// Test: `slack_dm_posts_to_the_member_id`, `credential_error_posts_nothing`.
pub struct SlackDmSender<'a, P: SlackPost, R: SecretResolver> {
    /// The Slack poster.
    pub poster: &'a P,
    /// The credential resolver.
    pub resolver: &'a R,
}

impl<P: SlackPost, R: SecretResolver> ChannelSender for SlackDmSender<'_, P, R> {
    fn send(&self, route: &OrgRoute, text: &str) -> Result<(), SendFailure> {
        if route.kind != ChannelKind::Slack {
            return Err(SendFailure::Rejected("gchat_send_is_s4".into()));
        }
        let secret_ref = route
            .credentials
            .get(SLACK_BOT_ROLE)
            .ok_or_else(|| SendFailure::Rejected("credential_unavailable".into()))?;
        // #10417: the resolver's error text is dropped; it may name the store.
        let token = self.resolver.resolve(secret_ref).map_err(|_| {
            tracing::warn!(org = %route.org, "channel send: bot credential unavailable");
            SendFailure::Rejected("credential_unavailable".into())
        })?;
        self.poster.post(&token, &route.address, text).map(|_| ())
    }
}

/// SHA-256 hex digest of `text`; the Approve token for it.
pub fn digest_of(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn line(id: Option<u64>, event: &'static str, org: Option<&str>, code: &str) -> AuditLine {
    AuditLine {
        id,
        event,
        org: org.map(str::to_owned),
        code: code.to_owned(),
    }
}
