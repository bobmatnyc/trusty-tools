//! Per-session prompter accounting and the `session.attach` forwarder that
//! holds one such claim (#8100). A child module of `registry`, split out for
//! the same 500-SLOC-cap reason as `events`.
//!
//! Why: an `ask` permission rule may only suspend a run when SOMEONE can see
//! the prompt for THAT session and answer it. The first cut of #8100 asked
//! `crate::events::bus().receiver_count()`, which is daemon-global: one
//! console watching one PM session marked every headless `task.run` sub-agent
//! as watched, restoring the 300 s stall the fix exists to remove.
//! What: [`PrompterGuard`], an RAII claim on one session's prompter count,
//! and the `claim_prompter`/`prompter_count` pair that mints and reads it. A
//! guard released by ANY means — detach, a dead connection, a dropped SSE
//! body, a panicking task — decrements, so a client that dies mid-ask stops
//! counting without anyone calling a teardown.
//! Test: `registry_tests::prompter_attached_while_a_session_attachment_lives`,
//! `registry_tests::prompter_claim_is_released_when_its_guard_drops`,
//! `registry_tests::prompter_attached_for_an_unknown_session_is_false`,
//! `registry_tests::prompter_count_survives_a_poisoned_registry_lock`.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

/// One live prompter's claim on one session, released when dropped (#8100).
///
/// Why: every prompter path ends in a different way — `session.detach` fires
/// a cancel channel, an HTTP SSE body is dropped when the socket closes, a
/// `session.events` stream ends when its receiver goes. A guard whose `Drop`
/// does the decrementing needs none of them to agree on a teardown call, and
/// covers the case the timeout was hiding: a client that dies mid-ask.
/// What: holds the session's counter directly rather than a handle to the
/// registry, so releasing it never re-enters the registry lock and a session
/// removed from the map while a client is still attached cannot strand a
/// count on an entry that no longer exists.
/// Test: `registry_tests::prompter_claim_is_released_when_its_guard_drops`.
pub struct PrompterGuard(Arc<AtomicUsize>);

impl Drop for PrompterGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl SessionRegistry {
    /// Claim `id` as watched by a prompter until the returned guard drops.
    ///
    /// Why (#8100): the counted thing is a client that can both SEE this
    /// session's `permission_requested` event and answer it with
    /// `session.permission.respond` — `session.attach`, the `session.events`
    /// stream, and the per-session HTTP SSE route. The workstream-aggregate
    /// route (`workstreams::sse`) is deliberately NOT a claimant: it is bound
    /// to a workstream, re-checks membership per event, and a session can
    /// leave the group mid-ask, so no session's prompt is its to display.
    /// What: `None` when `id` names no live session — nothing was claimed, so
    /// there is nothing to release, and [`prompter_count`] keeps reading `0`.
    /// Test: `registry_tests::prompter_attached_while_a_session_attachment_lives`,
    /// `registry_tests::prompter_claim_is_released_when_its_guard_drops`.
    ///
    /// [`prompter_count`]: SessionRegistry::prompter_count
    pub fn claim_prompter(&self, id: &str) -> Option<PrompterGuard> {
        let counter = Arc::clone(&self.lock().get(id)?.prompters);
        counter.fetch_add(1, Ordering::SeqCst);
        Some(PrompterGuard(counter))
    }

    /// How many prompters currently watch `id`; `0` for an unknown session.
    ///
    /// Why (#8100): an unknown session is the fail-safe arm — a permission
    /// prompt for a session the registry cannot find is a prompt nobody can be
    /// shown, so it must read as unwatched and deny rather than wait.
    /// What: `SessionRegistry::lock` recovers a poisoned mutex through
    /// `into_inner`, so the count stays readable after an unrelated panic and
    /// the missing entry is the only unreadable arm.
    /// Test: `registry_tests::prompter_attached_for_an_unknown_session_is_false`,
    /// `registry_tests::prompter_count_survives_a_poisoned_registry_lock`.
    pub fn prompter_count(&self, id: &str) -> usize {
        self.lock()
            .get(id)
            .map_or(0, |entry| entry.prompters.load(Ordering::SeqCst))
    }
}

/// Spawn the background task that forwards one session's live event
/// envelopes to a connection until cancelled or the connection is gone.
///
/// Why: split out of `attach` for readability; also gives `registry_tests`
/// a single well-named spawn site to reason about. Subscribing to the bus
/// SYNCHRONOUSLY here — before `tokio::spawn` schedules the forwarder body
/// — matters: if the subscription happened inside the spawned `async`
/// block instead, a caller that calls `send()` immediately after `attach()`
/// returns could publish its event before the spawned task ever gets polled
/// and subscribes, and `broadcast::Receiver`s only see events published
/// after `subscribe()` was called — the event would be silently missed.
/// What: subscribes to `crate::events::subscribe()` immediately, then
/// spawns a task that forwards envelopes whose `session_id` matches
/// `session_id` as a JSON-RPC notification via `notify` (`params` is the
/// envelope itself — `seq`/`at`/`kind`/`event` all present), skips
/// envelopes for other sessions and lag gaps, and exits on `cancel_rx`
/// firing (either a real detach or the sender being dropped) or
/// `notify.send` failing (connection gone). #8100: `prompter` rides the
/// spawned task, so this connection stops counting as a prompter the moment
/// the loop ends, however it ends.
/// Test: `registry_tests::attach_forwards_live_events_until_detach`,
/// `registry_tests::prompter_attached_while_a_session_attachment_lives`.
pub(super) fn spawn_forwarder(
    session_id: String,
    notify: NotifySender,
    cancel_rx: oneshot::Receiver<()>,
    prompter: Option<PrompterGuard>,
) {
    use tokio::sync::broadcast::error::RecvError;

    // Subscribe before spawning (see the Why above) so no event published
    // by the caller right after `attach()` returns can be missed.
    let mut events = crate::events::subscribe();

    tokio::spawn(async move {
        // #8100: released when this task ends — detach, dead connection, or
        // a closed bus all decrement without a teardown call.
        let _prompter = prompter;
        tokio::pin!(cancel_rx);
        loop {
            tokio::select! {
                biased;
                _ = &mut cancel_rx => break,
                received = events.recv() => match received {
                    Ok(envelope) if envelope.session_id == session_id => {
                        let notification = json!({
                            "jsonrpc": "2.0",
                            "method": "session.event",
                            "params": envelope,
                        });
                        if notify.send(notification).is_err() {
                            break;
                        }
                    }
                    Ok(_) => continue,
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                },
            }
        }
    });
}
