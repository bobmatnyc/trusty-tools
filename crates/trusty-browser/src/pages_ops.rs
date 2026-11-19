//! Running a validated page op (`Pages.run`, `send`, `leased`, `select`, `switch`).
//!
//! Why: every op must stay inside its grant and lease whatever the worker
//! does, and no mutating op may run twice: a journaled `request_id` answers
//! before the lease is checked (#11239), so an `OutcomeUnknown` that dropped
//! the lease still never repeats.
//! What: [`run`] dispatches one op; [`send`] is one worker command with the
//! contract's lease and grant fate on refusal; [`select`] takes or renews a
//! lease (one controller per tab, `TabClaimed` otherwise); [`switch`]
//! focuses the leased tab on a profile grant. Claim state lives in
//! `Inner.claims` and is changed only between awaits.
//! Test: `page_ops_replay_byte_identically` in `tests/parity_pages.rs`;
//! `an_idle_lease_expires_and_a_busy_one_does_not` in `src/leases.rs`.

use std::sync::Arc;

use crate::broker::{Claim, Core};
use crate::entropy::token_urlsafe;
use crate::grants::Grant;
use crate::leases::{JOURNAL_LIMIT, Journal, TTL};
use crate::pages::{self, HISTORY, Shape, Stop};
use crate::pages_capture::{self as capture, Capture};
use crate::pages_frames::{self as frames, FrameTable};
use crate::pages_url::host;
use crate::peer_conn::Peer;
use crate::pyjson::Json;
use crate::refusal::{MUTATING, Refusal, request_string};

/// One attached agent.
pub struct Session {
    /// Session id (`SessionRef.id`).
    pub id: u64,
    /// Owner token (`Session.owner`).
    pub owner: String,
    /// The grant it acts under.
    pub grant: String,
    /// Replies by `request_id`.
    pub journal: Journal,
    /// Screenshots a coordinate action may name, oldest first (#11211).
    pub captures: Vec<Capture>,
    /// Each lease's latest snapshot frames, for frame-scoped refs (#11362).
    pub frames: Vec<FrameTable>,
}

/// A reply, the operated tab and the result host.
pub type Ran = (Json, Option<i64>, Option<String>);

/// Worker refusals that keep the lease (`KEEP_LEASE`).
const KEEP_LEASE: &[&str] = &[
    "PermissionNeeded",
    "DocumentChanged",
    "NavigationPending",
    "StaleElement",
    "NotEditable",
    "TextDropped",
    "NotSelectable",
    "NoSuchOption",
    "Obscured",
    "DebuggerBusy",
    "NotDraggable",
    "InvalidUrl",
    "InvalidRequest",
    "InvalidOperation",
    "NoHistory",
    // #11211: a stale or out-of-view coordinate, or an oversized capture, keeps the lease.
    "StaleCapture",
    "OutOfViewport",
    "CaptureTooLarge",
    // #11362: Chrome refused a script world in the frame (a sandboxed frame, say).
    "FrameScriptBlocked",
];

pub(crate) fn hosted(origin: Option<&str>) -> Result<Option<String>, Stop> {
    host(origin).map_err(|_| Stop::Close)
}

/// The leased tab a page op acts on, as `leased` found it.
pub(crate) struct Leased {
    pub(crate) token: String,
    pub(crate) tab: i64,
    pub(crate) document: Option<String>,
    pub(crate) origin: Option<String>,
    pub(crate) peer: Arc<Peer>,
}

impl Leased {
    pub(crate) fn key(&self) -> Option<(String, i64)> {
        Some((self.peer.generation.clone(), self.tab))
    }
}

pub(crate) fn peer_of(core: &Core, grant: &Grant) -> Result<Arc<Peer>, Refusal> {
    let peer = core.lock().peer(&grant.route.profile);
    peer.filter(|p| Some(p.generation.as_str()) == grant.generation.as_deref())
        .ok_or_else(|| Refusal::new("BrowserDisconnected"))
}

