//! `secrets.unlock` and `secrets.lock`: the 8 h unlock grant (#10524, owner
//! ruling Dd5).
//!
//! Why: the owner should not answer a Keychain or 1Password prompt for every
//! secret read. One unlock opens a grant; while it is live the server keeps
//! the values it reads in locked, zeroed-on-drop memory
//! ([`crate::store::grant_cache`]) and serves repeats from there.
//! What: [`unlock`] refuses a caller with a Claude Code ancestor (or an
//! unreadable ancestry), writes its allow record, subscribes to the power
//! watcher, opens the grant and starts its sentinel thread; a watcher or
//! thread that cannot start opens no grant. [`lock`] ends the grant. The
//! sentinel checks the grant every tick: expiry on both clocks, a sleep gap,
//! a dead power source; its own exit, however it happens, lowers the
//! grant's watcher flag, which ends the grant at the next check. Every drop
//! is audited once with its typed reason. The grant lives in memory only:
//! a server restart never restores it, and while it is live the idle exit
//! is deferred ([`grant_keeps_serving`]). #10524 (owner ruling 2026-11-19):
//! when the Keychain holds the vault blob, unlock reads it once (the one
//! prompt) and the grant holds it, so no later Keychain read prompts; a
//! blob that cannot be read or held opens no grant. The router's peer-uid check and
//! `secrets.resolve`'s grant, ancestry, agents-flag and audit checks still
//! run before any value is served.
//! Test: `unlock_tests.rs` beside this file.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde_json::Value;

use super::ancestry::has_agent_ancestor;
use super::audit::{AuditMethod, AuditRecord, AuditSink};
use super::errors::ErrorKind;
use super::gate::{Gate, Recording, audited, suppressed_by_machine};
use super::methods::{Caller, to_json};
use super::power::{PowerEvent, PowerHook, PowerSource, Subscription};
use super::router::State;
use crate::api::BackendId;
use crate::api::methods::{LockResponse, UnlockResponse};
use crate::store::KEYCHAIN_CALL_TIMEOUT;
use crate::store::SecretCache;
use crate::store::grant_cache::{DropReason, Status};
use crate::store::platform::now_unix;

/// How often the sentinel checks the grant.
pub(crate) const SENTINEL_TICK: Duration = Duration::from_secs(1);

/// What a grant drop needs outside any request: the cache and the audit log.
#[derive(Clone)]
struct Dropper {
    cache: Arc<SecretCache>,
    audit: Arc<AuditSink>,
    suppressed: bool,
}

impl Dropper {
    fn of(state: &State) -> Self {
        Self {
            cache: Arc::clone(&state.cache),
            audit: Arc::clone(&state.audit),
            suppressed: suppressed_by_machine(state),
        }
    }

    /// Append one `secrets.unlock.drop` record, best-effort: the grant is
    /// already gone, which is the fail-closed state.
    fn record(&self, reason: DropReason) {
        if self.suppressed {
            return;
        }
        let mut record = AuditRecord::new(now_unix(), AuditMethod::GrantDrop, None);
        record.grant_drop = Some(reason);
        if let Ok(mut file) = self.audit.open() {
            let _ = file.append(&record);
        }
    }

    /// End grant `generation` (any, for `None`) for `reason`, audited.
    fn close(&self, reason: DropReason, generation: Option<u64>) {
        if self.cache.grant().close(reason, generation) {
            self.record(reason);
        }
    }

    /// Check the grant now, auditing an ending the check found.
    fn settle(&self) -> Status {
        let status = self.cache.grant().check();
        if let Status::Ended(reason) = status {
            self.record(reason);
        }
        status
    }
}

/// Refuse anything but a params object.
fn require_object(params: &Value) -> Result<(), ErrorKind> {
    match params {
        Value::Object(_) => Ok(()),
        _ => Err(ErrorKind::InvalidParams),
    }
}

