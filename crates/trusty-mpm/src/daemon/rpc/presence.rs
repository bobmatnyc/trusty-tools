//! The presence-device methods on the daemon socket: `mpm.presence.*`
//! (#8878 PR2b-1b).
//!
//! Why: plan-pr2b1.md §1 and §4 — a device approval needs state that only a
//! long-lived process can hold: the challenge per request and the passkey
//! ceremony state (Architect Q3; a restart drops them, fail-closed). The tm
//! daemon holds them; the CLI, the hook and (PR2b-2) the console page call
//! in. The socket admits only this uid; every write below is still gated by
//! the sealed records, the tickets and the passkey checks.
//! What: [`PresenceService`](crate::daemon::rpc::presence::PresenceService) runs one verb on the blocking pool:
//! `request` (a pending request for a live `claude`), `status`, `approve`
//! (open a challenge and the passkey options), `verify` (an assertion to a
//! grant through `verify_and_grant`), `register.start` / `register.finish`
//! (enrol behind a ticket), `devices.list`, `devices.revoke` (behind a
//! ticket). Refusals carry `data.exit` 1; a missing key, home or verifier
//! is exit 3; bad params exit 2.
//! Test: `presence_tests.rs` beside this file.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use trusty_common::uds::server::{RpcError, RpcRouter};

use crate::core::presence_device::{self, Device};
use crate::core::presence_enrol::Ticket;
use crate::core::presence_key::{GrantKey, KeySource, UNTRUSTED_NOTICE, acl_verified};
use crate::core::presence_request::{self, NewRequest};
use crate::core::presence_verify::{Assertion, Ceremonies, VerifyEnv, verify_and_grant};
use crate::core::presence_webauthn::{WebauthnRefusal, WebauthnVerifier};
use crate::daemon::error::{CODE_UNAVAILABLE, CODE_UNPROCESSABLE};

#[cfg(test)]
#[path = "presence_tests.rs"]
mod tests;

/// Every method this family serves.
pub const METHODS: [&str; 8] = [
    "mpm.presence.request",
    "mpm.presence.status",
    "mpm.presence.approve",
    "mpm.presence.verify",
    "mpm.presence.register.start",
    "mpm.presence.register.finish",
    "mpm.presence.devices.list",
    "mpm.presence.devices.revoke",
];

/// A refusal: (exit class, message).
type Failure = (i64, String);

fn refused(e: impl std::fmt::Display) -> Failure {
    (1, e.to_string())
}

/// The key lookup a service runs once per call.
type KeyFn = dyn Fn() -> Option<GrantKey> + Send + Sync;
/// The live start time of a PID.
type StartFn = dyn Fn(u32) -> Option<u64> + Send + Sync;

/// The daemon's presence state; see the module doc.
pub struct PresenceService {
    root: Option<PathBuf>,
    key: Box<KeyFn>,
    start_time_of: Box<StartFn>,
    ttl: Duration,
    now: Box<dyn Fn() -> u64 + Send + Sync>,
    ceremonies: Ceremonies,
    verifier: Result<WebauthnVerifier, WebauthnRefusal>,
}

impl PresenceService {
    /// A service over `root` with explicit inputs (tests, and [`Self::live`]).
    pub fn new(
        root: Option<PathBuf>,
        key: Box<KeyFn>,
        start_time_of: Box<StartFn>,
        ttl: Duration,
        now: Box<dyn Fn() -> u64 + Send + Sync>,
        verifier: Result<WebauthnVerifier, WebauthnRefusal>,
    ) -> Self {
        Self {
            root,
            key,
            start_time_of,
            ttl,
            now,
            ceremonies: Ceremonies::new(),
            verifier,
        }
    }

