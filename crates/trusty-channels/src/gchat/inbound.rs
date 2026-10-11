//! Inbound processing: bind a pulled Chat reply to exactly one open question,
//! learn a DM route's space at bootstrap, and drop everything else with an
//! audit line.
//!
//! Why: a reply must resolve only the question it answers, only from the
//! route's recipient, and only in the route's space (#9448 rulings 4 and 6):
//! the recipient's DM for a DM route, or the route's configured named space
//! (`SPACE` or `GROUP_CHAT`) for a space route, which never takes a DM.
//! Google documents `messageReplyOption` as named-space only, so a DM reply
//! may not thread; the visible `[Q-<n>]` token is the fallback binding.
//! A sender may not flood a question or the audit log (#8454).
//! What: [`GchatChannel::process_batch`] handles each [`PulledMessage`] and
//! returns the ack ids it may acknowledge: a message is acked only after its
//! outcome (answer or audit line) is on disk. [`GchatChannel::poll_once`]
//! pulls, processes and acknowledges one batch. After the route lookup and
//! before any state is touched, a message takes one admit from its route's
//! window, or from the shared unknown-sender window when it has no route
//! ([`LimitBucket`]); an exhausted window drops it.
//! Test: `src/gchat/tests/inbound.rs`, `src/gchat/tests/space_routes.rs`,
//! `src/gchat/tests/rate_limit.rs`.

use crate::gchat::api::error::EventParseError;
use crate::gchat::api::events::{ChatEvent, MessageEvent, PulledMessage};
use crate::gchat::channel::{GchatChannel, Inner};
use crate::gchat::error::{InboundError, StateError};
use crate::gchat::routes::RouteTable;
use crate::gchat::state::audit::{clip, AuditEvent, AuditRecord};
use crate::gchat::state::ledger::{question_tokens, Answer, ResolveOutcome};
use crate::gchat::state::now_rfc3339;
use crate::policy::{BucketDecision, Channel, Clock, RateLimit};

/// What happened to one pulled message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboundOutcome {
    /// The reply resolved this question.
    Answered {
        /// The question id.
        question_id: u64,
        /// The route it arrived on.
        route: String,
    },
    /// Dropped and audit-logged with this reason code.
    Dropped {
        /// e.g. `no_open_question`, `sender_not_recipient`, `sender_is_bot`.
        reason: &'static str,
    },
    /// The payload was not a Chat event; audit-logged.
    Unparseable {
        /// A content-free classification of the parse error.
        reason: String,
    },
    /// A non-MESSAGE event (`ADDED_TO_SPACE`, …), acknowledged unread.
    Ignored {
        /// The event type.
        event_type: String,
    },
    /// Dropped over the rate limit of this bucket (#8454); it resolved no
    /// question and learned no space.
    RateLimited {
        /// The window the message was refused by.
        bucket: LimitBucket,
    },
}

/// The inbound rate-limit window a message is counted in.
///
/// Why: a window is per binding, never per raw sender id, and every sender
/// with no route shares one window (#8454 Q3, #7457).
/// What: `Route` is keyed by route name; `UnknownSender` takes a sender
/// with no email, no route match, or a BOT type.
/// Test: `buckets_are_keyed_by_route_not_sender_id`,
/// `unknown_senders_share_one_bucket`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LimitBucket {
    /// The window of the route whose recipient sent the message.
    Route(String),
    /// The one window every sender with no route shares.
    UnknownSender,
}

impl LimitBucket {
    /// The audit reason code of a drop by this bucket.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Route(_) => "route_rate_limit",
            Self::UnknownSender => "unknown_sender_rate_limit",
        }
    }

    /// The route name, for a route bucket.
    pub fn route(&self) -> Option<&str> {
        match self {
            Self::Route(name) => Some(name),
            Self::UnknownSender => None,
        }
    }
}

/// The result of processing one pulled batch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BatchReport {
    /// One outcome per pulled message, in order.
    pub outcomes: Vec<InboundOutcome>,
    /// Ack ids whose outcome is recorded; safe to acknowledge.
    pub ack_ids: Vec<String>,
    /// Ack ids withheld because their audit line or answer could not be
    /// written; Pub/Sub redelivers them.
    pub withheld: Vec<String>,
    /// Messages dropped over a rate limit in this batch (#8454).
    pub rate_limited: u64,
}

