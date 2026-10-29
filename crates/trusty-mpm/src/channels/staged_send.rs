//! Staged outbound sends and the owner's Approve gate (#10417, epic #10416 S1).
//!
//! Why: Dd9: every outward send waits for the owner's Approve on the exact
//! text. This is a Fail-Open Check site: no refusal, timeout, credential
//! fault, audit or persistence failure, unauthenticated approver or unknown
//! outcome may post, and a send whose outcome is unknown is never reissued.
//! What: [`StagedSends::stage`] resolves the recipient's org route
//! ([`crate::channels::orgs::OrgsConfig::route`]) and records the text with
//! its SHA-256 digest. [`StagedSends::decide`] takes the owner's
//! [`Decision`] under an [`OwnerApproval`] (#10423: only the configured owner
//! identity): only `Approve` carrying the staged digest, inside the TTL, on a
//! still-`Staged` record posts, exactly once, through a [`ChannelSender`].
//! The state moves to `Sending` and is persisted ([`SendStore`]) before the
//! send, so no later call, and no restart, can post it again. A gating
//! [`SendAudit`] write that fails refuses the step (#10423). [`SlackDmSender`]
//! posts with `chat.postMessage` and `channel = <member id>` (chat:write
//! only, no `conversations.open`). Every step writes one [`AuditLine`] that
//! holds no message text and no credential value.
//! Test: `staged_send_tests.rs`, `send_store_tests.rs`.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
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

/// The code an unknown outcome gets when the daemon restarted mid-send.
pub const RESTART_DURING_SEND: &str = "restart_during_send";

/// The code a `Staged` record gets when the daemon restarted before a
/// decision was persisted (#10423).
pub const RESTART_WHILE_STAGED: &str = "restart_while_staged";

/// The state of one staged send. Every state but `Staged` is final.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

/// Proof that a decision comes from the configured owner (#10423).
///
/// Why: S1a matched only the digest, so anyone who saw the approval DM could
/// approve. Only the owner identity may decide.
/// What: built only by [`OwnerApproval::authenticate`], from an identity an
/// authenticated transport reports (a signed Slack event's `team` and
/// `user`), never from a field a caller writes.
/// Test: `unauthenticated_approve_posts_nothing`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerApproval {
    org: String,
    user: String,
}

impl OwnerApproval {
    /// Accept `(org, user)` only when it is the config's owner.
    ///
    /// What: `ApproverUnauthenticated` when no owner is configured or the
    /// identity differs in org or user.
    /// Test: `unauthenticated_approve_posts_nothing`.
    pub fn authenticate(cfg: &OrgsConfig, org: &str, user: &str) -> Result<Self, SendError> {
        match &cfg.owner {
            Some(o) if o.slack_org == org && o.slack_user == user => Ok(Self {
                org: org.to_owned(),
                user: user.to_owned(),
            }),
            _ => Err(SendError::ApproverUnauthenticated),
        }
    }

    /// The authenticated `(org, user)`.
    pub fn identity(&self) -> (&str, &str) {
        (&self.org, &self.user)
    }
}

/// One audit line. It never carries message text or a credential value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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

/// An audit or store write failed; its text names no value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct WriteFailed(pub String);

/// Where audit lines go. The daemon's appends to `channels/send_audit.jsonl`.
pub trait SendAudit {
    /// Record `line`; an `Err` refuses a gating step (#10423).
    fn record(&self, line: AuditLine) -> Result<(), WriteFailed>;
}

/// Where the table is persisted. The daemon's writes `staged_sends.json`.
pub trait SendStore {
    /// Persist the whole `table`; an `Err` refuses the step it guards.
    fn save(&self, table: &StagedSends) -> Result<(), WriteFailed>;
}

/// The audit and store a stage or decision writes through.
pub struct SendIo<'a> {
    /// The audit sink.
    pub audit: &'a dyn SendAudit,
    /// The table's persistence.
    pub store: &'a dyn SendStore,
}

/// Posts one approved text to one route. Blocking.
pub trait ChannelSender {
    /// Send `text` to `route`'s address on its channel.
    fn send(&self, route: &OrgRoute, text: &str) -> Result<(), SendFailure>;
}

/// One staged send.
#[derive(Clone, Serialize, Deserialize)]
pub struct StagedSend {
    /// The route resolved at stage time.
    pub route: OrgRoute,
    /// The configured user name the route was resolved for (#10423).
    #[serde(default)]
    pub user: String,
    text: String,
    /// SHA-256 hex digest of the text; Approve must carry it.
    pub digest: String,
    /// Unix seconds when it was staged.
    pub staged_at: u64,
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

impl StagedSend {
    /// The staged text, for the owner's approval request only.
    pub fn text(&self) -> &str {
        &self.text
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
    /// The decision did not come from the configured owner.
    #[error("approver_unauthenticated: only the configured owner may decide")]
    ApproverUnauthenticated,
    /// A gating audit line could not be written; nothing was posted.
    #[error("audit_unavailable: {0}")]
    Audit(WriteFailed),
    /// The table could not be persisted; nothing was posted.
    #[error("persist_failed: {0}")]
    Persist(WriteFailed),
    /// The current config routes the user differently than at stage time.
    #[error("route_changed: the org route changed since the send was staged")]
    RouteChanged,
}

impl SendError {
    /// The content-free rule code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Route(e) => e.code(),
            Self::TextInvalid => "text_invalid",
            Self::UnknownId => "unknown_id",
            Self::AlreadyDecided => "already_decided",
            Self::DigestMismatch => "digest_mismatch",
            Self::ApproverUnauthenticated => "approver_unauthenticated",
            Self::Audit(_) => "audit_unavailable",
            Self::Persist(_) => "persist_failed",
            Self::RouteChanged => "route_changed",
        }
    }
}

