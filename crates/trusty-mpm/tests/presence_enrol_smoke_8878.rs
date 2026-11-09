//! Live smoke of passkey enrolment through a sandbox tm daemon (#8878 PR2b-2).
//!
//! Why: the unit tests drive `PresenceService` in process; this drives the
//! real daemon over its socket, as the console relay does, to show a
//! completed enrolment, a spent-link refusal and an expired-link refusal.
//! What: SoftToken (a dev-dependency only) answers the registration. The
//! harness mints its tickets with the sandbox's file key, the valid one at
//! the current time and the expired one past-dated (Architect Q2: no clock
//! override in the daemon). It refuses to run unless the daemon socket and
//! the key sit inside the sandbox directory `scripts/sandbox_daemon.sh`
//! made, never under the account's own home (CLAUDE.md #9121).
//! Run (ignored by default; needs a sandbox daemon):
//! ```text
//! D=$(mktemp -d /tmp/pr2b2.XXXX); mkdir -p "$D/home/.trusty-mpm"
//! printf '[presence]\nrp_id = "studio.example.ts.net"\nrp_origin = "https://studio.example.ts.net"\n' \
//!   > "$D/home/.trusty-mpm/config.toml"
//! scripts/sandbox_daemon.sh --bin <tm> --dir "$D"        # another shell
//! TRUSTY_SMOKE_SANDBOX_DIR="$D" cargo test -p trusty-mpm --test integration \
//!   presence_enrol_smoke_8878 -- --include-ignored --nocapture
//! ```
//! Test: `enrol_spent_and_expired_through_the_sandbox_daemon_8878`.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use trusty_common::uds::server::RpcResponse;
use trusty_mpm::core::presence::unix_secs;
use trusty_mpm::core::presence_enrol::{TICKET_TTL, Ticket, TicketPurpose, mint_ticket};
use trusty_mpm::core::presence_key::{GrantKey, KeySource};
use webauthn_authenticator_rs::prelude::{Url, WebauthnAuthenticator};
use webauthn_authenticator_rs::softtoken::SoftToken;

/// The sandbox directory `scripts/sandbox_daemon.sh --dir` was given.
const SANDBOX_DIR_ENV: &str = "TRUSTY_SMOKE_SANDBOX_DIR";

/// One daemon call: the result, or the error frame's message.
async fn call(socket: &Path, method: &str, params: Value) -> Result<Value, String> {
    let frame = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let reply: RpcResponse =
        trusty_common::uds::send_framed_request(socket, &frame, Duration::from_secs(15))
            .await
            .map_err(|e| format!("{method}: the daemon did not answer: {e}"))?;
    match (reply.result, reply.error) {
        (_, Some(e)) => Err(e.message),
        (Some(v), None) => Ok(v),
        (None, None) => Err(format!("{method}: empty reply")),
    }
}

/// The sandbox's `(socket, anchor root)`, refusing anything outside it.
fn sandbox() -> (PathBuf, PathBuf) {
    let dir = PathBuf::from(std::env::var_os(SANDBOX_DIR_ENV).expect(SANDBOX_DIR_ENV));
    let dir = dir.canonicalize().expect("the sandbox directory exists");
    let account = nix::unistd::User::from_uid(nix::unistd::Uid::current())
        .expect("passwd lookup")
        .expect("passwd entry")
        .dir;
    let root = dir.join("home/.trusty-mpm");
    assert!(
        dir != account && root != account.join(".trusty-mpm"),
        "the sandbox must not be the account's own home: {}",
        dir.display()
    );
    let socket = dir.join("data/trusty-mpm/trusty-mpm.sock");
    assert!(
        socket.exists(),
        "no sandbox daemon socket at {}",
        socket.display()
    );
    (socket, root)
}

fn devices(listed: &Value) -> Vec<String> {
    listed["devices"]
        .as_array()
        .expect("devices")
        .iter()
        .filter_map(|d| d["credential_id"].as_str().map(str::to_owned))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

async fn list(socket: &Path) -> Vec<String> {
    devices(
        &call(socket, "mpm.presence.devices.list", json!({}))
            .await
            .expect("list"),
    )
}

/// SoftToken's registration for `ticket`, through `register.start`.
async fn register(socket: &Path, ticket: &Ticket, origin: &str) -> Value {
    let started = call(
        socket,
        "mpm.presence.register.start",
        json!({ "ticket": ticket }),
    )
    .await
    .expect("register.start");
    let options = serde_json::from_value(started["options"].clone()).expect("options");
    let mut token = SoftToken::new(true).expect("softtoken").0;
    let response = token
        .do_registration(Url::parse(origin).expect("origin"), options)
        .expect("softtoken registration");
    serde_json::to_value(response).expect("json")
}

#[tokio::test]
#[ignore = "needs a daemon from scripts/sandbox_daemon.sh; see the module doc"]
async fn enrol_spent_and_expired_through_the_sandbox_daemon_8878() {
    let (socket, root) = sandbox();
    let key: GrantKey = KeySource::for_root(&root, true)
        .load_or_create()
        .expect("the sandbox file key");
    let status = call(&socket, "mpm.presence.status", json!({}))
        .await
        .expect("status");
    eprintln!("status: {status}");
    assert_eq!(
        status["verifier"], "ready",
        "set [presence] rp_id and rp_origin"
    );
    let origin = status["enrol_origin"]
        .as_str()
        .expect("[presence] rp_origin is set")
        .to_owned();
    let before = list(&socket).await;
    let now = unix_secs(SystemTime::now());

    // (a) A completed enrolment.
    let ticket = mint_ticket(&key, TicketPurpose::Enrol, "smoke-8878", now).expect("ticket");
    let credential = register(&socket, &ticket, &origin).await;
    let finish = json!({ "ticket": ticket, "credential": credential });
    let done = call(&socket, "mpm.presence.register.finish", finish.clone())
        .await
        .expect("register.finish");
    eprintln!("(a) enrolled: {done}");
    let id = done["credential_id"]
        .as_str()
        .expect("credential_id")
        .to_owned();
    let after = list(&socket).await;
    assert!(after.contains(&id), "devices.list shows {id}: {after:?}");
    assert_eq!(after.len(), before.len() + 1);

    // (b) The same ticket again: refused at start and at finish.
    let again = call(
        &socket,
        "mpm.presence.register.start",
        json!({ "ticket": ticket }),
    )
    .await;
    eprintln!("(b) start again: {again:?}");
    assert!(
        again.as_ref().is_err_and(|m| m.contains("already used")),
        "{again:?}"
    );
    let refinish = call(&socket, "mpm.presence.register.finish", finish).await;
    eprintln!("(b) finish again: {refinish:?}");
    assert!(refinish.is_err());
    assert_eq!(list(&socket).await, after, "no second record");

    // (c) A past-dated ticket: refused at start and at finish.
    let stale_at = now - TICKET_TTL.as_secs() - 1;
    let stale = mint_ticket(&key, TicketPurpose::Enrol, "smoke-8878-old", stale_at).expect("old");
    let start = call(
        &socket,
        "mpm.presence.register.start",
        json!({ "ticket": stale }),
    )
    .await;
    eprintln!("(c) expired start: {start:?}");
    assert!(
        start.as_ref().is_err_and(|m| m.contains("expired")),
        "{start:?}"
    );
    let late = json!({ "ticket": stale, "credential": credential });
    let late = call(&socket, "mpm.presence.register.finish", late).await;
    eprintln!("(c) expired finish: {late:?}");
    assert!(late.is_err());
    assert_eq!(
        list(&socket).await,
        after,
        "no record for an expired ticket"
    );
}