impl GchatChannel {
    /// Process one pulled batch without any network call.
    ///
    /// Why: the testable core of the poller; S2b acknowledges what it returns.
    /// What: per message — a parse error is audit-logged; a non-MESSAGE
    /// event is ignored; a MESSAGE is bound or dropped (see
    /// [`InboundOutcome`]). If the batch is non-empty and every event lacks
    /// `type`, the app delivers the Workspace add-on format: each is
    /// audit-logged, none is acknowledged, and
    /// [`InboundError::AddOnEventFormat`] is returned. A BOT sender is
    /// dropped before route lookup. A message whose answer cannot be written
    /// is withheld, not acknowledged. A message over its rate limit is
    /// dropped, counted in [`BatchReport::rate_limited`], and acked once its
    /// audit line is written (#8454 Q1, Q2).
    /// Test: `unparseable_message_is_audited_before_ack`,
    /// `route_101st_message_in_a_minute_is_dropped_and_acked`,
    /// `add_on_format_batch_is_a_loud_error`,
    /// `reply_resolves_exactly_that_question`,
    /// `bot_sender_is_dropped_before_route_lookup`,
    /// `ledger_write_failure_on_a_reply_withholds_its_ack`.
    pub fn process_batch(&self, pulled: &[PulledMessage]) -> Result<BatchReport, InboundError> {
        let add_on = !pulled.is_empty()
            && pulled
                .iter()
                .all(|m| m.event == Err(EventParseError::MissingField("type")));
        let mut report = BatchReport::default();
        let mut inner = self.lock();
        for msg in pulled {
            let (outcome, recorded) = match &msg.event {
                Err(e) => {
                    let reason = parse_error_code(e);
                    let mut record = AuditRecord::new(AuditEvent::InboundUnparseable, reason);
                    record.message_id = Some(clip(&msg.message_id));
                    let ok = self.audit(&record);
                    let reason = reason.to_string();
                    (InboundOutcome::Unparseable { reason }, ok)
                }
                Ok(ChatEvent::Other { event_type }) => (
                    InboundOutcome::Ignored {
                        event_type: event_type.clone(),
                    },
                    true,
                ),
                Ok(ChatEvent::Message(m)) => match self.handle_message(&mut inner, m) {
                    Ok(Handled::Answered { question_id, route }) => {
                        (InboundOutcome::Answered { question_id, route }, true)
                    }
                    Ok(Handled::Dropped { reason, route }) => {
                        let ok = self.audit(&drop_record(reason, route, m, &msg.message_id));
                        (InboundOutcome::Dropped { reason }, ok)
                    }
                    Ok(Handled::Limited { bucket }) => {
                        // #8454 Q1: acked only once its audit line is on disk.
                        report.rate_limited += 1;
                        let ok = self.audit_limited(&mut inner, &bucket, m, &msg.message_id);
                        (InboundOutcome::RateLimited { bucket }, ok)
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "gchat inbound state write failed");
                        (
                            InboundOutcome::Dropped {
                                reason: "state_write_failed",
                            },
                            false,
                        )
                    }
                },
            };
            if recorded && !add_on {
                report.ack_ids.push(msg.ack_id.clone());
            } else {
                report.withheld.push(msg.ack_id.clone());
            }
            report.outcomes.push(outcome);
        }
        if add_on {
            return Err(InboundError::AddOnEventFormat {
                count: pulled.len(),
            });
        }
        Ok(report)
    }

    /// Pull up to `max` messages, process them, and acknowledge the ones
    /// whose outcome is recorded.
    ///
    /// Why: the poller's one step.
    /// What: refuses when the routes file was refused or has no connection.
    /// An add-on-format batch returns its error before any acknowledge.
    /// Test: `unparseable_message_is_audited_before_ack`,
    /// `add_on_format_batch_is_a_loud_error`.
    pub async fn poll_once(&self, max: u32) -> Result<BatchReport, InboundError> {
        let table = self
            .routes
            .as_ref()
            .map_err(|e| InboundError::NotConfigured {
                reason: e.to_string(),
            })?;
        let subscription = table
            .connection
            .as_ref()
            .map(|c| c.subscription_name())
            .ok_or_else(|| InboundError::NotConfigured {
                reason: "routes file names no [gchat.connection]".into(),
            })?;
        let client = self
            .client()
            .map_err(|reason| InboundError::NotConfigured { reason })?;
        let pulled = client.pull(&subscription, max).await?;
        let report = self.process_batch(&pulled)?;
        client.acknowledge(&subscription, &report.ack_ids).await?;
        Ok(report)
    }

    /// Record a limited drop; true when it may be acknowledged.
    ///
    /// Why: one audit line per bucket per window keeps a flood out of the
    /// audit log (#8454 Q2), while the first drop is still on disk before
    /// its ack (Q1).
    /// What: the first limited drop of `bucket` in a window of
    /// [`RateLimit::DEFAULT`] writes a `rate_limited` line and returns the
    /// write's result; a failed write is not remembered, so the next drop
    /// tries again. A later drop in that window writes nothing and returns
    /// true.
    /// Test: `second_limited_drop_in_a_window_writes_no_audit_line`,
    /// `audit_write_failure_on_rate_limited_withholds_ack`.
    fn audit_limited(
        &self,
        inner: &mut Inner,
        bucket: &LimitBucket,
        m: &MessageEvent,
        message_id: &str,
    ) -> bool {
        let now = inner.limiter.clock().now();
        let window = RateLimit::DEFAULT.window();
        // A backwards reading counts as inside the window: nothing to add.
        if inner
            .limit_audited
            .get(bucket)
            .is_some_and(|at| now.saturating_sub(*at) < window)
        {
            return true;
        }
        let route = bucket.route().map(str::to_string);
        let mut record = drop_record(bucket.reason(), route, m, message_id);
        record.event = AuditEvent::RateLimited;
        let written = self.audit(&record);
        if written {
            inner.limit_audited.insert(bucket.clone(), now);
        }
        written
    }

    /// Bind one MESSAGE event, learning a DM route's space at bootstrap.
    fn handle_message(&self, inner: &mut Inner, m: &MessageEvent) -> Result<Handled, StateError> {
        // #9448 review: a bot never binds a route or answers a question.
        if m.sender
            .user_type
            .as_deref()
            .is_some_and(|t| t.eq_ignore_ascii_case("BOT"))
        {
            return Ok(unknown_sender(inner, "sender_is_bot"));
        }
        let Ok(table) = self.routes.as_ref() else {
            return Ok(unknown_sender(inner, "routes_unavailable"));
        };
        let Some(email) = m.sender.email.as_deref() else {
            return Ok(unknown_sender(inner, "sender_has_no_email"));
        };
        let Some(route) = table.by_recipient(email) else {
            return Ok(unknown_sender(inner, "sender_not_recipient"));
        };
        // #8454: the route's window, keyed by route name, taken before any
        // learned space or ledger entry is read or written.
        match inner
            .limiter
            .take_binding(Channel::Gchat, &route.name, RateLimit::DEFAULT)
        {
            BucketDecision::Admit => {}
            BucketDecision::Exhausted => {
                return Ok(Handled::Limited {
                    bucket: LimitBucket::Route(route.name.clone()),
                });
            }
        }
        let name = Some(route.name.clone());
        let mut learned = false;
        if let Some(configured) = route.space.as_deref() {
            // #9448: a space route binds only in its configured space and
            // never reads or writes a learned binding.
            if let Some(reason) = space_route_refusal(table, configured, m) {
                return Ok(Handled::drop(reason, name));
            }
        } else {
            if !is_direct_message(m) {
                return Ok(Handled::drop("not_direct_message", name));
            }
            match inner.spaces.space_for(&route.name, &route.recipient) {
                Some(space) if space != m.space.name => {
                    return Ok(Handled::drop("space_mismatch", name));
                }
                Some(_) => {}
                None => {
                    if space_bound_elsewhere(table, inner, &route.name, &m.space.name) {
                        return Ok(Handled::drop("space_bound_to_other_route", name));
                    }
                    learned = inner
                        .spaces
                        .learn(&route.name, &route.recipient, &m.space.name)?;
                }
            }
        }
        let thread_match: Vec<u64> = m
            .thread_name
            .as_deref()
            .map(|t| {
                inner
                    .ledger
                    .by_thread(&route.name, t)
                    .map(|q| q.id)
                    .collect()
            })
            .unwrap_or_default();
        let target = if thread_match.len() == 1 {
            thread_match[0]
        } else {
            match question_tokens(&m.text).as_slice() {
                [id] => *id,
                [] if learned => return Ok(Handled::drop("space_learned", name)),
                [] => return Ok(Handled::drop("no_open_question", name)),
                _ => return Ok(Handled::drop("ambiguous_question_token", name)),
            }
        };
        if inner
            .ledger
            .get(target)
            .is_none_or(|q| q.route != route.name)
        {
            return Ok(Handled::drop("no_open_question", name));
        }
        let answer = Answer {
            text: m.text.clone(),
            sender: route.recipient.clone(),
            message_name: m.message_name.clone(),
            answered_at: now_rfc3339(),
        };
        Ok(match inner.ledger.resolve(target, answer)? {
            ResolveOutcome::Resolved => Handled::Answered {
                question_id: target,
                route: route.name.clone(),
            },
            ResolveOutcome::AlreadyResolved => Handled::drop("already_resolved", name),
            ResolveOutcome::Unknown => Handled::drop("no_open_question", name),
        })
    }
}

