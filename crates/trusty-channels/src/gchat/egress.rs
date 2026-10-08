//! The egress gate: every outbound Chat message passes the route check
//! before any token mint or network call (#9448 ruling 7).
//!
//! Why: a session may message only a recipient a reviewed route names, and
//! only with a kind that route allows. A review notice must carry an https
//! link (E3); a question carries a visible `[Q-<n>]` token and a thread key
//! derived from its id so a reply can bind back (D5).
//! What: [`GchatChannel::check_egress`] is the pure route check.
//! [`GchatChannel::send_question`] and [`GchatChannel::send_review_notice`]
//! run it, then the DM-space lookup (no fallback space), then the text
//! checks, and only then call the crate-private `create_message`. Every
//! refusal is written to `audit.jsonl` without the message text.
//! Test: `src/gchat/tests/egress.rs`.

use crate::gchat::api::client::CreateMessage;
use crate::gchat::channel::GchatChannel;
use crate::gchat::error::SendError;
use crate::gchat::routes::{MessageKind, Route};
use crate::gchat::state::audit::{clip, AuditEvent, AuditRecord};
use crate::gchat::state::ledger::{question_token, Question};
use crate::gchat::state::now_rfc3339;

/// The largest caller text accepted, in bytes. Chat caps a message at
/// 32,000 bytes; the rest is headroom for the token and the URL.
pub const MAX_TEXT_BYTES: usize = 30_000;

/// A posted question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentQuestion {
    /// The question id.
    pub id: u64,
    /// The visible token, `[Q-<id>]`.
    pub token: String,
    /// The route it was sent on.
    pub route: String,
    /// The posted message's name.
    pub message_name: String,
    /// The thread name Chat returned, if any.
    pub thread_name: Option<String>,
}

/// A posted review notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentNotice {
    /// The route it was sent on.
    pub route: String,
    /// The posted message's name.
    pub message_name: String,
}

impl GchatChannel {
    /// The route check: may a `kind` message go to `to`?
    ///
    /// Why: the one rule every send passes first; public so S2b and tests
    /// can probe it without sending.
    /// What: `to` is a route name, or a recipient email when it holds `@`.
    /// Refuses when the routes file was refused, when no route matches, or
    /// when the route does not allow `kind`. Pure: no I/O, no audit line.
    /// Test: `egress_per_route_allows_exactly_its_kinds`.
    pub fn check_egress(&self, kind: MessageKind, to: &str) -> Result<Route, SendError> {
        let table = self.table()?;
        let route = table
            .find(to)
            .ok_or_else(|| SendError::NoRoute { to: to.to_string() })?;
        if !route.allows(kind) {
            return Err(SendError::KindNotAllowed {
                route: route.name.clone(),
                kind: kind.as_str(),
            });
        }
        Ok(route.clone())
    }

    /// Post a question to the route `to` names.
    ///
    /// Why: the outbound half of the question/answer loop.
    /// What: route check, learned DM space, text check; then reserves the
    /// next id on disk, posts `[Q-<id>] <text>` with thread key
    /// `trusty-q-<id>-<unix ms>`, and records the question open with the
    /// thread name Chat returned. A failed post leaves the id reserved and
    /// no question open. A post that succeeds when the open record cannot be
    /// written returns [`SendError::SentNotRecorded`]: it was sent, so the
    /// caller must not resend.
    /// Test: `reply_resolves_exactly_that_question`,
    /// `kind_not_allowed_is_refused_with_no_request`,
    /// `post_then_ledger_write_failure_reports_sent_not_recorded`.
    pub async fn send_question(&self, to: &str, text: &str) -> Result<SentQuestion, SendError> {
        let kind = MessageKind::Question;
        let (route, space) = self.authorize(kind, to, text, None)?;
        let client = self
            .client()
            .map_err(|reason| SendError::ClientUnavailable { reason })?;
        let id = self.lock().ledger.reserve()?;
        let token = question_token(id);
        let thread_key = format!(
            "trusty-q-{id}-{}",
            chrono::Utc::now().timestamp_millis().max(0)
        );
        let sent = client
            .create_message(&CreateMessage {
                space: space.clone(),
                text: format!("{token} {text}"),
                thread_key: Some(thread_key.clone()),
                ..CreateMessage::default()
            })
            .await?;
        let thread_name = sent.thread.map(|t| t.name).filter(|n| !n.is_empty());
        let recorded = self.lock().ledger.record_open(Question {
            id,
            route: route.name.clone(),
            recipient: route.recipient.clone(),
            space,
            thread_key,
            thread_name: thread_name.clone(),
            message_name: sent.name.clone(),
            opened_at: now_rfc3339(),
            answer: None,
        });
        // #9448 review: a posted question is never reported as "not sent".
        if let Err(source) = recorded {
            tracing::error!(id, error = %source, "gchat question posted but not recorded");
            return Err(SendError::SentNotRecorded {
                id,
                message_name: sent.name,
                source,
            });
        }
        Ok(SentQuestion {
            id,
            token,
            route: route.name,
            message_name: sent.name,
            thread_name,
        })
    }