    /// This daemon's service: `~/.trusty-mpm`, the ambient key source (read
    /// per call, never cached), live `ps` start times, and `[presence]`.
    pub fn live() -> Self {
        let root = dirs::home_dir()
            .filter(|h| h.is_absolute())
            .map(|h| h.join(".trusty-mpm"));
        let source = root.as_deref().map(KeySource::ambient);
        let config = crate::core::config::MpmConfig::load_default().presence;
        Self::new(
            root,
            Box::new(move || source.as_ref()?.load().ok().flatten()),
            Box::new(|pid| {
                crate::core::architect_sidecar_prune::start_time(pid)
                    .ok()
                    .flatten()
            }),
            config.request_ttl(),
            Box::new(|| crate::core::presence::unix_secs(std::time::SystemTime::now())),
            WebauthnVerifier::from_config(&config),
        )
    }

    fn root(&self) -> Result<&std::path::Path, Failure> {
        self.root
            .as_deref()
            .ok_or_else(|| (3, "the home directory is unknown".to_owned()))
    }

    fn key(&self) -> Result<GrantKey, Failure> {
        (self.key)().ok_or_else(|| (3, "the presence key could not be read".to_owned()))
    }

    fn verifier(&self) -> Result<&WebauthnVerifier, Failure> {
        self.verifier.as_ref().map_err(|e| (3, e.to_string()))
    }

    /// Run `verb` (the method name after `mpm.presence.`) on `params`.
    ///
    /// Why/What: see the module doc; nothing here grants without
    /// `verify_and_grant`, and nothing enrols or revokes without a ticket.
    /// Test: `a_device_approval_round_trips_through_the_service_8878`,
    /// `a_request_for_a_dead_or_reused_pid_is_refused_8878`,
    /// `register_start_without_a_ticket_writes_nothing_8878`,
    /// `the_service_refuses_when_the_verifier_is_unconfigured_8878`.
    pub fn call(&self, verb: &str, params: Value) -> Result<Value, Failure> {
        match verb {
            "request" => self.request(parse(params)?),
            "status" => Ok(json!({
                "verifier": self.verifier.as_ref().err().map_or("ready".to_owned(), ToString::to_string),
                "acl_verified": acl_verified(),
                "notice": UNTRUSTED_NOTICE,
            })),
            "approve" => self.approve(&parse::<ById>(params)?.request_id),
            "verify" => self.verify(parse(params)?),
            "register.start" => {
                let p: WithTicket = parse(params)?;
                let options = self
                    .verifier()?
                    .start_registration(&self.key()?, &p.ticket, (self.now)())
                    .map_err(refused)?;
                Ok(
                    json!({ "options": options, "incomplete": "the browser leg arrives with PR2b-2" }),
                )
            }
            "register.finish" => {
                let p: Finish = parse(params)?;
                let response =
                    serde_json::from_value(p.credential).map_err(|e| (2, e.to_string()))?;
                let device = self
                    .verifier()?
                    .finish_registration(
                        self.root()?,
                        &self.key()?,
                        &p.ticket,
                        &response,
                        (self.now)(),
                    )
                    .map_err(refused)?;
                Ok(json!({ "credential_id": device.credential_id, "name": device.name }))
            }
            "devices.list" => self.list(),
            "devices.revoke" => {
                let p: Revoke = parse(params)?;
                let now = (self.now)();
                let done = presence_device::revoke(
                    self.root()?,
                    &self.key()?,
                    &p.ticket,
                    &p.credential_id,
                    now,
                )
                .map_err(refused)?;
                Ok(json!({ "grants_removed": done.grants_removed.len() }))
            }
            other => Err((2, format!("unknown presence verb `{other}`"))),
        }
    }

    fn request(&self, p: RequestParams) -> Result<Value, Failure> {
        // #8878: the pid must be a live process with the stated start time.
        if (self.start_time_of)(p.pid) != Some(p.start_time) {
            return Err(refused("no live process has this pid and start time"));
        }
        let new = NewRequest {
            pid: p.pid,
            start_time: p.start_time,
            project: p.project,
            session: p.session,
            action: p.action,
        };
        let pending =
            presence_request::create(self.root()?, &self.key()?, new, self.ttl, (self.now)())
                .map_err(refused)?;
        Ok(json!({
            "id": pending.id,
            "expires": pending.expires,
            "action": pending.action_label(),
            "notice": UNTRUSTED_NOTICE,
        }))
    }