enum Handled {
    Answered {
        question_id: u64,
        route: String,
    },
    Dropped {
        reason: &'static str,
        route: Option<String>,
    },
    Limited {
        bucket: LimitBucket,
    },
}

impl Handled {
    fn drop(reason: &'static str, route: Option<String>) -> Self {
        Self::Dropped { reason, route }
    }
}

/// Drop a sender with no route with `reason`, after a take on the shared
/// unknown-sender window; an exhausted take makes it a limited drop.
///
/// Why: an unrouted sender's drop writes an audit line, and that path must
/// not be floodable (#8454 Q3, Q6).
/// What: one window for a BOT sender, a sender with no email, a sender no
/// route names, and every sender while the routes file is refused.
/// Test: `unknown_senders_share_one_bucket`,
/// `bot_sender_counts_against_unknown_bucket`.
fn unknown_sender(inner: &mut Inner, reason: &'static str) -> Handled {
    match inner.limiter.take_unknown_sender() {
        BucketDecision::Admit => Handled::drop(reason, None),
        BucketDecision::Exhausted => Handled::Limited {
            bucket: LimitBucket::UnknownSender,
        },
    }
}

/// `DIRECT_MESSAGE` space type, or the deprecated `DM` type.
fn is_direct_message(m: &MessageEvent) -> bool {
    m.space.space_type.as_deref() == Some("DIRECT_MESSAGE")
        || (m.space.space_type.is_none() && m.space.legacy_type.as_deref() == Some("DM"))
}

