//! `agent.sock`: `profiles`, `attach`, and the session's op loop (`Broker.agent`, `Pages.dispatch`).
//!
//! Why: a local agent reaches a profile only through a grant Connect made;
//! every op is admitted and resolved in the fail-closed audit log, and a
//! grant that ended answers `GrantEnded` and closes the session.
//! What: [`agent`] serves one connection (`profiles`, or `attach` then the
//! op loop); [`dispatch`] is the check order of `Pages.dispatch` (liveness,
//! the fail-closed `admitted` line, validation, the op in `pages_ops`, the
//! result line). A mutating op whose grant ended mid-flight, or whose result
//! line failed, answers a journaled `OutcomeUnknown` instead of `GrantEnded`.
//! Test: `captured_exchanges_replay_byte_identically` in `tests/parity_broker.rs`;
//! `page_ops_replay_byte_identically` in `tests/parity_pages.rs`.

use std::sync::Arc;

use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::Notify;

use crate::broker::{Core, SessionRef, peer_ok, relayed};
use crate::entropy::token_urlsafe;
use crate::grants::{Grant, gone, with};
use crate::leases::clear;
use crate::pages::{self, Stop};
use crate::pages_ops::{Session, run};
use crate::pages_url::floor_host;
use crate::pairing::{self, Fault};
use crate::pyjson::Json;
use crate::refusal::{MUTATING, Refusal};
use crate::wire::{WireError, read_frame, read_frame_within, write_frame};

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
            let journal = Default::default();
            (
                Session {
                    id,
                    owner,
                    grant,
                    journal,
                    captures: Vec::new(),
                    frames: Vec::new(),
                },
                kill,
                ack,
            )
        })
    } else {
        Err(Refusal::new("InvalidRequest"))
    };
    let (mut session, kill, ack) = match attached {
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
            let response = match dispatch(&core, &mut session, &request).await {
                Ok(reply) => reply,
                Err(Stop::Refused(r)) => r.response(),
                // A Python ValueError (a malformed worker origin) closes the session.
                Err(Stop::Close) => break,
            };
            if write_frame(&mut writer, &response).await.is_err() {
                break;
            }
            if response.get("error") == Some(&Json::from("GrantEnded")) {
                break;
            }
        }
    }
    let held = {
        let mut inner = core.lock();
        inner.sessions.retain(|s| s.id != session.id);
        core.close_session(&mut inner, &session.grant, session.id);
        let held: Vec<_> = inner
            .claims
            .iter()
            .filter(|c| c.owner == session.owner)
            .cloned()
            .collect();
        inner.claims.retain(|c| c.owner != session.owner);
        held.into_iter()
            .map(|c| {
                (
                    inner
                        .peers
                        .iter()
                        .find(|p| p.generation == c.generation)
                        .cloned(),
                    c,
                )
            })
            .collect::<Vec<_>>()
    };
    for (peer, claim) in held {
        clear(&core, peer.as_deref(), &claim).await;
    }
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
///
/// Why: a code session must stay inside its grant whatever the worker does,
/// and no unrecorded result may leave the broker (#11239). What: liveness, the
/// fail-closed `admitted` line, validation and the op, then the result line; a
/// failed result line ends the grant with `audit`.
/// Test: `page_ops_replay_byte_identically`, `a_failed_result_line_ends_the_grant`
/// in `tests/parity_pages.rs`.
async fn dispatch(core: &Arc<Core>, session: &mut Session, request: &Json) -> Result<Json, Stop> {
    {
        let mut inner = core.lock();
        core.check_live(&mut inner, &session.grant).map_err(fault)?;
    }
    let grant = current(core, &session.grant)?;
    if !grant.active {
        return Err(gone(&grant).into());
    }
    let op = request.get("op").and_then(Json::as_str);
    let name = op
        .filter(|o| pages::op_entry(o).is_some())
        .unwrap_or("invalid");
    // #11208: an opened tab's target host is audited like a navigation's.
    let target = if matches!(name, "page_navigate" | "open_tab") {
        floor_host(request.get("url")).map(Json::from)
    } else {
        None
    };
    let admitted = line(
        &grant,
        vec![
            ("op", Some(name.into())),
            ("outcome", Some("admitted".into())),
            ("origin", target),
        ],
    );
    core.record("operation", &admitted)?;
    touch(core, &grant.ident);
    let ran = match pages::validate(request) {
        Ok(op) => run(core, session, &grant, op, request).await,
        Err(r) => Err(r.into()),
    };
    let after = current(core, &grant.ident)?;
    let (reply, tab, origin) = match ran {
        Ok(ran) => ran,
        Err(Stop::Close) => return Err(Stop::Close),
        Err(Stop::Refused(exc)) => {
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
                return Err(gone(&after).into());
            }
            return Err(exc.into());
        }
    };
    if !after.active {
        let extra = vec![
            ("op", Some(name.into())),
            ("outcome", Some("grant_ended".into())),
            ("tab", tab.map(Json::Int)),
        ];
        core.audit.best_effort("operation", &line(&after, extra));
        return unknown(session, name, request).ok_or_else(|| gone(&after).into());
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
        let ended = current(core, &grant.ident).unwrap_or(after);
        return unknown(session, name, request).ok_or_else(|| gone(&ended).into());
    }
    Ok(reply)
}

/// A mutating op's journaled `OutcomeUnknown`; `None` for a read.
fn unknown(session: &mut Session, name: &str, request: &Json) -> Option<Json> {
    if !MUTATING.contains(&name) {
        return None;
    }
    let reply = Refusal::new("OutcomeUnknown").response();
    let rid = request
        .get("request_id")
        .and_then(Json::as_str)
        .unwrap_or("");
    let copy = pages::remembered(request);
    session
        .journal
        .insert(rid.to_owned(), (copy, reply.clone()));
    Some(reply)
}
