//! Typed errors for the Google Chat route layer: route loading, the egress
//! gate, the state files and inbound processing.
//!
//! Why: S2b's `gchat-mcp` binary and its doctor rows branch on the failure
//! class — a refused routes file is an operator fix, a refused send names the
//! rule it broke, a state-file error is local I/O (#9448).
//! What: [`RouteError`] (load), [`SendError`] (egress), [`StateError`] (the
//! `state/` files), [`InboundError`] (one pulled batch). No variant carries
//! message text, a key or a token.
//! Test: `src/gchat/tests/routes_load.rs`, `src/gchat/tests/egress.rs`,
//! `src/gchat/tests/inbound.rs`.

use std::path::PathBuf;

use crate::gchat::api::error::GchatError;
use crate::policy::GateError;

/// Why `routes.toml` was refused. Any of these refuses every send.
///
/// Why: each load rule (#9448 ruling 2) and the load gate (E1) is a distinct,
/// matchable failure.
/// What: duplicate-name and duplicate-recipient errors name both entries.
/// Test: `load_rules_refuse_each_invalid_file`, `duplicates_name_both_entries`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RouteError {
    /// The file exists but could not be read.
    #[error("cannot read {path}: {reason}")]
    Read {
        /// The routes-file path.
        path: PathBuf,
        /// The I/O error kind.
        reason: String,
    },
    /// The file is not valid TOML for schema version 1, including an unknown
    /// key at any level.
    #[error("invalid routes file {path}: {reason}")]
    Parse {
        /// The routes-file path.
        path: PathBuf,
        /// The TOML parser's message.
        reason: String,
    },
    /// `version` is not 1.
    #[error("routes file version {found} is not supported; expected 1")]
    Version {
        /// The version the file declares.
        found: i64,
    },
    /// One route or the connection table breaks a load rule.
    #[error("{entry}: {reason}")]
    Invalid {
        /// Which entry, e.g. `gchat.routes[1] "janet"` or `gchat.connection`.
        entry: String,
        /// What is wrong.
        reason: String,
    },
    /// Two routes share a name or a recipient.
    #[error("{first} and {second} share the {field} {value:?}")]
    Duplicate {
        /// The earlier entry.
        first: String,
        /// The later entry.
        second: String,
        /// `name` or `recipient`.
        field: &'static str,
        /// The shared value.
        value: String,
    },
    /// The load gate refused the file (#9448 E1, #8454 Db1/G2/G3).
    ///
    /// Why: #8454 Q4: a neutral variant, since the gate refuses for more
    /// than an uncommitted edit (wrong branch, detached `HEAD`, unknown
    /// default branch, git failure).
    /// What: `reason` is the gate's typed refusal; its text names the rule.
    /// Test: `feature_branch_refuses_load_and_every_send`,
    /// `untracked_and_dirty_arms_refuse_load_and_every_send`.
    #[error("routes file {path} refused by the load gate: {reason}")]
    Gate {
        /// The routes-file path.
        path: PathBuf,
        /// Which gate rule refused the file.
        reason: GateError,
    },
}

/// Why a send was refused or failed.
///
/// Why: the egress gate (ruling 7) refuses before any network call; the
/// caller needs to know which rule refused it.
/// What: every variant up to `SpaceNotLearned` is a refusal raised before any
/// token mint or request, and each is written to `audit.jsonl`.
/// Test: `unknown_recipient_is_refused_with_no_request_and_an_audit_line`,
/// `kind_not_allowed_is_refused_with_no_request`,
/// `review_notice_without_https_url_is_refused`,
/// `unlearned_space_is_refused_without_fallback`.
#[derive(Debug, thiserror::Error)]
pub enum SendError {
    /// `routes.toml` was refused at load, so every send is refused.
    #[error("routes are unavailable, every send is refused: {reason}")]
    RoutesUnavailable {
        /// The load error.
        reason: String,
    },
    /// No route names this recipient or route name.
    #[error("no route names {to:?}")]
    NoRoute {
        /// The requested route name or recipient.
        to: String,
    },
    /// The route exists but does not allow this kind.
    #[error("route {route:?} does not allow kind {kind}")]
    KindNotAllowed {
        /// The route name.
        route: String,
        /// The refused kind.
        kind: &'static str,
    },
    /// A review notice without an https URL (E3).
    #[error("a review notice needs an https URL: {reason}")]
    InvalidReviewUrl {
        /// What is wrong with the URL.
        reason: String,
    },
    /// The message text is empty or over the size limit.
    #[error("message text is invalid: {reason}")]
    InvalidText {
        /// What is wrong with the text.
        reason: &'static str,
    },
    /// The route's DM space is not learned yet; there is no fallback space.
    #[error("route {route:?} has no DM space yet: the recipient must message the Chat app first")]
    SpaceNotLearned {
        /// The route name.
        route: String,
    },
    /// The Chat client could not be built (key file or connection).
    #[error("Chat client unavailable: {reason}")]
    ClientUnavailable {
        /// The redacted construction error.
        reason: String,
    },
    /// The Chat API call failed after the route check passed.
    #[error(transparent)]
    Chat(#[from] GchatError),
    /// A state file could not be written; nothing was posted.
    #[error(transparent)]
    State(#[from] StateError),
    /// The question was posted, but its open record could not be written.
    /// It is sent: do not resend. A reply to it cannot be bound.
    #[error(
        "question [Q-{id}] was posted as {message_name} but could not be recorded \
         (do not resend): {source}"
    )]
    SentNotRecorded {
        /// The question id in the posted text.
        id: u64,
        /// The posted message's name.
        message_name: String,
        /// The ledger write error.
        source: StateError,
    },
}

