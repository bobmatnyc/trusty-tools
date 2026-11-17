//! `service.sock`: the paired extension worker (`Broker.browser`, `Broker.pair`).
//!
//! Why: only a worker that proves it holds the profile's browser key may
//! receive page commands; the proof binds both nonces, the profile, the email
//! and the browser incarnation, so neither side can replay the other.
//! What: `hello` (exact key set) -> route lookup -> email and nonce checks ->
//! `challenge{nonce, proof}` -> `proof` checked in constant time, all within
//! 5 s; then the route is re-read (a key rotated mid-handshake is refused),
//! one worker per profile, grants rebind, and the ack carries the generation,
//! the live grant ids and the published asset stamp. Replies then resolve
//! pending commands by id.
//! Test: `captured_exchanges_replay_byte_identically` (hello refusals, proof
//! mismatch, pair handshake) in `tests/parity_broker.rs`.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use crate::broker::{Core, Inner, peer_ok, refused, relayed};
use crate::entropy::{token_hex, uuid4};
use crate::pairing::{self, Fault, Route};
use crate::peer_conn::Peer;
use crate::proof::{Hello, Role, pairing_proof, proofs_equal};
use crate::pyjson::Json;
use crate::refusal::{Refusal, request_string};
use crate::wire::{WireError, read_frame, write_frame};

const HELLO_FIELDS: [&str; 5] = ["type", "profile", "email", "incarnation", "nonce"];

/// How a service.sock exchange stopped.
enum Stop {
    /// Answer with the relayed refusal.
    Refused(Refusal),
    /// Close without a reply (`EOFError`, `OSError`, `ValueError`, `TimeoutError`).
    Quiet,
}

impl From<WireError> for Stop {
    fn from(e: WireError) -> Stop {
        e.refusal().map_or(Stop::Quiet, Stop::Refused)
    }
}

impl From<Refusal> for Stop {
    fn from(r: Refusal) -> Stop {
        Stop::Refused(r)
    }
}

impl From<Fault> for Stop {
    fn from(f: Fault) -> Stop {
        match f {
            Fault::Refused(r) => Stop::Refused(r),
            Fault::Internal(_) => Stop::Quiet,
        }
    }
}

fn route(core: &Core, profile: &str, request: &Json) -> Result<Route, Stop> {
    let found = pairing::profile_row(&core.lock().db, profile)?;
    found.ok_or_else(|| Stop::Refused(refused(core, "service", "route_unknown", Some(request))))
}