/// `secrets.unlock`: open the unlock grant for the configured TTL.
///
/// Why: AC1, AC5 — one approval opens 8 h of prompt-free reads; an agent
/// must never be able to open it.
/// What: a caller with no peer pid, an agent ancestor or an unreadable
/// ancestry is [`ErrorKind::AgentUseRefused`]. The allow record is written
/// before the grant opens, so an unwritable log opens nothing. A live grant
/// is replaced, never extended. A power watcher or sentinel that cannot
/// start is [`ErrorKind::Internal`] and leaves no grant.
/// Test: `unlock_by_agent_descendant_is_refused_and_opens_nothing`,
/// `unlock_watcher_that_cannot_start_opens_no_grant`,
/// `unlock_then_resolve_reads_the_keychain_once`.
pub(crate) fn unlock(state: &State, caller: Caller, params: Value) -> Result<Value, ErrorKind> {
    audited(
        state,
        caller,
        AuditMethod::Unlock,
        Recording::WriteAhead,
        |gate| {
            require_object(&params)?;
            person_only(state, caller, gate)?;
            let dropper = Dropper::of(state);
            dropper.settle();
            gate.write_ahead()?;
            let ttl = state.settings.unlock_ttl;
            let generation = open(state, &dropper, ttl)?;
            // #10524: no blob held when one exists means no grant.
            let blob_held = hold_blob(state, generation).inspect_err(|_| {
                dropper.close(DropReason::Fault, Some(generation));
            })?;
            to_json(&UnlockResponse {
                expires_in_secs: ttl.as_secs(),
                blob_held,
            })
        },
    )
}

/// Refuse a caller with no peer pid, a Claude Code ancestor, or an
/// unreadable ancestry; record the verdict on `gate`.
///
/// Why: #10524 — an agent never opens the grant or migrates the Keychain.
/// Test: `unlock_by_agent_descendant_is_refused_and_opens_nothing`,
/// `migrate_by_agent_descendant_is_refused_and_changes_nothing`.
pub(crate) fn person_only(
    state: &State,
    caller: Caller,
    gate: &mut Gate<'_>,
) -> Result<(), ErrorKind> {
    let peer = caller.pid().ok_or(ErrorKind::AgentUseRefused)?;
    let agent = has_agent_ancestor(state.grants.processes(), peer)
        .map_err(|_| ErrorKind::AgentUseRefused)?;
    gate.agent_parent(agent);
    if agent {
        return Err(ErrorKind::AgentUseRefused);
    }
    Ok(())
}

/// Read the Keychain's vault blob once and hold it in grant `generation`.
///
/// What: `Ok(false)` when there is no blob to hold: a sandboxed server, no
/// Keychain, a backend with no blob store, or the per-item layout. A read
/// that fails or outlasts [`KEYCHAIN_CALL_TIMEOUT`], or a blob the grant
/// cannot hold (a refused page lock), is an error.
/// Test: `migrate_then_unlock_reads_the_keychain_once`,
/// `unlock_with_an_unreadable_blob_opens_no_grant`.
fn hold_blob(state: &State, generation: u64) -> Result<bool, ErrorKind> {
    if state.sandboxed || !state.keychain_compiled {
        return Ok(false);
    }
    let Some(store) = state
        .open(&BackendId::keychain())
        .ok()
        .and_then(|backend| backend.blob_store())
    else {
        return Ok(false);
    };
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("trusty-secrets-blob-read".to_owned())
        .spawn(move || {
            let _ = tx.send(store.read_blob());
        })
        .map_err(|_| ErrorKind::Internal)?;
    // See #7524: an abandoned read stays blocked on its prompt; it holds
    // nothing and writes nothing.
    let blob = match rx.recv_timeout(KEYCHAIN_CALL_TIMEOUT) {
        Ok(read) => read.map_err(ErrorKind::from)?,
        Err(_) => return Err(ErrorKind::BackendTimeout),
    };
    match blob {
        None => Ok(false),
        Some(blob) if state.cache.grant().hold(generation, &blob) => Ok(true),
        Some(_) => Err(ErrorKind::Internal),
    }
}

/// End a live grant after `secrets.migrate` changed the Keychain layout, so
/// no grant keeps a snapshot of the old one (#10524).
pub(crate) fn end_for_layout_change(state: &State) {
    Dropper::of(state).close(DropReason::Revoked, None);
}