    /// Post a notice that a ticket or task waits for review.
    ///
    /// Why: the second message kind; it expects no reply (E3).
    /// What: route check, https URL check, learned DM space, text check;
    /// then posts `<text>\n<url>` with no thread key. Opens no question.
    /// Test: `review_notice_without_https_url_is_refused`,
    /// `valid_review_notice_is_sent_and_opens_no_question`.
    pub async fn send_review_notice(
        &self,
        to: &str,
        text: &str,
        url: &str,
    ) -> Result<SentNotice, SendError> {
        let kind = MessageKind::ReviewNotice;
        let (route, space) = self.authorize(kind, to, text, Some(url))?;
        let client = self
            .client()
            .map_err(|reason| SendError::ClientUnavailable { reason })?;
        let sent = client
            .create_message(&CreateMessage {
                space,
                text: format!("{text}\n{}", url.trim()),
                ..CreateMessage::default()
            })
            .await?;
        Ok(SentNotice {
            route: route.name,
            message_name: sent.name,
        })
    }

    /// Every pre-network check, in order; a refusal is audited.
    fn authorize(
        &self,
        kind: MessageKind,
        to: &str,
        text: &str,
        url: Option<&str>,
    ) -> Result<(Route, String), SendError> {
        let result = self.check_egress(kind, to).and_then(|route| {
            if let Some(url) = url {
                check_review_url(url)?;
            }
            let space = self
                .lock()
                .spaces
                .space_for(&route.name, &route.recipient)
                .map(str::to_string)
                .ok_or_else(|| SendError::SpaceNotLearned {
                    route: route.name.clone(),
                })?;
            check_text(text)?;
            Ok((route, space))
        });
        if let Err(e) = &result {
            if let Some(reason) = e.refusal_code() {
                let mut record = AuditRecord::new(AuditEvent::SendRefused, reason);
                record.kind = Some(kind.as_str());
                record.to = Some(clip(to));
                record.route = self
                    .routes
                    .as_ref()
                    .ok()
                    .and_then(|t| t.find(to))
                    .map(|r| r.name.clone());
                record.length = Some(text.len());
                self.audit(&record);
            }
        }
        result
    }
}

/// Accept only an absolute `https` URL with a host (E3).
fn check_review_url(raw: &str) -> Result<(), SendError> {
    let refuse = |reason: &str| SendError::InvalidReviewUrl {
        reason: reason.to_string(),
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(refuse("no URL given"));
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Err(refuse("the URL contains whitespace"));
    }
    let parsed = url::Url::parse(trimmed).map_err(|_| refuse("not an absolute URL"))?;
    if parsed.scheme() != "https" {
        return Err(refuse("the scheme is not https"));
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(refuse("the URL has no host"));
    }
    Ok(())
}

fn check_text(text: &str) -> Result<(), SendError> {
    if text.trim().is_empty() {
        return Err(SendError::InvalidText {
            reason: "text is empty",
        });
    }
    if text.len() > MAX_TEXT_BYTES {
        return Err(SendError::InvalidText {
            reason: "text exceeds 30000 bytes",
        });
    }
    Ok(())
}
