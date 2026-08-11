//! Short-lived tickets that authenticate the `/api/events` SSE stream (#5052).
//!
//! Why: `/api/events` streams conversation content — `PmThinking.text`,
//! `AgentMessage.text`, `PmDelegating.task_preview` — and used to be exempt from
//! bearer auth outright, because the browser `EventSource` API cannot attach an
//! `Authorization` header. A loopback bind does not contain that: any page in
//! the operator's browser could open
//! `new EventSource("http://127.0.0.1:7654/api/events")` and read the stream,
//! with no shell access, no token, and no non-loopback opt-in. Removing the
//! exemption alone would break the shipped UI, so the stream needs a credential
//! a `EventSource` URL can carry.
//!
//! A ticket rather than the bearer token itself in `?token=`: the URL is the one
//! part of the request that leaks — into access logs, the `Referer` header, and
//! browser history. A ticket that leaks grants read-only access to the event
//! stream, expiring after 5 minutes idle and after 1 hour absolutely however
//! often it is used; the bearer token that leaks grants the whole `/api/*`
//! write surface, which can spawn arbitrary subprocesses, forever.
//!
//! What: [`EventTicketStore`] mints unguessable ticket strings and redeems them
//! for a sliding TTL window. [`EventStreamAuth`] is the request-time decision:
//! a matching bearer token, or a live ticket, or 401. Minting happens at
//! `POST /api/events/ticket`, which — being a write method — is already behind
//! the router-wide same-origin guard even when no bearer token is configured, so
//! a cross-origin page cannot mint one for itself.
//! Test: `super::tests::event_tickets`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, header};
use base64::Engine as _;
use rand::RngCore as _;
use serde::Serialize;

use super::auth::bearer_token_matches;

/// How long a ticket stays valid, measured from mint OR from its last
/// successful redemption (a sliding window).
///
/// Long enough that the browser's own `EventSource` reconnect after a transient
/// blip re-uses the same URL successfully; short enough that a ticket recovered
/// from an access log after the fact is dead. The UI mints a fresh ticket when a
/// reconnect does fail (`connectEventSource` in `ui/src/lib/transport.ts`), so
/// this value is not an upper bound on stream lifetime.
pub(super) const TICKET_TTL: Duration = Duration::from_secs(300);

/// Hard ceiling on a ticket's total life, regardless of how often it is
/// redeemed. (#5052, critic MEDIUM-1)
///
/// Why: a purely sliding window has a failure mode — a ticket recovered from an
/// access log and then polled every four minutes renews forever, so the
/// "short-lived credential" the whole ticket design rests on quietly becomes a
/// permanent one. The absolute cap makes the bound unconditional.
/// Chosen an hour because it self-heals: when a live client's ticket hits the
/// ceiling its next reconnect 401s, and both shipped clients (the Svelte
/// `connectEventSource` loop and the desktop `sse_bridge`) re-mint on error.
const MAX_TICKET_LIFETIME: Duration = Duration::from_secs(3600);

/// Cap on simultaneously-live tickets, so a client that mints in a loop cannot
/// grow the store without bound. The oldest ticket is evicted past this.
const MAX_LIVE_TICKETS: usize = 32;

/// One minted, not-yet-expired ticket.
struct LiveTicket {
    value: String,
    /// End of the sliding idle window; refreshed on every redemption.
    expires_at: Instant,
    /// End of the absolute lifetime; never moves after mint.
    expires_hard_at: Instant,
}

impl LiveTicket {
    /// Whether this ticket is still usable at `now` — inside BOTH the sliding
    /// idle window and the absolute lifetime.
    fn is_live(&self, now: Instant) -> bool {
        self.expires_at > now && self.expires_hard_at > now
    }
}

/// Body returned by `POST /api/events/ticket`. (#5052)
///
/// `expires_in_secs` is advisory — it lets a client schedule a refresh instead
/// of discovering expiry as a failed reconnect.
#[derive(Debug, Clone, Serialize)]
pub(super) struct EventTicketResponse {
    pub(super) ticket: String,
    pub(super) expires_in_secs: u64,
}

