//! `enrollment.sock` and `operator.sock` (`Broker.bootstrap`, `Broker.operator`).
//!
//! Why: the popup pairs, unpairs, connects and disconnects through the
//! native host on `enrollment.sock`; the operator CLI lists and revokes on
//! `operator.sock`. Their error replies differ (a safe-name whitelist and
//! `reason` codes for the popup, a flat `RevokeFailed` for the operator), and
//! clients branch on those bytes.
//! What: enrollment checks the uid, then the Chrome host (7 s bound, each
//! failure a `refused` audit line and an `Unauthorized` with `reason`), and
//! only then reads one frame (5 s). Pair and unpair are audited per outcome;
//! a same-email re-pair detaches the replaced profile.
//! Test: `captured_exchanges_replay_byte_identically` in `tests/parity_broker.rs`.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::UnixStream;

use crate::broker::{Core, peer_ok, refused};
use crate::grants::{OPERATOR_OPS, RELAYED};
use crate::pairing::{self, CREDENTIAL, Fault};
use crate::pyjson::Json;
use crate::refusal::{Refusal, request_string};
use crate::wire::{WireError, read_frame_within, write_frame};

/// Error names the popup may see verbatim; anything else is `EnrollmentFailed`.
const SAFE: &[&str] = &[
    "InvalidEnrollment",
    "EnrollmentConflict",
    "EnrollmentRevoked",
    "ReplacementStale",
    "CredentialConflict",
    "WorkflowCredentialOccupied",
    "Unauthorized",
];

fn failure_reply(fault: &Fault) -> Json {
    let refusal = match fault {
        Fault::Refused(r) => r,
        Fault::Internal(_) => return Refusal::new("EnrollmentFailed").response(),
    };
    if RELAYED.contains(&refusal.code.as_str()) {
        return refusal.response();
    }
    let code = if SAFE.contains(&refusal.code.as_str()) {
        refusal.code.as_str()
    } else {
        "EnrollmentFailed"
    };
    let mut reply = Refusal::new(code);
    if let Some((_, reason)) = refusal.details.iter().find(|(k, _)| k == "reason") {
        reply = reply.with("reason", reason.clone());
    }
    reply.response()
}

/// Serve one `enrollment.sock` connection.
pub async fn bootstrap(core: Arc<Core>, stream: UnixStream) {
    let outcome = enroll(&core, stream).await;
    if let Err((fault, mut stream)) = outcome {
        let _ = write_frame(&mut stream, &failure_reply(&fault)).await;
    }
}

type Failed = (Fault, UnixStream);

async fn enroll(core: &Arc<Core>, mut stream: UnixStream) -> Result<(), Failed> {
    if let Err(r) = peer_ok(core, &stream, "enrollment") {
        return Err((r.into(), stream));
    }
    let std_stream = match stream.as_fd_clone() {
        Ok(s) => s,
        Err(_) => {
            return Err((
                refused(core, "enrollment", "host_not_native_host", None).into(),
                stream,
            ));
        }
    };
    let (check, state) = (Arc::clone(&core.host_check), core.state.clone());
    let verdict = tokio::time::timeout(
        Duration::from_secs(7),
        tokio::task::spawn_blocking(move || check.verify(&std_stream, &state)),
    )
    .await;
    let reason = match verdict {
        Ok(Ok(Ok(()))) => None,
        Ok(Ok(Err(refusal))) => Some(refusal.code),
        Ok(Err(_)) => Some("host_not_native_host"),
        Err(_) => Some("host_timeout"),
    };
    if let Some(reason) = reason {
        return Err((refused(core, "enrollment", reason, None).into(), stream));
    }
    // Read the enrollment key only after OS authentication.
    let request = match read_frame_within(&mut stream, 5.0).await {
        Ok(r) => r,
        Err(WireError::Eof) => return Ok(()),
        Err(WireError::Invalid) => return Err((Refusal::new("InvalidFrame").into(), stream)),
        Err(e) => return Err((Fault::Internal(e.to_string()), stream)),
    };
    let reply = match enrollment_request(core, &request).await {
        Ok(reply) => reply,
        Err(fault) => return Err((fault, stream)),
    };
    match write_frame(&mut stream, &reply).await {
        Ok(()) => Ok(()),
        Err(e) => Err((Fault::Internal(e.to_string()), stream)),
    }
}

/// A std handle on the same socket, for the blocking host check.
trait FdClone {
    fn as_fd_clone(&self) -> std::io::Result<std::os::unix::net::UnixStream>;
}

impl FdClone for UnixStream {
    fn as_fd_clone(&self) -> std::io::Result<std::os::unix::net::UnixStream> {
        use std::os::fd::AsFd;
        let owned = self.as_fd().try_clone_to_owned()?;
        Ok(std::os::unix::net::UnixStream::from(owned))
    }
}

/// The popup's ops: `pair`, `unpair`, `connect`, `grants`, `revoke_grant(s)`.
pub async fn enrollment_request(core: &Arc<Core>, request: &Json) -> Result<Json, Fault> {
    let op = request_string(request, "op")?;
    if op == "pair" || op == "unpair" {
        return pairing_request(core, request, op);
    }
    if op == "connect" || OPERATOR_OPS.contains(&op) {
        let mut inner = core.lock();
        return core.popup_grants(&mut inner, request);
    }
    Err(Refusal::new("InvalidRequest").into())
}

