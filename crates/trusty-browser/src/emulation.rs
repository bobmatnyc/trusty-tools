//! Leased-tab emulation: JavaScript off, `prefers-color-scheme`, viewport size (#11371).
//!
//! Why: a visual check needs a tab with JavaScript disabled, a forced color
//! scheme or a fixed viewport, and none of it may outlive the lease that set it:
//! the next holder of the tab must see a plain tab.
//! What: [`run`] sends `page_emulate` with the complete emulation state (the
//! defaults reset it) and records the tab in [`State`] before it sends, so an
//! `OutcomeUnknown` still gets a reset. [`reconcile`] resets every recorded tab
//! whose lease ended: released, expired, dropped with its session, its grant or
//! its worker. It runs on `release`, before a new lease's probe, at session end
//! and from a one-second watcher while a record exists. A reset needs the live
//! grant and worker; when either is gone the extension resets the tab itself
//! (`grant_end`, a dropped native port, a worker restart; `extension/emulation.js`).
//! Test: `emulation_tests.rs`; `an_emulated_tab_resets_on_release`,
//! `a_new_lease_resets_a_tab_left_emulated`, `an_expired_emulated_lease_resets_without_a_caller`,
//! `a_failed_emulation_reset_is_retried`,
//! `a_closed_session_resets_its_emulated_tab` in `tests/parity_pages.rs`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::broker::{Core, Inner};
use crate::grants::Grant;
use crate::leases::TTL;
use crate::pages::{self, Shape, Stop, Ty};
use crate::pages_ops::{Ran, send};
use crate::peer_conn::Peer;
use crate::pyjson::Json;

/// Largest emulated viewport side, in CSS pixels.
pub const MAX_SIDE: i64 = 10_000;
/// How often the watcher looks for an ended lease, while one tab is emulated.
const WATCH: Duration = Duration::from_secs(1);

/// `EMULATED`: the worker's answer, the state now in force.
pub const EMULATED: Shape = &[
    ("javascript", &[Ty::Bool]),
    ("color_scheme", &[Ty::Str, Ty::Null]),
    ("width", &[Ty::Int]),
    ("height", &[Ty::Int]),
];

/// One emulated tab and the lease that emulated it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Record {
    pub generation: String,
    pub tab: i64,
    pub token: String,
    pub grant: String,
}

/// Emulated tabs; lives on `Core`, so the public `Inner` gains no field.
#[derive(Default)]
pub(crate) struct State {
    records: Mutex<Vec<Record>>,
    /// Serializes resets, so a new lease's probe waits for a reset in flight.
    gate: tokio::sync::Mutex<()>,
    watching: AtomicBool,
}

impl State {
    fn records(&self) -> std::sync::MutexGuard<'_, Vec<Record>> {
        self.records.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Record `tab` as emulated under `record.token`, replacing an older record.
    pub(crate) fn put(&self, record: Record) {
        let mut all = self.records();
        all.retain(|r| !(r.generation == record.generation && r.tab == record.tab));
        all.push(record);
    }

    /// Put back a record whose reset failed, unless a newer lease recorded the tab since.
    fn restore(&self, record: Record) {
        let mut all = self.records();
        if !all
            .iter()
            .any(|r| r.generation == record.generation && r.tab == record.tab)
        {
            all.push(record);
        }
    }

    /// Forget `tab`: its emulation was reset by its own lease.
    fn forget(&self, generation: &str, tab: i64) {
        self.records()
            .retain(|r| !(r.generation == generation && r.tab == tab));
    }

    /// The emulated tabs.
    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> Vec<Record> {
        self.records().clone()
    }
}

/// The frame's emulation fields are well-typed and in range.
///
/// Why: the broker refuses a malformed op before the worker sees it, as for
/// every page op. What: `javascript` bool, `color_scheme` null, `"light"` or
/// `"dark"`, `width` and `height` ints in `0..=MAX_SIDE`, both zero (no
/// override) or both set.
/// Test: `emulation_fields_are_validated`.
pub fn valid(request: &Json) -> bool {
    let side = |k: &str| match request.get(k) {
        Some(Json::Int(n)) if (0..=MAX_SIDE).contains(n) => Some(*n),
        _ => None,
    };
    let scheme = matches!(
        request.get("color_scheme"),
        Some(Json::Null) | Some(Json::Str(_))
    ) && request
        .get("color_scheme")
        .and_then(Json::as_str)
        .is_none_or(|s| matches!(s, "light" | "dark"));
    matches!(request.get("javascript"), Some(Json::Bool(_)))
        && scheme
        && matches!((side("width"), side("height")), (Some(w), Some(h)) if (w == 0) == (h == 0))
}

/// The defaults: JavaScript on, no color scheme, no viewport override.
fn is_reset(request: &Json) -> bool {
    request.get("javascript") == Some(&Json::Bool(true))
        && request.get("color_scheme") == Some(&Json::Null)
        && request.get("width") == Some(&Json::Int(0))
}

fn reset_args(tab: i64) -> Vec<(&'static str, Json)> {
    vec![
        ("tab", Json::Int(tab)),
        ("javascript", Json::Bool(true)),
        ("color_scheme", Json::Null),
        ("width", Json::Int(0)),
        ("height", Json::Int(0)),
    ]
}