/// The table of staged sends.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

    /// A table read back from disk after a restart.
    ///
    /// Why: #10423: a send that was `Sending` when the daemon stopped may
    /// have posted; it must never post again. A `Staged` record on disk may
    /// hide a Deny or an expiry whose save failed, so it is never approvable.
    /// What: every `Sending` record becomes `Unknown(restart_during_send)`;
    /// every `Staged` record becomes `NotSent(restart_while_staged)`; final
    /// records stay as they are.
    /// Test: `restart_turns_sending_into_unknown_and_never_reposts`,
    /// `unpersisted_deny_is_an_error_and_never_approvable_after_reload`.
    pub fn restored(mut self) -> Self {
        for entry in self.entries.values_mut() {
            match entry.state {
                SendState::Sending => {
                    entry.state = SendState::Unknown(RESTART_DURING_SEND.to_owned());
                }
                SendState::Staged => {
                    entry.state = SendState::NotSent(RESTART_WHILE_STAGED.to_owned());
                }
                _ => {}
            }
        }
        self
    }

    /// Refuse an Approve whose staged route differs from the current config.
    ///
    /// Why: #10423: the bot token must resolve under the org's current
    /// `secrets` scope and credential refs, not the ones saved at stage time.
    /// What: for a `Staged` record, rebuilds the route with
    /// `cfg.route(user, Some(org))`; a different org, address, kind, scope or
    /// ref set, or no route at all, is `RouteChanged` with a `refused` audit
    /// line. A decided record passes, so `decide` reports it.
    /// Test: `approve_after_scope_change_is_refused_and_posts_nothing`.
    pub fn check_route(&self, id: u64, cfg: &OrgsConfig, io: &SendIo<'_>) -> Result<(), SendError> {
        let entry = self.entries.get(&id).ok_or(SendError::UnknownId)?;
        if entry.state != SendState::Staged {
            return Ok(());
        }
        let current = cfg.route(&entry.user, Some(&entry.route.org)).ok();
        if current.as_ref() != Some(&entry.route) {
            let org = Some(entry.route.org.as_str());
            note(io, line(Some(id), "refused", org, "route_changed"));
            return Err(SendError::RouteChanged);
        }
        Ok(())
    }

    /// The staged send `id`.
    pub fn get(&self, id: u64) -> Option<&StagedSend> {
        self.entries.get(&id)
    }

    /// Stage `text` to `user`, optionally pinned to `via_org`.
    ///
    /// Why: acceptance 4 and 8: a send with no route, or through another
    /// org, is refused before anything is recorded. #10423: a record that
    /// cannot be audited or persisted is not staged.
    /// What: checks the text, resolves the route, writes the `staged` audit
    /// line, records it `Staged` and persists the table; returns its id.
    /// Posts nothing.
    /// Test: `cross_org_stage_is_refused_and_audited_without_text`,
    /// `no_route_stage_is_denied`, `audit_failure_refuses_the_stage`.
    pub fn stage(
        &mut self,
        cfg: &OrgsConfig,
        user: &str,
        via_org: Option<&str>,
        text: &str,
        now: u64,
        io: &SendIo<'_>,
    ) -> Result<u64, SendError> {
        if text.is_empty() || text.len() > MAX_SEND_BYTES {
            note(io, line(None, "refused", None, "text_invalid"));
            return Err(SendError::TextInvalid);
        }
        let route = match cfg.route(user, via_org) {
            Ok(route) => route,
            Err(e) => {
                note(io, line(None, "refused", None, e.code()));
                return Err(e.into());
            }
        };
        let id = self.next;
        let staged = line(Some(id), "staged", Some(&route.org), "awaiting_approve");
        io.audit.record(staged).map_err(SendError::Audit)?;
        self.next += 1;
        self.entries.insert(
            id,
            StagedSend {
                route,
                user: user.to_owned(),
                digest: digest_of(text),
                text: text.to_owned(),
                staged_at: now,
                state: SendState::Staged,
            },
        );
        if let Err(e) = io.store.save(self) {
            // #10423: an unpersisted stage is withdrawn; it cannot be approved.
            self.entries.remove(&id);
            return Err(SendError::Persist(e));
        }
        Ok(id)
    }

    /// Apply the owner's `decision` to staged send `id`.
    ///
    /// Why: acceptance 1, 2, 3 and 6; #10423 owner-only, fail-closed audit
    /// and persistence.
    /// What: a decided id is `AlreadyDecided`; a record past the TTL becomes
    /// `Expired`; `Deny` becomes `Denied`; `FreeText` leaves it `Staged`;
    /// `Approve` with a wrong digest is `DigestMismatch` and stays `Staged`.
    /// A `Denied` or `Expired` state whose save fails stays final in memory
    /// and returns `Persist`.
    /// A matching Approve writes the `sending` audit line and persists
    /// `Sending`; either failing leaves it `Staged` and posts nothing. Then it
    /// sends the stored text once and settles `Sent`, `NotSent` or `Unknown`.
    /// Never retries.
    /// Test: `approve_posts_exact_text_once`, `deny_timeout_and_free_text_post_nothing`,
    /// `unknown_outcome_is_never_reissued`, `approve_with_wrong_digest_posts_nothing`,
    /// `audit_failure_before_send_posts_nothing`, `persist_failure_before_send_posts_nothing`,
    /// `unpersisted_deny_is_an_error_and_never_approvable_after_reload`.
    pub fn decide(
        &mut self,
        id: u64,
        _approver: &OwnerApproval,
        decision: &Decision,
        now: u64,
        sender: &impl ChannelSender,
        io: &SendIo<'_>,
    ) -> Result<SendState, SendError> {
        let ttl = self.ttl.as_secs();
        let entry = self.entries.get_mut(&id).ok_or(SendError::UnknownId)?;
        let org = entry.route.org.clone();
        if entry.state != SendState::Staged {
            note(io, line(Some(id), "refused", Some(&org), "already_decided"));
            return Err(SendError::AlreadyDecided);
        }
        let settled = if now.saturating_sub(entry.staged_at) > ttl {
            Some((SendState::Expired, "expired", "ttl"))
        } else {
            match decision {
                Decision::Deny => Some((SendState::Denied, "denied", "owner_deny")),
                Decision::FreeText => {
                    note(io, line(Some(id), "held", Some(&org), "not_an_approve"));
                    return Ok(SendState::Staged);
                }
                Decision::Approve { digest } if *digest != entry.digest => {
                    note(io, line(Some(id), "refused", Some(&org), "digest_mismatch"));
                    return Err(SendError::DigestMismatch);
                }
                Decision::Approve { .. } => None,
            }
        };
        if let Some((state, event, code)) = settled {
            entry.state = state.clone();
            note(io, line(Some(id), event, Some(&org), code));
            // #10423: an unpersisted final state is an error; the in-memory
            // state stays final and a restart never restores it approvable.
            io.store.save(self).map_err(SendError::Persist)?;
            return Ok(state);
        }
        // #10423: the gating audit line first; no line, no post.
        io.audit
            .record(line(Some(id), "sending", Some(&org), "approved"))
            .map_err(SendError::Audit)?;
        // #10417 (AC6) / #10423: `Sending` persisted first, so neither a
        // later call nor a restart can post it again.
        entry.state = SendState::Sending;
        if let Err(e) = io.store.save(self) {
            if let Some(entry) = self.entries.get_mut(&id) {
                entry.state = SendState::Staged;
            }
            return Err(SendError::Persist(e));
        }
        let entry = self.entries.get_mut(&id).ok_or(SendError::UnknownId)?;
        let state = match sender.send(&entry.route, &entry.text) {
            Ok(()) => SendState::Sent,
            Err(SendFailure::Rejected(code)) => SendState::NotSent(code),
            Err(SendFailure::Unknown(code)) => SendState::Unknown(code),
            // Slack took it; only the thread is unrecorded.
            Err(SendFailure::Unrecorded(_)) => SendState::Sent,
        };
        let (event, code) = match &state {
            SendState::NotSent(c) => ("not_sent", c.clone()),
            SendState::Unknown(c) => ("unknown", c.clone()),
            _ => ("sent", "ok".to_owned()),
        };
        entry.state = state.clone();
        note(io, line(Some(id), event, Some(&org), &code));
        // A failed save leaves `Sending` on disk, which restores as Unknown.
        save_or_warn(io, self);
        Ok(state)
    }
}

/// Write a non-gating audit line; a failure is logged, content-free.
fn note(io: &SendIo<'_>, l: AuditLine) {
    if io.audit.record(l).is_err() {
        tracing::warn!("channel send: a non-gating audit line could not be written");
    }
}

/// Persist after a step that posts nothing more; a failure is logged.
fn save_or_warn(io: &SendIo<'_>, table: &StagedSends) {
    if io.store.save(table).is_err() {
        tracing::warn!("channel send: the staged-send table could not be persisted");
    }
}

/// Resolves a `secret://` credential ref to its value. Errors name no value.
pub trait SecretResolver {
    /// The value `secret_ref` names, within the org lookup order `scope`.
    fn resolve(&self, scope: Option<&str>, secret_ref: &str) -> Result<Secret<String>, String>;
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
        let token = self
            .resolver
            .resolve(route.secrets.as_deref(), secret_ref)
            .map_err(|_| {
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