/// One page command with the contract's grant and lease fate on refusal (`Pages.send`).
pub(crate) async fn send(
    core: &Core,
    grant: &Grant,
    claim: Option<(String, i64)>,
    peer: &Arc<Peer>,
    op: &str,
    args: Vec<(&str, Json)>,
) -> Result<Json, Refusal> {
    let set_busy = |busy: bool| {
        if let Some((generation, tab)) = &claim {
            let mut inner = core.lock();
            let held = inner.claims.iter_mut();
            if let Some(c) = held
                .into_iter()
                .find(|c| &c.generation == generation && c.tab == *tab)
            {
                c.busy = busy;
            }
        }
    };
    set_busy(true);
    let result = peer.page(&*core.entropy, op, &grant.ident, args).await;
    set_busy(false);
    let Err(exc) = result else {
        return result;
    };
    if KEEP_LEASE.contains(&exc.code.as_str()) {
        return Err(exc);
    }
    let mut inner = core.lock();
    if let Some((generation, tab)) = &claim {
        inner
            .claims
            .retain(|c| !(&c.generation == generation && c.tab == *tab));
    }
    let gone_peer = inner
        .peer(&grant.route.profile)
        .is_none_or(|p| !Arc::ptr_eq(&p, peer));
    let live = inner.grant(&grant.ident).cloned();
    let lost = matches!(exc.code.as_str(), "BrowserDisconnected" | "OutcomeUnknown")
        && gone_peer
        && live.as_ref().is_some_and(|g| {
            g.grace_until.is_none() && g.generation.as_deref() == Some(peer.generation.as_str())
        });
    if exc.code == "TabNotFound" && grant.scope == "tab" {
        core.retire(&mut inner, &grant.ident, "tab_closed");
    } else if exc.code == "UnknownGrant" || lost {
        core.retire(&mut inner, &grant.ident, "browser_gone");
    }
    Err(exc)
}

/// The session's live lease named by the frame, inside the grant's scope (`Pages.leased`).
pub(crate) fn leased(
    core: &Core,
    session: &Session,
    grant: &Grant,
    request: &Json,
) -> Result<Leased, Refusal> {
    let token = request_string(request, "lease")?;
    let stale = || Refusal::new("StaleLease");
    let now = core.lease_clock.now();
    let mut inner = core.lock();
    let Some(claim) = inner
        .claims
        .iter()
        .find(|c| c.owner == session.owner && c.token == token)
        .cloned()
    else {
        return Err(stale());
    };
    if inner
        .active(&claim.generation, claim.tab, now)
        .is_none_or(|c| c.token != claim.token)
    {
        return Err(stale());
    }
    let peer = inner
        .peer(&grant.route.profile)
        .filter(|p| p.generation == claim.generation);
    let Some(peer) = peer else {
        inner.drop_claim(&claim.token);
        return Err(stale());
    };
    if grant.scope == "tab" && Some(claim.tab) != grant.tab {
        inner.drop_claim(&claim.token);
        return Err(Refusal::new("OutOfScope").with("tab", claim.tab));
    }
    Ok(Leased {
        token: claim.token,
        tab: claim.tab,
        document: claim.document,
        origin: claim.origin,
        peer,
    })
}

fn opt_str(v: Option<&Json>) -> Option<String> {
    v.and_then(Json::as_str).map(str::to_owned)
}

fn reply_with(head: Vec<(&str, Json)>, result: Json) -> Json {
    let mut reply = Json::object(head);
    if let Json::Object(pairs) = result {
        for (k, v) in pairs {
            reply.insert(k, v);
        }
    }
    reply
}

pub(crate) fn ok_tab(tab: i64) -> Vec<(&'static str, Json)> {
    vec![("ok", Json::Bool(true)), ("tab", Json::Int(tab))]
}