/// Why a message from a space route's recipient is refused, or `None`.
///
/// Why: a space route accepts a reply only in its configured named space
/// (#9448 owner ruling: one shared Space, no per-person DMs).
/// What: in order — a DM is `direct_message_on_space_route`; a `spaceType`
/// other than `SPACE` or `GROUP_CHAT` (absent included) is
/// `space_type_not_allowed`; a space another route configures is
/// `configured_space_mismatch`; any other space is `space_not_configured`.
/// Test: `dm_from_a_space_route_recipient_is_dropped_and_audited`,
/// `recipient_in_another_routes_space_is_dropped_and_audited`,
/// `message_from_an_unconfigured_space_is_dropped_and_audited`.
fn space_route_refusal(
    table: &RouteTable,
    configured: &str,
    m: &MessageEvent,
) -> Option<&'static str> {
    if is_direct_message(m) {
        return Some("direct_message_on_space_route");
    }
    if !matches!(m.space.space_type.as_deref(), Some("SPACE" | "GROUP_CHAT")) {
        return Some("space_type_not_allowed");
    }
    if m.space.name == configured {
        return None;
    }
    let elsewhere = table
        .routes
        .iter()
        .any(|r| r.space.as_deref() == Some(m.space.name.as_str()));
    Some(if elsewhere {
        "configured_space_mismatch"
    } else {
        "space_not_configured"
    })
}

/// True when another route already holds `space`.
fn space_bound_elsewhere(table: &RouteTable, inner: &Inner, route: &str, space: &str) -> bool {
    table
        .routes
        .iter()
        .filter(|r| r.name != route)
        .any(|r| inner.spaces.space_for(&r.name, &r.recipient) == Some(space))
}

fn drop_record(
    reason: &'static str,
    route: Option<String>,
    m: &MessageEvent,
    message_id: &str,
) -> AuditRecord {
    let mut record = AuditRecord::new(AuditEvent::InboundDropped, reason);
    record.route = route;
    record.sender = Some(clip(m.sender.email.as_deref().unwrap_or(&m.sender.name)));
    record.space = Some(clip(&m.space.name));
    record.message_id = Some(clip(message_id));
    record.length = Some(m.text.len());
    record
}

/// A content-free code: a JSON error message can quote the payload.
fn parse_error_code(e: &EventParseError) -> &'static str {
    match e {
        EventParseError::Base64 => "unparseable_base64",
        EventParseError::Json(_) => "unparseable_json",
        EventParseError::MissingField("type") => "missing_event_type",
        EventParseError::MissingField(_) => "missing_event_field",
    }
}