impl SendError {
    /// The audit `reason` code for a refusal, or `None` for a failure that
    /// happened after the route check passed.
    pub(crate) fn refusal_code(&self) -> Option<&'static str> {
        match self {
            Self::RoutesUnavailable { .. } => Some("routes_unavailable"),
            Self::NoRoute { .. } => Some("no_route"),
            Self::KindNotAllowed { .. } => Some("kind_not_allowed"),
            Self::InvalidReviewUrl { .. } => Some("invalid_review_url"),
            Self::InvalidText { .. } => Some("invalid_text"),
            Self::SpaceNotLearned { .. } => Some("space_not_learned"),
            Self::ClientUnavailable { .. }
            | Self::Chat(_)
            | Self::State(_)
            | Self::SentNotRecorded { .. } => None,
        }
    }
}

/// A `state/` file could not be read or written.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StateError {
    /// I/O on a state file failed.
    #[error("state file {path}: {reason}")]
    Io {
        /// The state-file path.
        path: PathBuf,
        /// The I/O error kind.
        reason: String,
    },
    /// A state file holds content this layer cannot read.
    #[error("state file {path} is corrupt at line {line}: {reason}")]
    Corrupt {
        /// The state-file path.
        path: PathBuf,
        /// 1-based line (0 for a whole-file JSON document).
        line: usize,
        /// The decode error class.
        reason: String,
    },
    /// Another `GchatChannel` holds this state directory's single-writer
    /// lock, in this process or another.
    #[error("state directory is in use: another gchat channel holds {path}")]
    Locked {
        /// The lock-file path.
        path: PathBuf,
    },
    /// Another `gchat-mcp`, from any project dir, already polls this Pub/Sub
    /// subscription (#9448: one consumer per subscription).
    #[error(
        "another gchat-mcp already serves subscription {subscription}: the lock {path} is \
         held. Only one server may poll a subscription; stop the other one first."
    )]
    SubscriptionInUse {
        /// The subscription resource name.
        subscription: String,
        /// The user-level lock-file path.
        path: PathBuf,
    },
}

impl StateError {
    pub(crate) fn io(path: &std::path::Path, e: &std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            reason: e.kind().to_string(),
        }
    }
}

/// Why one pulled batch could not be fully processed.
#[derive(Debug, thiserror::Error)]
pub enum InboundError {
    /// Every event in the batch lacks `type`: the Chat app delivers the
    /// Workspace add-on event format, which this layer does not read.
    #[error(
        "all {count} pulled event(s) lack a `type` field: the Chat app is configured as a \
         Workspace add-on. In the Google Cloud console, Chat API > Configuration, clear \
         \"Build this Chat app as a Workspace add-on\" so Chat sends interaction events. \
         The messages are audit-logged and left unacknowledged."
    )]
    AddOnEventFormat {
        /// How many events failed this way.
        count: usize,
    },
    /// The routes file was refused or has no connection, so there is
    /// nothing to pull.
    #[error("cannot poll: {reason}")]
    NotConfigured {
        /// Why.
        reason: String,
    },
    /// Pull or acknowledge failed.
    #[error(transparent)]
    Chat(#[from] GchatError),
    /// A state file could not be written.
    #[error(transparent)]
    State(#[from] StateError),
}