/// Run a validated op; returns the reply, the operated tab and the result host (`Pages.run`).
pub async fn run(
    core: &Arc<Core>,
    session: &mut Session,
    grant: &Grant,
    op: &str,
    request: &Json,
) -> Result<Ran, Stop> {
    if op == "tabs" {
        return Ok(tabs(core, grant).await?);
    }
    if op == "select" {
        let Some(Json::Int(tab)) = request.get("tab").cloned() else {
            return Err(Refusal::new("InvalidRequest").into());
        };
        return select(core, session, grant, tab).await;
    }
    if MUTATING.contains(&op) {
        let rid = request_string(request, "request_id")?;
        if let Some((copy, reply)) = session.journal.get(rid) {
            if !pages::py_eq(&pages::remembered(request), copy) {
                return Err(Refusal::new("Conflict").into());
            }
            let seen = match reply.get("tab") {
                Some(Json::Int(t)) => Some(*t),
                _ => None,
            };
            return Ok((reply.clone(), seen, None));
        }
    }
    if let Some(ran) = capture::route(core, session, grant, op, request).await {
        return ran;
    }
    if let Some(ran) = frames::route(core, session, grant, op, request).await {
        return ran;
    }
    let claim = leased(core, session, grant, request)?;
    let tab = claim.tab;
    match op {
        "release" => {
            core.lock().drop_claim(&claim.token);
            return Ok((Json::object([("ok", Json::Bool(true))]), Some(tab), None));
        }
        "switch" => return switch(core, grant, &claim).await,
        "page_read" | "page_find" => return query(core, grant, &claim, op, request).await,
        "page_downloads" => return downloads(core, grant, &claim).await,
        _ => {}
    }
    let rid = request_string(request, "request_id")?.to_owned();
    if session.journal.len() >= JOURNAL_LIMIT {
        return Err(Refusal::new("SessionFull").into());
    }
    let field = |k: &str| request.get(k).cloned().unwrap_or(Json::Null);
    let mut args = vec![("tab", Json::Int(tab))];
    if op == "page_navigate" {
        args.push(("url", field("url")));
    } else if !HISTORY.contains(&op) {
        let document = claim.document.clone().map_or(Json::Null, Json::from);
        // #11362: a frame ref from page_snapshot goes to the worker as {frame, loader, node}.
        let r#ref = frames::resolve(session, &claim, &field("ref"))?;
        args.extend([("document", document), ("ref", r#ref)]);
        match op {
            "page_click" => args.extend(pages::controls(request)?),
            "page_type" => args.push(("text", field("text"))),
            "page_scroll" => args.extend([("to", field("to")), ("wait_ms", field("wait_ms"))]),
            "page_upload" => args.push(("files", field("files"))),
            "page_press" => args.extend([("key", field("key")), ("modifiers", field("modifiers"))]),
            "page_select" => args.push(("value", field("value"))),
            "page_drag" => args.push((
                "target",
                frames::resolve(session, &claim, &field("target"))?,
            )),
            _ => {}
        }
    }
    let raw = match send(core, grant, claim.key(), &claim.peer, op, args).await {
        Ok(raw) => raw,
        Err(exc) => {
            // #11352: TextDropped reports typing that ran, so a retry must not type again.
            if matches!(exc.code.as_str(), "OutcomeUnknown" | "TextDropped") {
                let copy = pages::remembered(request);
                session.journal.insert(rid, (copy, exc.response()));
            }
            return Err(exc.into());
        }
    };
    let (result, where_) = if op == "page_navigate" || HISTORY.contains(&op) {
        let result = pages::shaped(&raw, pages::LANDED)?;
        let document = opt_str(result.get("document"));
        let access = result.get("access") == Some(&Json::Bool(true));
        let origin = opt_str(result.get("origin")).filter(|_| access);
        core.lock().with_claim(&claim.token, |c| {
            c.document = document;
            c.origin = origin.clone();
        });
        (result, hosted(origin.as_deref())?)
    } else {
        let cut = matches!(op, "page_type" | "page_press") && raw.get("truncated").is_some();
        let shape: Shape = match op {
            "page_scroll" => pages::SCROLLED,
            "page_select" => pages::SELECTED,
            "page_drag" => pages::DRAGGED,
            _ if cut => pages::CUT,
            _ => pages::DONE,
        };
        let result = if op == "page_click" {
            pages::clicked(&raw)?
        } else {
            pages::shaped(&raw, shape)?
        };
        (result, hosted(claim.origin.as_deref())?)
    };
    renew(core, &claim.token);
    let reply = reply_with(ok_tab(tab), result);
    session
        .journal
        .insert(rid, (pages::remembered(request), reply.clone()));
    Ok((reply, Some(tab), where_))
}

pub(crate) fn renew(core: &Core, token: &str) {
    let deadline = core.lease_clock.now() + TTL;
    core.lock().with_claim(token, |c| c.deadline = deadline);
}

async fn tabs(core: &Core, grant: &Grant) -> Result<Ran, Refusal> {
    let peer = peer_of(core, grant)?;
    let result = send(core, grant, None, &peer, "page_tabs", Vec::new()).await?;
    let Some(Json::Array(entries)) = result.get("tabs") else {
        return Err(Refusal::new("BrowserRefused"));
    };
    let keys = [
        "tab", "window", "index", "active", "access", "origin", "title",
    ];
    let in_scope = |e: &&Json| matches!(e.get("tab"), Some(Json::Int(t)) if grant.scope != "tab" || Some(*t) == grant.tab);
    let tabs = entries
        .iter()
        .filter(in_scope)
        .map(|e| Json::object(keys.map(|k| (k, e.get(k).cloned().unwrap_or(Json::Null)))))
        .collect();
    let reply = Json::object([("ok", Json::Bool(true)), ("tabs", Json::Array(tabs))]);
    Ok((reply, None, None))
}

pub(crate) async fn query(
    core: &Core,
    grant: &Grant,
    claim: &Leased,
    op: &str,
    request: &Json,
) -> Result<Ran, Stop> {
    let document = claim.document.clone().map_or(Json::Null, Json::from);
    let mut args = vec![("tab", Json::Int(claim.tab)), ("document", document)];
    args.extend(pages::controls(request)?);
    let raw = send(core, grant, claim.key(), &claim.peer, op, args).await?;
    let shape = if op == "page_read" {
        pages::READ
    } else {
        pages::FOUND
    };
    let result = pages::shaped(&raw, shape)?;
    let document = opt_str(result.get("document"));
    let origin = opt_str(result.get("origin"));
    let changed = document != claim.document;
    let deadline = core.lease_clock.now() + TTL;
    core.lock().with_claim(&claim.token, |c| {
        c.document.clone_from(&document);
        c.origin.clone_from(&origin);
        c.deadline = deadline;
    });
    let mut reply = reply_with(ok_tab(claim.tab), result);
    reply.insert("document_changed", Json::Bool(changed));
    Ok((reply, Some(claim.tab), hosted(origin.as_deref())?))
}

async fn downloads(core: &Core, grant: &Grant, claim: &Leased) -> Result<Ran, Stop> {
    let args = vec![("tab", Json::Int(claim.tab))];
    let raw = send(
        core,
        grant,
        claim.key(),
        &claim.peer,
        "page_downloads",
        args,
    )
    .await?;
    let listed = pages::shaped(&raw, pages::LISTED)?;
    let found = match listed.get("downloads") {
        Some(Json::Array(items)) => items
            .iter()
            .take(20)
            .map(|d| pages::shaped(d, pages::DOWNLOAD))
            .collect::<Result<Vec<_>, _>>()?,
        _ => Vec::new(),
    };
    renew(core, &claim.token);
    let origin = listed.get("origin").cloned().unwrap_or(Json::Null);
    let mut reply = Json::object(ok_tab(claim.tab));
    reply.insert("origin", origin.clone());
    reply.insert("downloads", Json::Array(found));
    Ok((reply, Some(claim.tab), hosted(origin.as_str())?))
}

/// Make the leased tab the active tab of its window and focus that window (`Pages.switch`).
///
/// Why: an agent on a profile grant must reach a tab a link opened, and the
/// human must see which tab it drives (#11293). What: needs this session's own
/// lease; a tab grant answers `OutOfScope`; idempotent, so not journaled.
/// Test: `page_ops_replay_byte_identically` (`switch`, `b-switch-foreign-lease`).
async fn switch(core: &Core, grant: &Grant, claim: &Leased) -> Result<Ran, Stop> {
    if grant.scope == "tab" {
        return Err(Refusal::new("OutOfScope").with("tab", claim.tab).into());
    }
    let args = vec![("tab", Json::Int(claim.tab))];
    let raw = send(core, grant, claim.key(), &claim.peer, "page_switch", args).await?;
    let result = pages::shaped(&raw, pages::SWITCHED)?;
    renew(core, &claim.token);
    let reply = reply_with(ok_tab(claim.tab), result);
    Ok((reply, Some(claim.tab), hosted(claim.origin.as_deref())?))
}

/// Take or return this session's lease on a tab; one controller per tab (`Pages.select`).
///
/// Why: two agents driving one tab would race each other's clicks. What: a
/// tab another session holds answers `TabClaimed` with its holder and expiry;
/// a new lease is taken before the `page_tab` probe and dropped if the probe
/// fails; an existing one keeps its token.
/// Test: `page_ops_replay_byte_identically` (`b-select-claimed`, `b-select-repeat`,
/// `select-malformed-probe`).
async fn select(core: &Arc<Core>, session: &Session, grant: &Grant, tab: i64) -> Result<Ran, Stop> {
    if grant.scope == "tab" && Some(tab) != grant.tab {
        return Err(Refusal::new("OutOfScope").with("tab", tab).into());
    }
    let peer = peer_of(core, grant)?;
    let now = core.lease_clock.now();
    let (token, created) = {
        let mut inner = core.lock();
        match inner.active(&peer.generation, tab, now) {
            Some(c) if c.owner != session.owner => {
                let expires = core.wall_clock.now() + (c.deadline - now).max(0.0);
                return Err(Refusal::new("TabClaimed")
                    .with("holder", c.owner.as_str())
                    .with("expires_at", Json::Float(expires))
                    .into());
            }
            Some(c) => (c.token.clone(), false),
            None => {
                let token = token_urlsafe(&*core.entropy, 24);
                let claim = claim_for(session, &peer.generation, tab, token.clone(), now);
                inner.claims.push(claim);
                (token, true)
            }
        }
    };
    let key = Some((peer.generation.clone(), tab));
    let args = vec![("tab", Json::Int(tab))];
    let probed = send(core, grant, key, &peer, "page_tab", args)
        .await
        .and_then(|seen| pages::shaped(&seen, pages::PROBE));
    let mut inner = core.lock();
    let still = inner
        .claims
        .iter()
        .any(|c| c.generation == peer.generation && c.tab == tab && c.token == token);
    let seen = match probed.and_then(|p| {
        if still {
            Ok(p)
        } else {
            Err(Refusal::new("StaleLease"))
        }
    }) {
        Ok(p) => p,
        Err(r) => {
            if created {
                inner.drop_claim(&token);
            }
            return Err(r.into());
        }
    };
    let access = seen.get("access") == Some(&Json::Bool(true));
    let origin = opt_str(seen.get("origin"));
    let deadline = core.lease_clock.now() + TTL;
    inner.with_claim(&token, |c| {
        c.document = opt_str(seen.get("document"));
        c.deadline = deadline;
        c.origin = origin.clone().filter(|_| access);
    });
    drop(inner);
    let head = vec![
        ("ok", Json::Bool(true)),
        ("lease", Json::from(token)),
        ("tab", Json::Int(tab)),
    ];
    let where_ = if access {
        hosted(origin.as_deref())?
    } else {
        None
    };
    Ok((reply_with(head, seen), Some(tab), where_))
}

/// A fresh code-session lease on `tab`, expiring [`TTL`] after `now`.
pub(crate) fn claim_for(
    session: &Session,
    generation: &str,
    tab: i64,
    token: String,
    now: f64,
) -> Claim {
    Claim {
        generation: generation.to_owned(),
        tab,
        owner: session.owner.clone(),
        owner_session: session.id,
        token,
        deadline: now + TTL,
        busy: false,
        document: None,
        origin: None,
        indicator: false,
    }
}
