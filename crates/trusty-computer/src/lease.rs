//! The one desktop lease: one holder session, a FIFO queue behind it.
//!
//! Why: one daemon drives one window at a time, and 17+ agent sessions may
//! want it (plan section 4, "One desktop lease"; brief risk 3).
//! What: [`DesktopLease::connect`] makes a session the holder when the lease
//! is free, else queues it once (FIFO). The holder keeps it until
//! [`DesktopLease::release`], [`DesktopLease::disconnect`] or [`IDLE_TTL`]
//! seconds without a call ([`DesktopLease::expire`]). Each change that moves
//! the lease returns the new holder so the broker can rescope. Time is passed
//! in, so tests drive the clock.
//! Test: `tests/broker_lease.rs`.

use std::collections::VecDeque;

/// Seconds without a call before the holder loses the lease (a confirmed click takes ~3.2 s).
pub const IDLE_TTL: f64 = 120.0;

/// A session waiting for, or holding, the lease with one grant.
#[derive(Debug, Clone, PartialEq)]
pub struct Claim {
    pub session: String,
    pub grant: String,
    pub last_call: f64,
}

/// The answer to `connect`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Connect {
    /// The session holds the lease.
    Held,
    /// The session waits; `position` 1 is next.
    Queued { position: usize },
}

/// Why a lease operation was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LeaseError {
    /// The session holds the lease with another grant; release first.
    #[error("lease_grant_mismatch: release before connecting with another grant")]
    GrantMismatch,
    /// The session does not hold the lease.
    #[error("lease_not_held: {0}")]
    NotHeld(String),
}

impl LeaseError {
    /// Stable reply code.
    pub fn code(&self) -> &'static str {
        match self {
            LeaseError::GrantMismatch => "lease_grant_mismatch",
            LeaseError::NotHeld(_) => "lease_not_held",
        }
    }
}

/// The lease and its queue.
#[derive(Debug, Default)]
pub struct DesktopLease {
    holder: Option<Claim>,
    queue: VecDeque<Claim>,
}

impl DesktopLease {
    /// A free lease with [`IDLE_TTL`].
    pub fn new() -> DesktopLease {
        DesktopLease::default()
    }

    /// The current holder.
    pub fn holder(&self) -> Option<&Claim> {
        self.holder.as_ref()
    }

    /// Sessions waiting, in order.
    pub fn queue(&self) -> impl Iterator<Item = &Claim> {
        self.queue.iter()
    }

    /// Ask for the lease. Idempotent for a session already holding or queued.
    ///
    /// Test: `second_session_is_queued_fifo_while_first_holds`.
    pub fn connect(&mut self, session: &str, grant: &str, now: f64) -> Result<Connect, LeaseError> {
        if let Some(h) = &self.holder
            && h.session == session
        {
            return if h.grant == grant {
                Ok(Connect::Held)
            } else {
                Err(LeaseError::GrantMismatch)
            };
        }
        if let Some(i) = self.queue.iter().position(|c| c.session == session) {
            if self.queue[i].grant != grant {
                return Err(LeaseError::GrantMismatch);
            }
            return Ok(Connect::Queued { position: i + 1 });
        }
        let claim = Claim {
            session: session.to_owned(),
            grant: grant.to_owned(),
            last_call: now,
        };
        if self.holder.is_none() {
            self.holder = Some(claim);
            return Ok(Connect::Held);
        }
        self.queue.push_back(claim);
        Ok(Connect::Queued {
            position: self.queue.len(),
        })
    }

    /// Require `session` to hold the lease with `grant` at `now`; marks the call.
    ///
    /// Why: no call reaches the driver from a session that is queued, idle-expired or on another grant.
    /// Test: `queued_session_call_is_refused`, `idle_holder_loses_the_lease`.
    pub fn check(&mut self, session: &str, grant: &str, now: f64) -> Result<(), LeaseError> {
        match &mut self.holder {
            Some(h) if h.session == session && h.grant == grant => {
                h.last_call = now;
                Ok(())
            }
            Some(h) if h.session == session => Err(LeaseError::NotHeld(format!(
                "held with grant {}, not {grant}",
                h.grant
            ))),
            _ => Err(LeaseError::NotHeld(format!(
                "{session} does not hold the lease"
            ))),
        }
    }

    fn promote(&mut self, now: f64) -> Option<Claim> {
        self.holder = self.queue.pop_front().map(|mut c| {
            c.last_call = now;
            c
        });
        self.holder.clone()
    }

    /// Give up the lease (or a queue place). Returns the new holder when the lease moved.
    pub fn release(&mut self, session: &str, now: f64) -> Option<Claim> {
        self.queue.retain(|c| c.session != session);
        if self.holder.as_ref().is_some_and(|h| h.session == session) {
            return self.promote(now);
        }
        None
    }

    /// A session's connection closed: same as [`DesktopLease::release`].
    pub fn disconnect(&mut self, session: &str, now: f64) -> Option<Claim> {
        self.release(session, now)
    }

    /// Drop every claim, holder and queue (the lockout).
    pub fn clear(&mut self) {
        self.holder = None;
        self.queue.clear();
    }

    /// Drop every claim on `grant` (revoked or expired). Returns the new holder when it moved.
    pub fn drop_grant(&mut self, grant: &str, now: f64) -> Option<Claim> {
        self.queue.retain(|c| c.grant != grant);
        if self.holder.as_ref().is_some_and(|h| h.grant == grant) {
            return self.promote(now);
        }
        None
    }

    /// Expire an idle holder. Returns `(expired session, new holder)` when one expired.
    ///
    /// Test: `idle_holder_loses_the_lease`.
    pub fn expire(&mut self, now: f64) -> Option<(String, Option<Claim>)> {
        let h = self.holder.as_ref()?;
        if now - h.last_call < IDLE_TTL {
            return None;
        }
        let gone = h.session.clone();
        Some((gone, self.promote(now)))
    }

    /// Position of `session` in the queue (1 is next), `0` for the holder.
    pub fn position(&self, session: &str) -> Option<usize> {
        if self.holder.as_ref().is_some_and(|h| h.session == session) {
            return Some(0);
        }
        self.queue
            .iter()
            .position(|c| c.session == session)
            .map(|i| i + 1)
    }
}
