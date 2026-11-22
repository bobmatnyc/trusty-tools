//! Leased-tab emulation: JavaScript off, `prefers-color-scheme`, viewport (#11371).
//!
//! Why: a visual check (JS off, dark scheme, 1440x900) must not need a manual
//! DevTools pass, and an emulated tab must not stay emulated once the lease
//! that set it ends, whether by release, expiry, session end or grant end.
//! What: [`apply`] sends `page_javascript`, `page_color_scheme` or
//! `page_viewport` to the worker, which drives `Emulation.*` over
//! chrome.debugger and keeps the tab attached while any override holds. Each
//! emulated tab is tracked in `Inner.emulated`; a watcher per tab sends
//! `page_emulation_reset` once its lease is gone or expired. A failed reset
//! is never dropped: the entry stays pending, stderr names it, a `release`
//! keeps its lease and a `select` of that tab is refused until a reset runs.
//! A dropped worker connection removes its entries: the extension detaches
//! every emulated tab itself when its native port closes.
//! Test: `a_failed_reset_keeps_the_lease_and_refuses_the_next_select`,
//! `a_lease_end_resets_an_emulated_tab`, `emulation_requests_are_validated`.

use std::ops::RangeInclusive;
use std::sync::Arc;
use std::time::Duration;

use crate::broker::{Core, Inner};
use crate::grants::Grant;
use crate::pages::{Shape, Stop, Ty, shaped};
use crate::pages_ops::{Leased, Ran, hosted, ok_tab, renew, reply_with, send};
use crate::pyjson::Json;
use crate::refusal::Refusal;

/// The agent-facing emulation ops.
pub const OPS: &[&str] = &["page_javascript", "page_color_scheme", "page_viewport"];
/// The broker-only op that clears every override on a tab.
pub const RESET_OP: &str = "page_emulation_reset";
/// `scheme` values; `none` clears the override.
pub const SCHEMES: &[&str] = &["light", "dark", "none"];
/// Viewport width bounds in CSS pixels; `0` with height `0` clears the override.
pub const WIDTH: RangeInclusive<i64> = 200..=7680;
/// Viewport height bounds in CSS pixels.
pub const HEIGHT: RangeInclusive<i64> = 200..=4320;
/// How often a watcher checks its tab's lease.
const WATCH: Duration = Duration::from_secs(2);

/// The worker's reply: the tab's emulation state after the op.
pub const EMULATED: Shape = &[
    ("javascript", &[Ty::Bool]),
    ("color_scheme", &[Ty::Str]),
    ("width", &[Ty::Int]),
    ("height", &[Ty::Int]),
];

/// One tab the broker emulated and has not yet reset.
#[derive(Debug, Clone)]
pub struct Emulated {
    /// Peer generation.
    pub generation: String,
    /// Tab id.
    pub tab: i64,
    /// The grant the reset is sent under.
    pub grant: String,
    /// The lease that set the emulation.
    pub token: String,
    /// A reset is in flight.
    pub resetting: bool,
    /// Resets that failed so far.
    pub failures: u32,
}

/// Field checks for an emulation op; `true` for any other op.
///
/// Why: a malformed value must be refused before it reaches the worker.
/// What: `enabled` is a bool, `scheme` one of [`SCHEMES`], and the viewport
/// both `0` (reset) or both inside [`WIDTH`] x [`HEIGHT`].
/// Test: `emulation_requests_are_validated`.
pub fn valid(op: &str, request: &Json) -> bool {
    let int = |k: &str| match request.get(k) {
        Some(Json::Int(n)) => Some(*n),
        _ => None,
    };
    match op {
        "page_javascript" => matches!(request.get("enabled"), Some(Json::Bool(_))),
        "page_color_scheme" => request
            .get("scheme")
            .and_then(Json::as_str)
            .is_some_and(|s| SCHEMES.contains(&s)),
        "page_viewport" => match (int("width"), int("height")) {
            (Some(0), Some(0)) => true,
            (Some(w), Some(h)) => WIDTH.contains(&w) && HEIGHT.contains(&h),
            _ => false,
        },
        _ => true,
    }
}

fn fields(op: &str, request: &Json) -> Vec<(&'static str, Json)> {
    let names: &[&'static str] = match op {
        "page_javascript" => &["enabled"],
        "page_color_scheme" => &["scheme"],
        _ => &["width", "height"],
    };
    names
        .iter()
        .map(|k| (*k, request.get(k).cloned().unwrap_or(Json::Null)))
        .collect()
}

/// Run one emulation op on the leased tab.
///
/// Why/What: see the module doc. The tab is tracked before the command is
/// sent, so a command that may have run is still reset when the lease ends.
/// Test: `a_lease_end_resets_an_emulated_tab`.
pub(crate) async fn apply(
    core: &Arc<Core>,
    grant: &Grant,
    claim: &Leased,
    op: &str,
    request: &Json,
) -> Result<Ran, Stop> {
    track(
        core,
        &grant.ident,
        &claim.peer.generation,
        claim.tab,
        &claim.token,
    );
    let mut args = vec![("tab", Json::Int(claim.tab))];
    args.extend(fields(op, request));
    let raw = send(core, grant, claim.key(), &claim.peer, op, args).await?;
    let result = shaped(&raw, EMULATED)?;
    renew(core, &claim.token);
    let reply = reply_with(ok_tab(claim.tab), result);
    Ok((reply, Some(claim.tab), hosted(claim.origin.as_deref())?))
}