fn pairing_request(core: &Arc<Core>, request: &Json, op: &str) -> Result<Json, Fault> {
    let result = if op == "pair" {
        pair(core, request)
    } else {
        unpair(core, request)
    };
    match result {
        Ok(reply) => {
            let profile = reply.get("profile").cloned();
            core.audit
                .best_effort(op, &[("profile", profile), ("outcome", Some("ok".into()))]);
            Ok(reply)
        }
        Err(Fault::Refused(r)) => {
            let profile = pairing::profile_id(request.get("profile")).map(Json::from);
            let outcome = Some(Json::from(r.code.as_str()));
            core.audit
                .best_effort(op, &[("profile", profile), ("outcome", outcome)]);
            Err(Fault::Refused(r))
        }
        Err(other) => Err(other),
    }
}

fn pair(core: &Arc<Core>, request: &Json) -> Result<Json, Fault> {
    let mut inner = core.lock();
    let (reply, replaced) =
        pairing::pair_profile(&inner.db, &core.vault, &core.audit, &*core.entropy, request)?;
    if let Some(replaced) = replaced {
        // Owner ruling Dqw 2026-11-16: the newest pairing wins.
        core.detach(&mut inner, &replaced);
        let fields = [
            ("profile", reply.get("profile").cloned()),
            ("replaced", Some(Json::from(replaced))),
            ("outcome", Some(Json::from("ok"))),
        ];
        core.audit.best_effort("pair_replaced", &fields);
    }
    Ok(reply)
}

fn unpair(core: &Arc<Core>, request: &Json) -> Result<Json, Fault> {
    let mut want = vec!["op"];
    want.extend(CREDENTIAL);
    let fields = pairing::triple(request).filter(|_| pairing::key_set_is(request, &want));
    let Some((profile, _, _)) = fields else {
        return Err(Refusal::new("InvalidEnrollment").into());
    };
    let mut inner = core.lock();
    if !pairing::unpair_profile(&inner.db, &core.vault, Some(request), &profile)? {
        drop(inner);
        return Err(refused(core, "unpair", "unpair_bad_credential", Some(request)).into());
    }
    core.detach(&mut inner, &profile);
    Ok(Json::object([
        ("ok", Json::Bool(true)),
        ("profile", Json::from(profile)),
    ]))
}

impl Core {
    /// End a profile's grants, close its agents and drop its worker (`detach`).
    pub fn detach(&self, inner: &mut crate::broker::Inner, profile: &str) {
        self.end_profile(inner, profile);
        for session in inner.sessions.iter().filter(|s| s.profile == profile) {
            session.kill.notify_one();
        }
        if let Some(peer) = inner.peer(profile) {
            self.forget(inner, &peer);
        }
    }
}

/// Operator `pairings` and credential-free `unpair`.
fn pairing_operator(core: &Core, request: &Json) -> Result<Json, Fault> {
    let mut inner = core.lock();
    if pairing::key_set_is(request, &["op"]) && request.get("op") == Some(&Json::from("pairings")) {
        let list = pairing::pairings(&inner.db)?;
        return Ok(Json::object([("ok", Json::Bool(true)), ("pairings", list)]));
    }
    let profile = pairing::profile_id(request.get("profile")).map(str::to_owned);
    let (Some(profile), true) = (profile, pairing::key_set_is(request, &["op", "profile"])) else {
        return Err(Refusal::new("InvalidRequest").into());
    };
    if !pairing::unpair_profile(&inner.db, &core.vault, None, &profile)? {
        return Ok(Refusal::new("UnknownProfile").response());
    }
    core.detach(&mut inner, &profile);
    let fields = [
        ("profile", Some(Json::from(profile.as_str()))),
        ("outcome", Some(Json::from("ok"))),
    ];
    core.audit.best_effort("unpair", &fields);
    Ok(Json::object([
        ("ok", Json::Bool(true)),
        ("profile", Json::from(profile)),
    ]))
}

/// Serve one `operator.sock` connection; every failure answers `RevokeFailed`.
pub async fn operator(core: Arc<Core>, mut stream: UnixStream) {
    let reply = match operate(&core, &mut stream).await {
        Ok(reply) => reply,
        Err(_) => Refusal::new("RevokeFailed").response(),
    };
    let _ = write_frame(&mut stream, &reply).await;
}

async fn operate(core: &Arc<Core>, stream: &mut UnixStream) -> Result<Json, Fault> {
    peer_ok(core, stream, "operator")?;
    let request = read_frame_within(stream, 5.0)
        .await
        .map_err(|e| Fault::Internal(e.to_string()))?;
    let op = request.get("op").and_then(Json::as_str);
    if op.is_some_and(|o| OPERATOR_OPS.contains(&o)) {
        let mut inner = core.lock();
        return Ok(core.grant_operator(&mut inner, &request, None)?);
    }
    if op == Some("pairings") || op == Some("unpair") {
        return pairing_operator(core, &request);
    }
    if !pairing::key_set_is(&request, &["op", "workflow"]) || op != Some("revoke") {
        return Err(Refusal::new("InvalidRequest").into());
    }
    let workflow = request_string(&request, "workflow")?;
    let mut inner = core.lock();
    if let Some(profile) = crate::legacy::remove(&inner.db, &core.vault, workflow)? {
        core.detach(&mut inner, &profile);
    }
    crate::legacy::finish_removals(&inner.db, &core.vault, &core.state)?;
    Ok(Json::object([("ok", Json::Bool(true))]))
}
