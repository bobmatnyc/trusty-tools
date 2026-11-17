//! `agent.sock`: `profiles`, `attach`, and the session's op loop (`Broker.agent`, `Pages.dispatch`).
//!
//! Why: a local agent reaches a profile only through a grant Connect made;
//! every op is admitted and resolved in the fail-closed audit log, and a
//! grant that ended answers `GrantEnded` and closes the session.
//! What: the dispatch frame of `pages.py` (liveness, the `admitted` line,
//! validation, the op, the result line) with the `tabs` and `select` ops.
//! The other page ops (`page_*`, `release`, `switch`) arrive with leases in
//! phase 3 of the port plan; until then they are refused `InvalidRequest`
//! after the same audit lines.
//! Test: `captured_exchanges_replay_byte_identically` (profiles, attach,
//! tabs, select, unknown op) in `tests/parity_broker.rs`.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::Notify;

use crate::broker::{Claim, Core, SessionRef, peer_ok, relayed};
use crate::entropy::{Monotonic, token_urlsafe};
use crate::grants::{Grant, gone, hostname, with};
use crate::pairing::{self, Fault};
use crate::peer_conn::Peer;
use crate::pyjson::Json;
use crate::refusal::Refusal;
use crate::wire::{WireError, read_frame, read_frame_within, write_frame};

/// Lease lifetime in seconds (`leases.TTL`).
pub const TTL: f64 = 60.0;
/// Every session op name `pages.OPS` knows, for the audit `op` field.
pub const OPS: &[&str] = &[
    "tabs",
    "select",
    "page_read",
    "page_click",
    "page_hover",
    "page_type",
    "page_scroll",
    "page_press",
    "page_select",
    "page_drag",
    "page_navigate",
    "page_upload",
    "page_downloads",
    "page_find",
    "page_back",
    "page_forward",
    "page_reload",
    "release",
    "switch",
];
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
];

/// One attached agent.
struct Session {
    id: u64,
    owner: String,
    grant: String,
}

type Ran = (Json, Option<i64>, Option<String>);

/// Serve one `agent.sock` connection.
pub async fn agent(core: Arc<Core>, stream: UnixStream) {
    let checked = peer_ok(&core, &stream, "agent");
    let (mut reader, mut writer) = stream.into_split();
    if let Err(r) = checked {
        let _ = write_frame(&mut writer, &relayed(&r)).await;
        return;
    }
    let hello = match read_frame_within(&mut reader, 5.0).await {
        Ok(h) => h,
        Err(e) => return refuse_wire(e, &mut writer).await,
    };
    let op = hello.get("op").and_then(Json::as_str);
    if op == Some("profiles") {
        let reply = if pairing::key_set_is(&hello, &["op"]) {
            let mut inner = core.lock();
            core.expire(&mut inner, core.clock.now());
            Json::object([
                ("ok", Json::Bool(true)),
                ("profiles", Core::profiles(&inner)),
            ])
        } else {
            relayed(&Refusal::new("InvalidRequest"))
        };
        let _ = write_frame(&mut writer, &reply).await;
        return;
    }
    let attached = if op == Some("attach") {
        let mut inner = core.lock();
        core.attach(&mut inner, &hello).map(|(grant, id)| {
            let owner = token_urlsafe(&*core.entropy, 12);
            let kill = Arc::new(Notify::new());
            let profile = inner
                .grant(&grant)
                .map(|g| g.route.profile.clone())
                .unwrap_or_default();
            inner.sessions.push(SessionRef {
                id,
                profile,
                kill: Arc::clone(&kill),
            });
            let ack = inner.grant(&grant).map(ack_of).unwrap_or(Json::Null);
            (Session { id, owner, grant }, kill, ack)
        })
    } else {
        Err(Refusal::new("InvalidRequest"))
    };
    let (session, kill, ack) = match attached {
        Ok(found) => found,
        Err(r) => {
            let _ = write_frame(&mut writer, &relayed(&r)).await;
            return;
        }
    };
    if write_frame(&mut writer, &ack).await.is_ok() {
        loop {
            let request = tokio::select! {
                r = read_frame(&mut reader) => r,
                () = kill.notified() => break,
            };
            let request = match request {
                Ok(r) => r,
                Err(e) => {
                    refuse_wire(e, &mut writer).await;
                    break;
                }
            };
            let response = dispatch(&core, &session, &request)
                .await
                .unwrap_or_else(|r| r.response());
            if write_frame(&mut writer, &response).await.is_err() {
                break;
            }
            if response.get("error") == Some(&Json::from("GrantEnded")) {
                break;
            }
        }
    }
    let mut inner = core.lock();
    inner.sessions.retain(|s| s.id != session.id);
    core.close_session(&mut inner, &session.grant, session.id);
    inner.claims.retain(|c| c.owner != session.owner);
}

async fn refuse_wire(e: WireError, writer: &mut OwnedWriteHalf) {
    if let Some(r) = e.refusal() {
        let _ = write_frame(writer, &relayed(&r)).await;
    }
}