/// Run `page_emulate` on the session's leased tab.
///
/// Why: AC 1-3 of #11371. What: records the tab before sending unless the
/// frame resets it, sends the full state, checks [`EMULATED`], forgets the
/// record after a reset, renews the lease and replies `{ok, tab, ...state}`.
/// Test: `an_emulated_tab_resets_on_release`.
pub(crate) async fn run(
    core: &Core,
    grant: &Grant,
    token: &str,
    tab: i64,
    peer: &Arc<Peer>,
    origin: Option<&str>,
    request: &Json,
) -> Result<Ran, Stop> {
    let field = |k: &str| request.get(k).cloned().unwrap_or(Json::Null);
    let mut args = vec![("tab", Json::Int(tab))];
    args.extend(["javascript", "color_scheme", "width", "height"].map(|k| (k, field(k))));
    let reset = is_reset(request);
    if !reset {
        core.emulation.put(Record {
            generation: peer.generation.clone(),
            tab,
            token: token.to_owned(),
            grant: grant.ident.clone(),
        });
        watch(core);
    }
    let key = Some((peer.generation.clone(), tab));
    let raw = send(core, grant, key, peer, "page_emulate", args).await?;
    let result = pages::shaped(&raw, EMULATED)?;
    if reset {
        core.emulation.forget(&peer.generation, tab);
    }
    let deadline = core.lease_clock.now() + TTL;
    core.lock().with_claim(token, |c| c.deadline = deadline);
    let mut reply = Json::object([("ok", Json::Bool(true)), ("tab", Json::Int(tab))]);
    if let Json::Object(pairs) = result {
        for (k, v) in pairs {
            reply.insert(k, v);
        }
    }
    let host = crate::pages_url::host(origin).map_err(|_| Stop::Close)?;
    Ok((reply, Some(tab), host))
}

/// A record whose lease ended, with the worker and grant ident that can reset it.
pub(crate) type Ended = (Record, Option<(Arc<Peer>, String)>);

/// Take every record whose lease ended at `now`; pair each with its live worker and grant.
///
/// Why: AC 4: every lease end path must reset. What: a lease ended when no
/// claim holds its token, or the claim is idle past its deadline. The reset
/// goes through the record's grant only while that grant is active on the
/// record's worker generation; otherwise the extension resets the tab itself.
/// Test: `ended_records_cover_every_lease_end`.
pub(crate) fn ended(core: &Core, inner: &Inner, now: f64) -> Vec<Ended> {
    let live = |r: &Record| {
        inner.claims.iter().any(|c| {
            c.token == r.token
                && c.generation == r.generation
                && c.tab == r.tab
                && (c.busy || c.deadline > now)
        })
    };
    let mut all = core.emulation.records();
    let (gone, kept): (Vec<_>, Vec<_>) = all.drain(..).partition(|r| !live(r));
    *all = kept;
    gone.into_iter()
        .map(|r| {
            let peer = inner.peers.iter().find(|p| p.generation == r.generation);
            let grant = inner
                .grant(&r.grant)
                .filter(|g| g.active && g.generation.as_deref() == Some(r.generation.as_str()));
            let route = match (peer, grant) {
                (Some(p), Some(g)) => Some((Arc::clone(p), g.ident.clone())),
                _ => None,
            };
            (r, route)
        })
        .collect()
}

/// Reset every emulated tab whose lease ended; a failed reset is retried.
///
/// Why/What: see [`ended`]; the gate makes a new lease's probe wait for a
/// reset already in flight. A routed reset that fails puts its record back,
/// so the next reconcile (watcher, release or select) sends it again. A record
/// with no route is dropped: the extension resets a tab whose worker or grant
/// went away.
/// Test: `a_new_lease_resets_a_tab_left_emulated`, `a_failed_emulation_reset_is_retried`.
pub(crate) async fn reconcile(core: &Core) {
    let _gate = core.emulation.gate.lock().await;
    let now = core.lease_clock.now();
    let due = {
        let inner = core.lock();
        ended(core, &inner, now)
    };
    for (record, route) in due {
        if let Some((peer, grant)) = route {
            let args = reset_args(record.tab);
            // #11371: a reset that fails must not let the emulation reach a later lease.
            if let Err(e) = peer
                .page(&*core.entropy, "page_emulate", &grant, args)
                .await
            {
                eprintln!(
                    "trusty-browser: emulation reset of tab {} failed ({}); retrying",
                    record.tab, e.code
                );
                core.emulation.restore(record);
                watch(core);
            }
        }
    }
}

/// Keep one watcher task while any tab is emulated, so an idle expiry resets.
fn watch(core: &Core) {
    if core.emulation.watching.swap(true, Ordering::SeqCst) {
        return;
    }
    let me = core.this.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(WATCH).await;
            let Some(core) = me.upgrade() else { return };
            if core.lock().halted {
                core.emulation.watching.store(false, Ordering::SeqCst);
                return;
            }
            reconcile(&core).await;
            if core.emulation.records().is_empty() {
                core.emulation.watching.store(false, Ordering::SeqCst);
                // A record added since the check restarts the loop only if no other watcher did.
                if core.emulation.records().is_empty()
                    || core.emulation.watching.swap(true, Ordering::SeqCst)
                {
                    return;
                }
            }
        }
    });
}

#[cfg(test)]
#[path = "emulation_tests.rs"]
mod tests;