fn is_nonce(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// The pairing handshake; returns the route and the browser incarnation.
async fn pair(
    core: &Core,
    reader: &mut OwnedReadHalf,
    writer: &mut OwnedWriteHalf,
) -> Result<(Route, String), Stop> {
    let hello = read_frame(reader).await?;
    if !pairing::key_set_is(&hello, &HELLO_FIELDS)
        || hello.get("type") != Some(&Json::from("hello"))
    {
        return Err(Refusal::new("InvalidRequest").into());
    }
    let profile = request_string(&hello, "profile")?;
    let row = route(core, profile, &hello)?;
    let email = request_string(&hello, "email")?;
    if row.email != crate::casefold::casefold(email) {
        return Err(Refusal::new("ProfileMismatch").into());
    }
    let client_nonce = request_string(&hello, "nonce")?;
    if !is_nonce(client_nonce) {
        return Err(Refusal::new("InvalidNonce").into());
    }
    let incarnation = request_string(&hello, "incarnation")?;
    let nonce = token_hex(&*core.entropy, 32);
    let key = row.key(&core.vault)?;
    let fields = Hello {
        profile,
        email,
        incarnation,
        nonce: client_nonce,
    };
    let proof = |role| pairing_proof(&key, role, &fields, &nonce).map_err(|_| Stop::Quiet);
    let challenge = Json::object([
        ("type", Json::from("challenge")),
        ("nonce", Json::from(nonce.as_str())),
        ("proof", Json::from(proof(Role::Broker)?)),
    ]);
    write_frame(writer, &challenge).await?;
    let response = read_frame(reader).await?;
    let expected = proof(Role::Extension)?;
    if response.get("type") != Some(&Json::from("proof")) {
        return Err(Stop::Refused(refused(
            core,
            "service",
            "proof_mismatch",
            Some(&hello),
        )));
    }
    if !proofs_equal(&expected, request_string(&response, "proof")?) {
        return Err(Stop::Refused(refused(
            core,
            "service",
            "proof_mismatch",
            Some(&hello),
        )));
    }
    Ok((row, incarnation.to_owned()))
}

impl Core {
    /// Drop a worker: its claims, its pending commands, and start its grants' grace (`forget`).
    pub fn forget(&self, inner: &mut Inner, peer: &Arc<Peer>) {
        inner.peers.retain(|p| !Arc::ptr_eq(p, peer));
        inner.claims.retain(|c| c.generation != peer.generation);
        peer.kill();
        self.peer_gone(inner, peer);
    }
}

/// The published extension's asset stamp (`assets.published`).
pub fn published(state: &std::path::Path) -> Option<String> {
    crate::assets::published(state)
}

async fn register(
    core: &Arc<Core>,
    writer: OwnedWriteHalf,
    row: Route,
    incarnation: String,
) -> Result<Arc<Peer>, (Stop, Option<OwnedWriteHalf>)> {
    let request = Json::object([("profile", Json::from(row.profile.as_str()))]);
    let current = match route(core, &row.profile, &request) {
        Ok(c) => c,
        Err(stop) => return Err((stop, Some(writer))),
    };
    if current.browser_key != row.browser_key || current.ticket != row.ticket {
        let dict = Json::object([
            ("profile", Json::from(row.profile.as_str())),
            ("email", Json::from(row.email.as_str())),
        ]);
        return Err((
            Stop::Refused(refused(core, "service", "key_rotated", Some(&dict))),
            Some(writer),
        ));
    }
    let mut inner = core.lock();
    if inner.peer(&row.profile).is_some() {
        return Err((Refusal::new("DuplicateProfile").into(), Some(writer)));
    }
    let peer = Peer::new(writer, &row, incarnation, uuid4(&*core.entropy));
    inner.peers.push(Arc::clone(&peer));
    core.peer_back(&mut inner, &peer);
    Ok(peer)
}

/// Serve one `service.sock` connection.
pub async fn browser(core: Arc<Core>, stream: UnixStream) {
    if let Err(r) = peer_ok(&core, &stream, "service") {
        let (_, mut writer) = stream.into_split();
        let _ = write_frame(&mut writer, &relayed(&r)).await;
        return;
    }
    let (mut reader, mut writer) = stream.into_split();
    let paired = tokio::time::timeout(
        Duration::from_secs(5),
        pair(&core, &mut reader, &mut writer),
    )
    .await;
    let (row, incarnation) = match paired {
        Ok(Ok(found)) => found,
        Ok(Err(stop)) => return answer(stop, &mut writer).await,
        Err(_) => return,
    };
    let peer = match register(&core, writer, row, incarnation).await {
        Ok(peer) => peer,
        Err((stop, writer)) => {
            if let Some(mut w) = writer {
                answer(stop, &mut w).await;
            }
            return;
        }
    };
    let stop = serve_peer(&core, &peer, &mut reader).await;
    if let Stop::Refused(r) = stop {
        let _ = peer.send(&relayed(&r)).await;
    }
    let mut inner = core.lock();
    core.forget(&mut inner, &peer);
}

async fn answer(stop: Stop, writer: &mut OwnedWriteHalf) {
    if let Stop::Refused(r) = stop {
        let _ = write_frame(writer, &relayed(&r)).await;
    }
}

async fn serve_peer(core: &Arc<Core>, peer: &Arc<Peer>, reader: &mut OwnedReadHalf) -> Stop {
    let live = {
        let inner = core.lock();
        Core::live(&inner, &peer.profile)
    };
    let mut ack = Json::object([
        ("ok", Json::Bool(true)),
        ("generation", Json::from(peer.generation.as_str())),
        (
            "live",
            Json::Array(live.into_iter().map(Json::from).collect()),
        ),
    ]);
    if let Some(stamp) = published(&core.state) {
        ack.insert("assets", Json::from(stamp));
    }
    if let Err(e) = peer.send(&ack).await {
        return e.into();
    }
    loop {
        let reply = tokio::select! {
            r = read_frame(reader) => r,
            () = peer.killed() => return Stop::Quiet,
        };
        let reply = match reply {
            Ok(r) => r,
            Err(e) => return e.into(),
        };
        let id = match request_string(&reply, "id") {
            Ok(id) => id.to_owned(),
            Err(r) => return Stop::Refused(r),
        };
        if !peer.deliver(&id, reply) {
            return Stop::Refused(Refusal::new("UnexpectedReply"));
        }
    }
}