/// Record `(generation, tab)` as emulated under `token`; the first record starts its watcher.
pub fn track(core: &Arc<Core>, grant: &str, generation: &str, tab: i64, token: &str) {
    let mut inner = core.lock();
    if let Some(e) = find(&mut inner, generation, tab) {
        e.token = token.to_owned();
        e.grant = grant.to_owned();
        return;
    }
    inner.emulated.push(Emulated {
        generation: generation.to_owned(),
        tab,
        grant: grant.to_owned(),
        token: token.to_owned(),
        resetting: false,
        failures: 0,
    });
    drop(inner);
    watch(core, generation.to_owned(), tab);
}

fn find<'a>(inner: &'a mut Inner, generation: &str, tab: i64) -> Option<&'a mut Emulated> {
    inner
        .emulated
        .iter_mut()
        .find(|e| e.generation == generation && e.tab == tab)
}

/// True while the lease `token` is held and not past its deadline.
fn live(inner: &Inner, token: &str, now: f64) -> bool {
    inner
        .claims
        .iter()
        .any(|c| c.token == token && (c.busy || c.deadline > now))
}

/// Reset `(generation, tab)` now; `Ok` once no override can remain.
///
/// Why: the lease must not be reported ended while the tab stays emulated.
/// What: no entry, or no connected worker of that generation (the extension
/// detaches on disconnect), is `Ok`. A worker refusal keeps the entry pending,
/// counts the failure, logs it to stderr and returns the refusal.
/// Test: `a_failed_reset_keeps_the_lease_and_refuses_the_next_select`.
pub async fn reset(core: &Core, generation: &str, tab: i64) -> Result<(), Refusal> {
    let (peer, grant) = {
        let mut inner = core.lock();
        let peer = inner
            .peers
            .iter()
            .find(|p| p.generation == generation)
            .cloned();
        let Some(entry) = find(&mut inner, generation, tab) else {
            return Ok(());
        };
        if entry.resetting {
            return Err(Refusal::new("DebuggerBusy").with("tab", tab));
        }
        let Some(peer) = peer else {
            inner
                .emulated
                .retain(|e| !(e.generation == generation && e.tab == tab));
            return Ok(());
        };
        entry.resetting = true;
        (peer, entry.grant.clone())
    };
    let args = vec![("tab", Json::Int(tab))];
    let result = peer.page(&*core.entropy, RESET_OP, &grant, args).await;
    let mut inner = core.lock();
    match result {
        Ok(_) => {
            inner
                .emulated
                .retain(|e| !(e.generation == generation && e.tab == tab));
            Ok(())
        }
        Err(refusal) => {
            if let Some(e) = find(&mut inner, generation, tab) {
                e.resetting = false;
                e.failures += 1;
            }
            eprintln!(
                "trusty-browser: emulation reset failed for tab {tab}: {}; still pending",
                refusal.code
            );
            Err(refusal)
        }
    }
}

/// Reset the tab the lease `token` emulated, if any (before `release` drops it).
pub async fn reset_lease(core: &Core, token: &str) -> Result<(), Refusal> {
    let target = core
        .lock()
        .emulated
        .iter()
        .find(|e| e.token == token)
        .map(|e| (e.generation.clone(), e.tab));
    match target {
        Some((generation, tab)) => reset(core, &generation, tab).await,
        None => Ok(()),
    }
}

/// Before a new lease on `(generation, tab)`: reset an emulation whose lease ended.
///
/// Why: a pending reset must not let a new lease start on a still-emulated
/// tab. What: an entry whose lease is still live is left alone (the caller
/// refuses `TabClaimed` or reuses it); an ended one is reset or refused.
/// Test: `a_failed_reset_keeps_the_lease_and_refuses_the_next_select`.
pub async fn settle(core: &Core, generation: &str, tab: i64) -> Result<(), Refusal> {
    let pending = {
        let now = core.lease_clock.now();
        let inner = core.lock();
        inner
            .emulated
            .iter()
            .any(|e| e.generation == generation && e.tab == tab && !live(&inner, &e.token, now))
    };
    if pending {
        reset(core, generation, tab).await
    } else {
        Ok(())
    }
}

/// Reset every tab whose lease has ended; failures stay pending.
pub async fn reap(core: &Core) {
    let ended: Vec<(String, i64)> = {
        let now = core.lease_clock.now();
        let inner = core.lock();
        inner
            .emulated
            .iter()
            .filter(|e| !e.resetting && !live(&inner, &e.token, now))
            .map(|e| (e.generation.clone(), e.tab))
            .collect()
    };
    for (generation, tab) in ended {
        // A failure is logged and kept pending by `reset`; the watcher retries it.
        let _ = reset(core, &generation, tab).await;
    }
}

/// Poll one emulated tab until its entry is gone, resetting it once its lease ends.
fn watch(core: &Arc<Core>, generation: String, tab: i64) {
    let weak = Arc::downgrade(core);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(WATCH).await;
            let Some(core) = weak.upgrade() else {
                return;
            };
            let ended = {
                let now = core.lease_clock.now();
                let inner = core.lock();
                if inner.halted {
                    return;
                }
                let entry = inner
                    .emulated
                    .iter()
                    .find(|e| e.generation == generation && e.tab == tab);
                match entry {
                    None => return,
                    Some(e) => !e.resetting && !live(&inner, &e.token, now),
                }
            };
            if ended {
                // A failure stays pending and is retried on the next tick.
                let _ = reset(&core, &generation, tab).await;
            }
        }
    });
}

#[cfg(test)]
#[path = "emulation_tests.rs"]
mod tests;