fn ack_of(g: &Grant) -> Json {
    Json::object([
        ("ok", Json::Bool(true)),
        ("grant", Json::from(g.ident.as_str())),
        ("scope", Json::from(g.scope.as_str())),
        ("profile", Json::from(g.route.profile.as_str())),
        ("email", Json::from(g.route.email.as_str())),
    ])
}

fn fault(f: Fault) -> Refusal {
    match f {
        Fault::Refused(r) => r,
        // A registry failure mid-op: Python lets the sqlite3 error close the session.
        Fault::Internal(_) => Refusal::new("BrowserDisconnected"),
    }
}

fn current(core: &Core, ident: &str) -> Result<Grant, Refusal> {
    core.lock()
        .grant(ident)
        .cloned()
        .ok_or_else(|| Refusal::new("GrantEnded").with("reason", "revoked"))
}

fn line(
    grant: &Grant,
    extra: Vec<(&'static str, Option<Json>)>,
) -> Vec<(&'static str, Option<Json>)> {
    with(
        with(grant.fields(), vec![("tab", None), ("origin", None)]),
        extra,
    )
}

fn touch(core: &Core, ident: &str) {
    let now = core.clock.now();
    if let Some(g) = core.lock().grants.iter_mut().find(|g| g.ident == ident) {
        g.last_used = now;
    }
}

/// Run one session op in the contract's check order (`Pages.dispatch`).
async fn dispatch(core: &Arc<Core>, session: &Session, request: &Json) -> Result<Json, Refusal> {
    {
        let mut inner = core.lock();
        core.check_live(&mut inner, &session.grant).map_err(fault)?;
    }
    let grant = current(core, &session.grant)?;
    if !grant.active {
        return Err(gone(&grant));
    }
    let op = request.get("op").and_then(Json::as_str);
    let name = op.filter(|o| OPS.contains(o)).unwrap_or("invalid");
    let admitted = line(
        &grant,
        vec![
            ("op", Some(name.into())),
            ("outcome", Some("admitted".into())),
        ],
    );
    core.record("operation", &admitted)?;
    touch(core, &grant.ident);
    let ran = match validate(request) {
        Ok(op) => run(core, session, &grant, op, request).await,
        Err(r) => Err(r),
    };
    let after = current(core, &grant.ident)?;
    let (reply, tab, origin) = match ran {
        Ok(ran) => ran,
        Err(exc) => {
            let seen = exc
                .details
                .iter()
                .find(|(k, _)| k == "tab")
                .and_then(|(_, v)| match v {
                    Json::Int(t) => Some(Json::Int(*t)),
                    _ => None,
                });
            let extra = vec![
                ("op", Some(name.into())),
                ("outcome", Some(exc.code.as_str().into())),
                ("tab", seen),
            ];
            core.audit.best_effort("operation", &line(&after, extra));
            if !after.active && exc.code != "OutcomeUnknown" {
                return Err(gone(&after));
            }
            return Err(exc);
        }
    };
    if !after.active {
        let extra = vec![
            ("op", Some(name.into())),
            ("outcome", Some("grant_ended".into())),
            ("tab", tab.map(Json::Int)),
        ];
        core.audit.best_effort("operation", &line(&after, extra));
        return Err(gone(&after));
    }
    touch(core, &grant.ident);
    let outcome = match (reply.get("ok"), reply.get("error")) {
        (Some(Json::Bool(false)), Some(e)) => e.clone(),
        _ => Json::from("ok"),
    };
    let extra = vec![
        ("op", Some(name.into())),
        ("outcome", Some(outcome)),
        ("tab", tab.map(Json::Int)),
        ("origin", origin.map(Json::from)),
    ];
    if core.audit.write("operation", &line(&after, extra)).is_err() {
        // The op ran but is unrecorded: end the grant and release no result.
        core.retire(&mut core.lock(), &grant.ident, "audit");
        return Err(gone(&current(core, &grant.ident).unwrap_or(after)));
    }
    Ok(reply)
}

/// Exact fields and types for the ops this phase runs; returns the op name.
fn validate(request: &Json) -> Result<&'static str, Refusal> {
    let op = request.get("op").and_then(Json::as_str);
    let (op, fields): (&'static str, &[&str]) = match op {
        Some("tabs") => ("tabs", &["op"]),
        Some("select") => ("select", &["op", "tab"]),
        _ => return Err(Refusal::new("InvalidRequest")),
    };
    if !pairing::key_set_is(request, fields) {
        return Err(Refusal::new("InvalidRequest"));
    }
    if let Some(tab) = request.get("tab")
        && !matches!(tab, Json::Int(t) if *t >= 0)
    {
        return Err(Refusal::new("InvalidRequest"));
    }
    Ok(op)
}

fn peer_of(core: &Core, grant: &Grant) -> Result<Arc<Peer>, Refusal> {
    let peer = core.lock().peer(&grant.route.profile);
    peer.filter(|p| Some(p.generation.as_str()) == grant.generation.as_deref())
        .ok_or_else(|| Refusal::new("BrowserDisconnected"))
}