    fn approve(&self, request_id: &str) -> Result<Value, Failure> {
        let (root, key, now) = (self.root()?, self.key()?, (self.now)());
        let pending = presence_request::load(root, &key, request_id, now).map_err(refused)?;
        let devices: Vec<Device> = presence_device::list(root, &key)
            .map_err(refused)?
            .into_iter()
            .filter_map(|(_, d)| d.ok())
            .collect();
        let verifier = self.verifier()?;
        let challenge = self
            .ceremonies
            .begin(root, &key, request_id, now)
            .map_err(refused)?;
        let options = verifier
            .start_authentication(&challenge, &devices, now)
            .map_err(refused)?;
        Ok(json!({
            "options": options,
            "action": pending.action_label(),
            "action_hash": pending.action_hash,
            "notice": UNTRUSTED_NOTICE,
        }))
    }

    fn verify(&self, p: VerifyParams) -> Result<Value, Failure> {
        let root = self.root()?;
        let verifier = self.verifier()?;
        let assertion = Assertion {
            request_id: p.request_id,
            credential_id: p.credential_id,
            action_hash: p.action_hash,
            payload: serde_json::to_vec(&p.credential).map_err(|e| (2, e.to_string()))?,
        };
        let key_fn = || (self.key)();
        let start_fn = |pid| (self.start_time_of)(pid);
        let env = VerifyEnv {
            root,
            key: &key_fn,
            start_time_of: &start_fn,
            now: (self.now)(),
        };
        let granted =
            verify_and_grant(&env, &self.ceremonies, verifier, &assertion).map_err(refused)?;
        Ok(json!({ "pid": granted.claude.pid, "method": granted.method }))
    }

    fn list(&self) -> Result<Value, Failure> {
        let rows: Vec<Value> = presence_device::list(self.root()?, &self.key()?)
            .map_err(refused)?
            .into_iter()
            .map(|(id, found)| match found {
                Ok(d) => json!({
                    "credential_id": d.credential_id, "name": d.name, "created": d.created,
                    "last_used": d.last_used, "sign_count": d.sign_count,
                    "backup_eligible": d.backup_eligible, "backup_state": d.backup_state,
                }),
                Err(e) => json!({ "credential_id": id, "error": e.to_string() }),
            })
            .collect();
        Ok(json!({ "devices": rows, "notice": UNTRUSTED_NOTICE }))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestParams {
    pid: u32,
    start_time: u64,
    project: String,
    session: String,
    action: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ById {
    request_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifyParams {
    request_id: String,
    credential_id: String,
    action_hash: String,
    credential: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WithTicket {
    ticket: Ticket,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Finish {
    ticket: Ticket,
    credential: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Revoke {
    ticket: Ticket,
    credential_id: String,
}

fn parse<T: DeserializeOwned>(params: Value) -> Result<T, Failure> {
    serde_json::from_value(params).map_err(|e| (2, format!("bad params: {e}")))
}

/// The error frame for a failure, keeping its exit class in `data.exit`.
fn to_rpc((exit, message): Failure) -> RpcError {
    let code = if exit == 3 {
        CODE_UNAVAILABLE
    } else {
        CODE_UNPROCESSABLE
    };
    RpcError::new(code, message).with_data(json!({ "exit": exit }))
}

/// Mount every `mpm.presence.*` method on `router`, sharing `service`.
///
/// Test: `the_presence_methods_are_served_8878`.
pub fn register(router: RpcRouter, service: Arc<PresenceService>) -> RpcRouter {
    METHODS
        .into_iter()
        .fold(router, |router, method: &'static str| {
            let service = Arc::clone(&service);
            let verb = method.trim_start_matches("mpm.presence.");
            router.typed(method, move |params: Value| {
                let service = Arc::clone(&service);
                async move {
                    match tokio::task::spawn_blocking(move || service.call(verb, params)).await {
                        Ok(result) => result.map_err(to_rpc),
                        Err(e) => Err(RpcError::internal(format!("{method}: {e}"))),
                    }
                }
            })
        })
}