/// In-memory store of live event-stream tickets. (#5052)
///
/// Why: process-local and non-persistent on purpose — a ticket outliving the
/// server that issued it has no legitimate use, and persisting it would put a
/// credential on disk. A `Vec` rather than a `HashMap` because redemption is a
/// constant-time comparison against every entry (see [`Self::redeem`]) and the
/// store is capped at [`MAX_LIVE_TICKETS`].
/// What: [`Self::mint`] issues a 256-bit random ticket; [`Self::redeem`]
/// consumes one for a refreshed TTL window; both prune expired entries first.
/// Test: `event_tickets::*` in `super::tests`.
#[derive(Default)]
pub(super) struct EventTicketStore {
    live: Mutex<Vec<LiveTicket>>,
}

impl EventTicketStore {
    /// Issue a new ticket, valid for [`TICKET_TTL`].
    ///
    /// Why: the caller has already proven it is allowed to read the stream (it
    /// got past the same-origin write guard and, when one is configured, the
    /// bearer-token middleware). This converts that one-shot proof into a
    /// credential the browser can put in an `EventSource` URL.
    /// What: draws 32 bytes from the OS CSPRNG, encodes them URL-safe (so the
    /// value survives a query string unescaped), prunes expired entries, evicts
    /// the oldest if the store is at capacity, and returns the ticket.
    /// Test: `mint_returns_distinct_unguessable_tickets`,
    /// `store_is_capacity_bounded`.
    pub(super) fn mint(&self) -> EventTicketResponse {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        let value = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);

        let now = Instant::now();
        let mut live = self.lock();
        live.retain(|t| t.is_live(now));
        if live.len() >= MAX_LIVE_TICKETS {
            // Evict the entry closest to expiry — the one whose loss costs the
            // least. `min_by_key` over a <= 32-entry Vec is not worth a heap.
            if let Some(idx) = live
                .iter()
                .enumerate()
                .min_by_key(|(_, t)| t.expires_at)
                .map(|(i, _)| i)
            {
                live.remove(idx);
            }
        }
        live.push(LiveTicket {
            value: value.clone(),
            expires_at: now + TICKET_TTL,
            expires_hard_at: now + MAX_TICKET_LIFETIME,
        });

        EventTicketResponse {
            ticket: value,
            expires_in_secs: TICKET_TTL.as_secs(),
        }
    }

    /// Whether `presented` is a live ticket; refreshes its TTL if so.
    ///
    /// Why: a ticket is not single-use because `EventSource` reconnects to the
    /// SAME url after a dropped connection — invalidating on first use would
    /// turn every transient network blip into a permanently dead stream.
    /// Refreshing on redemption instead means an ACTIVELY reconnecting client
    /// keeps working while an abandoned ticket still ages out — bounded by
    /// [`MAX_TICKET_LIFETIME`], which the refresh can never push past.
    /// What: prunes expired entries, then compares `presented` against every
    /// remaining ticket with the same constant-time comparison the bearer token
    /// uses (#3761) — a short-circuiting `==` or a `HashMap` lookup would leak
    /// the matching prefix to an attacker who can time their own requests.
    /// Every entry is compared even after a hit, so the time taken does not
    /// reveal WHICH ticket matched.
    /// Test: `redeem_rejects_unknown_ticket`, `redeem_rejects_expired_ticket`,
    /// `minted_ticket_is_redeemable_more_than_once`,
    /// `sliding_ttl_cannot_outlive_the_absolute_cap`.
    pub(super) fn redeem(&self, presented: &str) -> bool {
        let now = Instant::now();
        let mut live = self.lock();
        live.retain(|t| t.is_live(now));

        let mut hit = None;
        for (idx, ticket) in live.iter().enumerate() {
            if bearer_token_matches(&ticket.value, presented) {
                hit = Some(idx);
            }
        }
        match hit {
            Some(idx) => {
                // #5052 (critic MEDIUM-1): the sliding window may extend the
                // idle deadline but never past the absolute lifetime, so a
                // ticket polled forever still dies at `MAX_TICKET_LIFETIME`.
                let hard = live[idx].expires_hard_at;
                live[idx].expires_at = (now + TICKET_TTL).min(hard);
                true
            }
            None => false,
        }
    }

    /// Lock the store, recovering from a poisoned mutex.
    ///
    /// A panic in one redemption must not permanently 401 every subsequent
    /// stream connection; the guarded data is a plain `Vec` with no invariant a
    /// panic could have broken mid-update.
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<LiveTicket>> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Test-only: number of live (not-yet-pruned) tickets.
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.lock().len()
    }

    /// Test-only: force every live ticket to have already expired, so the
    /// expiry branch is reachable without sleeping for [`TICKET_TTL`].
    #[cfg(test)]
    pub(super) fn expire_all_for_test(&self) {
        let past = Instant::now() - Duration::from_secs(1);
        for t in self.lock().iter_mut() {
            t.expires_at = past;
        }
    }

    /// Test-only: force every live ticket past its ABSOLUTE lifetime while
    /// leaving the sliding idle window wide open, so `MAX_TICKET_LIFETIME` is
    /// reachable without waiting an hour.
    #[cfg(test)]
    pub(super) fn expire_hard_deadline_for_test(&self) {
        let past = Instant::now() - Duration::from_secs(1);
        for t in self.lock().iter_mut() {
            t.expires_hard_at = past;
            t.expires_at = Instant::now() + TICKET_TTL;
        }
    }
}