/// Subscribe, open the grant and start its sentinel; any failure leaves no
/// grant. Returns the grant's generation.
fn open(state: &State, dropper: &Dropper, ttl: Duration) -> Result<u64, ErrorKind> {
    let watcher = Arc::new(AtomicBool::new(true));
    let generation = Arc::new(OnceLock::<u64>::new());
    let fired = Arc::new(AtomicBool::new(false));
    let hook: PowerHook = {
        let (dropper, generation, fired) =
            (dropper.clone(), Arc::clone(&generation), Arc::clone(&fired));
        Arc::new(move |event| match event {
            PowerEvent::Sleep => {
                fired.store(true, Ordering::SeqCst);
                if let Some(generation) = generation.get() {
                    dropper.close(DropReason::Sleep, Some(*generation));
                }
            }
        })
    };
    // #10524 AC3: no watcher, no grant.
    let subscription = state
        .power
        .subscribe(hook)
        .map_err(|_| ErrorKind::Internal)?;
    let opened = state.cache.grant().open(ttl, Arc::clone(&watcher));
    // `set` fails only if already set, and this is its one writer.
    let _ = generation.set(opened);
    if fired.load(Ordering::SeqCst) {
        dropper.close(DropReason::Sleep, Some(opened));
    }
    let sentinel = Sentinel {
        dropper: dropper.clone(),
        power: Arc::clone(&state.power),
        generation: opened,
        watcher,
        _subscription: subscription,
        tick: state.grant_tick,
    };
    std::thread::Builder::new()
        .name("trusty-secrets-grant".to_owned())
        .spawn(move || sentinel.run())
        .map(|_| opened)
        .map_err(|_| {
            // #10524: no sentinel, no grant; the deny record says why.
            state.cache.grant().close(DropReason::Fault, Some(opened));
            ErrorKind::Internal
        })
}

/// One grant's watcher thread.
struct Sentinel {
    dropper: Dropper,
    power: Arc<dyn PowerSource>,
    generation: u64,
    watcher: Arc<AtomicBool>,
    _subscription: Subscription,
    tick: Duration,
}

impl Drop for Sentinel {
    fn drop(&mut self) {
        // #10524 AC3: a sentinel that stops, by return or panic, ends its
        // grant at the next check.
        self.watcher.store(false, Ordering::SeqCst);
    }
}

impl Sentinel {
    fn run(self) {
        loop {
            std::thread::sleep(self.tick);
            if !self.power.alive() {
                self.dropper.close(DropReason::Fault, Some(self.generation));
                return;
            }
            match self.dropper.settle() {
                Status::Live { generation, .. } if generation == self.generation => {}
                _ => return,
            }
        }
    }
}

/// `secrets.lock`: end the unlock grant.
///
/// What: the grant ends first, so an unwritable log never keeps it alive
/// (the reply is then [`ErrorKind::AuditUnavailable`]). Any same-uid
/// caller may lock. One record, with `grant_drop: revoked` when a live
/// grant ended.
/// Test: `unlock_then_resolve_reads_the_keychain_once`.
pub(crate) fn lock(state: &State, caller: Caller, params: Value) -> Result<Value, ErrorKind> {
    audited(state, caller, AuditMethod::Lock, Recording::Once, |gate| {
        require_object(&params)?;
        Dropper::of(state).settle();
        let locked = state.cache.grant().close(DropReason::Revoked, None);
        if locked {
            gate.grant_drop(DropReason::Revoked);
        }
        gate.admit()?;
        to_json(&LockResponse { locked })
    })
}

/// Whether a read starting now runs under a live grant; audits an ending.
pub(crate) fn live_for_read(state: &State) -> bool {
    matches!(Dropper::of(state).settle(), Status::Live { .. })
}

/// Whether a live grant defers the idle exit; audits an ending.
pub(crate) fn grant_keeps_serving(state: &State) -> bool {
    live_for_read(state)
}

/// End the grant as the session ends (the server is shutting down).
pub(crate) fn end_session(state: &State) {
    Dropper::of(state).close(DropReason::Logout, None);
}