/// One page command with the contract's grant and lease fate on refusal (`Pages.send`).
async fn send(
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
            if let Some(c) = inner
                .claims
                .iter_mut()
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

async fn run(
    core: &Arc<Core>,
    session: &Session,
    grant: &Grant,
    op: &str,
    request: &Json,
) -> Result<Ran, Refusal> {
    if op == "tabs" {
        let peer = peer_of(core, grant)?;
        let result = send(core, grant, None, &peer, "page_tabs", Vec::new()).await?;
        let Some(Json::Array(entries)) = result.get("tabs") else {
            return Err(Refusal::new("BrowserRefused"));
        };
        let keys = [
            "tab", "window", "index", "active", "access", "origin", "title",
        ];
        let tabs = entries
            .iter()
            .filter(|e| matches!(e, Json::Object(_)) && matches!(e.get("tab"), Some(Json::Int(t)) if grant.scope != "tab" || Some(*t) == grant.tab))
            .map(|e| Json::object(keys.map(|k| (k, e.get(k).cloned().unwrap_or(Json::Null)))))
            .collect();
        return Ok((
            Json::object([("ok", Json::Bool(true)), ("tabs", Json::Array(tabs))]),
            None,
            None,
        ));
    }
    let Some(Json::Int(tab)) = request.get("tab").cloned() else {
        return Err(Refusal::new("InvalidRequest"));
    };
    select(core, session, grant, tab).await
}

fn shaped_probe(value: &Json) -> Result<(bool, Option<String>, Option<String>, Json), Refusal> {
    let refused = || Refusal::new("BrowserRefused");
    if !pairing::key_set_is(value, &["access", "document", "origin", "title"]) {
        return Err(refused());
    }
    let opt = |k: &str| match value.get(k) {
        Some(Json::Str(s)) => Ok(Some(s.clone())),
        Some(Json::Null) => Ok(None),
        _ => Err(refused()),
    };
    let Some(Json::Bool(access)) = value.get("access") else {
        return Err(refused());
    };
    let (document, origin) = (opt("document")?, opt("origin")?);
    opt("title")?;
    Ok((*access, document, origin, value.clone()))
}

/// Take or return this session's lease on a tab (`Pages.select`).
async fn select(
    core: &Arc<Core>,
    session: &Session,
    grant: &Grant,
    tab: i64,
) -> Result<Ran, Refusal> {
    if grant.scope == "tab" && Some(tab) != grant.tab {
        return Err(Refusal::new("OutOfScope").with("tab", tab));
    }
    let peer = peer_of(core, grant)?;
    let now = core.lease_clock.now();
    let (token, created) = {
        let mut inner = core.lock();
        let peer_gen = peer.generation.clone();
        inner.claims.retain(|c| {
            !(c.generation == peer_gen && c.tab == tab && !c.busy && c.deadline <= now)
        });
        match inner
            .claims
            .iter()
            .find(|c| c.generation == peer_gen && c.tab == tab)
        {
            Some(c) if c.owner != session.owner => {
                let wall = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0.0, |d| d.as_secs_f64());
                let expires = wall + (c.deadline - now).max(0.0);
                return Err(Refusal::new("TabClaimed")
                    .with("holder", c.owner.as_str())
                    .with("expires_at", Json::Float(expires)));
            }
            Some(c) => (c.token.clone(), false),
            None => {
                let token = token_urlsafe(&*core.entropy, 24);
                inner.claims.push(Claim {
                    generation: peer_gen,
                    tab,
                    owner: session.owner.clone(),
                    owner_session: session.id,
                    token: token.clone(),
                    deadline: now + TTL,
                    busy: false,
                    document: None,
                    origin: None,
                });
                (token, true)
            }
        }
    };
    let key = (peer.generation.clone(), tab);
    let probed = send(
        core,
        grant,
        Some(key.clone()),
        &peer,
        "page_tab",
        vec![("tab", Json::Int(tab))],
    )
    .await
    .and_then(|seen| shaped_probe(&seen));
    let mut inner = core.lock();
    let still = inner
        .claims
        .iter()
        .any(|c| c.generation == key.0 && c.tab == tab && c.token == token);
    let probed = probed.and_then(|p| {
        if still {
            Ok(p)
        } else {
            Err(Refusal::new("StaleLease"))
        }
    });
    let (access, document, origin, seen) = match probed {
        Ok(p) => p,
        Err(r) => {
            if created {
                inner.claims.retain(|c| c.token != token);
            }
            return Err(r);
        }
    };
    let lease_now = core.lease_clock.now();
    if let Some(c) = inner.claims.iter_mut().find(|c| c.token == token) {
        c.document = document;
        c.deadline = lease_now + TTL;
        c.origin = if access { origin.clone() } else { None };
    }
    let mut reply = Json::object([
        ("ok", Json::Bool(true)),
        ("lease", Json::from(token)),
        ("tab", Json::Int(tab)),
    ]);
    if let Json::Object(pairs) = seen {
        for (k, v) in pairs {
            reply.insert(k, v);
        }
    }
    let host = if access {
        hostname(origin.as_deref())
    } else {
        None
    };
    Ok((reply, Some(tab), host))
}