/// Everything `GET /api/events` needs to decide whether a caller may read the
/// stream. (#5052)
///
/// Why: the SSE route accepts TWO credentials for two different callers — a
/// bearer header for programmatic clients (`curl`, a reverse proxy, the desktop
/// shell's Rust SSE bridge) and a ticket for the browser, which cannot set
/// headers on an `EventSource`. Carrying both in one value, delivered as an
/// axum `Extension`, keeps the decision in one place instead of splitting it
/// between the middleware and the handler.
/// What: `token` is the configured bearer token, if any; `tickets` is the
/// process-wide store. Cloning is cheap — both fields are `Arc`-backed.
/// Test: `super::tests::event_tickets`.
#[derive(Clone)]
pub(super) struct EventStreamAuth {
    tickets: Arc<EventTicketStore>,
    token: Option<Arc<str>>,
}

impl EventStreamAuth {
    /// Build the stream-auth state for a router with the given bearer token.
    pub(super) fn new(token: Option<String>) -> Self {
        Self {
            tickets: Arc::new(EventTicketStore::default()),
            token: token.map(Arc::from),
        }
    }

    /// Mint a ticket (see [`EventTicketStore::mint`]).
    pub(super) fn mint(&self) -> EventTicketResponse {
        self.tickets.mint()
    }

    /// Test-only: the underlying store, so a test can age its tickets without
    /// reaching into the router's request extensions.
    #[cfg(test)]
    pub(super) fn store(&self) -> &EventTicketStore {
        &self.tickets
    }

    /// Whether this request may read the event stream.
    ///
    /// Why: the exact opposite of the pre-#5052 rule, which let EVERY request
    /// through. Note that when no bearer token is configured there is nothing
    /// for the header branch to match, so a ticket is the only way in — that is
    /// deliberate: the shipped default (Tauri sidecar on `127.0.0.1:7654`, no
    /// token) is exactly the configuration the cross-origin `EventSource` attack
    /// targeted, and it must be closed too.
    /// What: accepts an `Authorization: Bearer <t>` header that matches the
    /// configured token, else a live ticket, else rejects.
    /// Test: `event_stream_without_credential_is_rejected`,
    /// `event_stream_without_credential_is_rejected_with_token_configured`,
    /// `minted_ticket_opens_event_stream`, `bearer_token_opens_event_stream`,
    /// `event_stream_with_unknown_ticket_is_rejected`,
    /// `ticket_opens_event_stream_on_a_token_configured_server`.
    pub(super) fn authorize(&self, headers: &HeaderMap, ticket: Option<&str>) -> bool {
        if let Some(expected) = self.token.as_deref()
            && let Some(presented) = bearer_header(headers)
            && bearer_token_matches(expected, presented)
        {
            return true;
        }
        matches!(ticket, Some(t) if self.tickets.redeem(t))
    }
}

/// Extract the `Bearer` credential from an `Authorization` header.
///
/// Mirrors the parsing in `auth::auth_middleware` (same prefix, same trim) so
/// the two paths cannot disagree about what counts as a presented token.
fn bearer_header(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::trim)
}
