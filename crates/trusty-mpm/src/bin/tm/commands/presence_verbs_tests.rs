//! Tests for `tm presence request|wait|devices` (#8878 PR2b-1b). A fake
//! authenticator stands in for Touch ID and a closure for the daemon; the
//! key is the test key, never the Keychain.

use std::cell::RefCell;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use trusty_mpm::core::architect_launch::ARCHITECT_DIR;
use trusty_mpm::core::presence::{self, METHOD_LOCAL, grant_path};
use trusty_mpm::core::presence_device::{self, NewDevice};
use trusty_mpm::core::presence_enrol::{TicketPurpose, mint_ticket};
use trusty_mpm::core::presence_key::{GrantKey, is_account_root};
use trusty_mpm::core::presence_local::PresenceOutcome;
use trusty_mpm::core::twin_identity::ClaudeProcess;

use crate::commands::pm_guard_trust_anchor::tests::{ARCHITECT, bound_env, fixture, test_key};
use crate::commands::presence::tests::Fake;
use crate::commands::presence_devices::{add, revoke};
use crate::commands::presence_request::request;
use crate::commands::presence_wait::{WaitEnv, parse_timeout, wait};
use crate::commands::wait::EXIT_PENDING;

fn now() -> u64 {
    presence::unix_secs(std::time::SystemTime::now())
}

fn scratch() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join(".trusty-mpm");
    std::fs::create_dir_all(root.join(ARCHITECT_DIR)).expect("mkdir");
    (dir, root)
}

fn run_wait(root: &Path, claude: ClaudeProcess, request: Option<&str>) -> i32 {
    let key = || Some(test_key());
    let env = WaitEnv {
        root,
        claude,
        key: &key,
        now: &now,
        sleep: &|_| {},
    };
    wait(&env, request, Duration::ZERO)
}

#[test]
fn presence_request_sends_this_claude_and_prints_the_notice_8878() {
    let fx = fixture();
    let sent = RefCell::new(Value::Null);
    let call = |method: &str, params: Value| {
        assert_eq!(method, "mpm.presence.request");
        *sent.borrow_mut() = params;
        Ok(json!({ "id": "abc", "action": "x (unverified origin)" }))
    };
    let id = request(&bound_env(&fx), "write config", &call).expect("request");
    assert_eq!(id, "abc");
    let sent = sent.into_inner();
    assert_eq!(sent["pid"], ARCHITECT.pid);
    assert_eq!(sent["start_time"], ARCHITECT.start_time);
    assert_eq!(sent["action"], "write config");
    let down = |_: &str, _: Value| Err("the daemon is unreachable".to_owned());
    assert!(request(&bound_env(&fx), "x", &down).is_err());
}

#[test]
fn wait_returns_zero_once_granted_8878() {
    let (_d, root) = scratch();
    presence::mint(&root, &ARCHITECT, METHOD_LOCAL, &test_key(), now()).expect("mint");
    assert_eq!(run_wait(&root, ARCHITECT, None), 0);
    assert_eq!(parse_timeout("5m").expect("5m"), Duration::from_secs(300));
    assert_eq!(parse_timeout("90s").expect("90s"), Duration::from_secs(90));
    assert!(parse_timeout("soon").is_err());
}

/// Fail-open 10: a forged, other-pid or idle grant never exits 0; a named
/// request that is gone with no grant exits 1.
#[test]
fn wait_never_exits_zero_on_a_present_but_invalid_grant_8878() {
    let (_d, root) = scratch();
    let forger = GrantKey::from_bytes([0xee; 32]);
    presence::mint(&root, &ARCHITECT, METHOD_LOCAL, &forger, now()).expect("mint");
    assert_eq!(run_wait(&root, ARCHITECT, None), EXIT_PENDING);
    let other = ClaudeProcess {
        pid: ARCHITECT.pid,
        start_time: ARCHITECT.start_time + 1,
    };
    presence::mint(&root, &other, METHOD_LOCAL, &test_key(), now()).expect("mint");
    assert_eq!(run_wait(&root, ARCHITECT, None), EXIT_PENDING);
    presence::mint(
        &root,
        &ARCHITECT,
        METHOD_LOCAL,
        &test_key(),
        now() - 121 * 60,
    )
    .expect("mint");
    assert_eq!(run_wait(&root, ARCHITECT, None), EXIT_PENDING);
    assert_eq!(run_wait(&root, ARCHITECT, Some(&"0".repeat(32))), 1);
}

/// Spoof: a declined or unavailable prompt mints no ticket, calls no daemon
/// and revokes nothing.
#[test]
fn devices_add_and_revoke_refuse_without_presence_8878() {
    let (_d, root) = scratch();
    let key = || Ok(test_key());
    let called = RefCell::new(0);
    let call = |_: &str, _: Value| {
        *called.borrow_mut() += 1;
        Ok(json!({}))
    };
    let device = enrol(&root, "cred-A");
    for outcome in [
        PresenceOutcome::Declined("cancelled".to_owned()),
        PresenceOutcome::Unavailable("no GUI session".to_owned()),
    ] {
        let fake = Fake::new(outcome);
        assert!(add(&fake, &key, "iphone", Some(ORIGIN), &call).is_err());
        assert!(revoke(&fake, &key, &root, "cred-A").is_err());
    }
    assert_eq!(*called.borrow(), 0, "no daemon call without presence");
    assert_eq!(
        presence_device::load(&root, &test_key(), "cred-A").as_ref(),
        Ok(&device)
    );
    assert_eq!(
        crate::commands::presence_devices::list(&root, &test_key()).expect("list"),
        1
    );
}

/// The console origin the enrol link points at in these tests.
const ORIGIN: &str = "https://studio.example.ts.net";

/// #8878 PR2b-2: the link carries the ticket the daemon accepted.
#[test]
fn devices_add_prints_the_enrol_link_after_presence_8878() {
    use base64::Engine as _;
    let key = || Ok(test_key());
    let sent = RefCell::new(Value::Null);
    let call = |method: &str, params: Value| {
        assert_eq!(method, "mpm.presence.register.start");
        assert_eq!(params["ticket"]["subject"], "iphone");
        assert_eq!(params["ticket"]["purpose"], "enrol");
        *sent.borrow_mut() = params["ticket"].clone();
        Ok(json!({ "options": { "publicKey": {} } }))
    };
    let fake = Fake::new(PresenceOutcome::Granted);
    let url = add(&fake, &key, "iphone", Some(ORIGIN), &call).expect("add");
    assert_eq!(fake.prompts.get(), 1);
    let (page, fragment) = url.split_once('#').expect("fragment");
    assert_eq!(page, format!("{ORIGIN}/tools/presence/enrol/"));
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(fragment)
        .expect("base64url");
    let ticket: Value = serde_json::from_slice(&raw).expect("json");
    assert_eq!(ticket, sent.into_inner());
    let refused = |_: &str, _: Value| Err("the ticket was refused".to_owned());
    assert!(add(&fake, &key, "iphone", Some(ORIGIN), &refused).is_err());
}

/// #8878 PR2b-2 (Architect Q4): with the switch off, no prompt, no ticket,
/// no daemon call, and a message naming the switch.
#[test]
fn devices_add_fails_closed_while_rp_origin_is_unset_8878() {
    let minted = RefCell::new(0);
    let key = || {
        *minted.borrow_mut() += 1;
        Ok(test_key())
    };
    let call = |_: &str, _: Value| -> Result<Value, String> {
        panic!("no daemon call while browser enrol is off")
    };
    let fake = Fake::new(PresenceOutcome::Granted);
    let err = add(&fake, &key, "iphone", None, &call).expect_err("refused");
    assert!(
        err.to_string()
            .contains("enrolment via browser is not enabled"),
        "{err}"
    );
    assert_eq!(fake.prompts.get(), 0);
    assert_eq!(*minted.borrow(), 0);
}

#[test]
fn devices_revoke_removes_the_device_and_its_grant_8878() {
    let (_d, root) = scratch();
    let device = enrol(&root, "cred-A");
    presence::mint(&root, &ARCHITECT, &device.method(), &test_key(), now()).expect("mint");
    let fake = Fake::new(PresenceOutcome::Granted);
    let removed = revoke(&fake, &|| Ok(test_key()), &root, "cred-A").expect("revoke");
    assert_eq!(removed, 1);
    assert!(!grant_path(&root, ARCHITECT.pid).exists());
    assert!(presence_device::load(&root, &test_key(), "cred-A").is_err());
}

#[test]
fn a_scratch_home_is_not_the_account_root_8878() {
    let (_d, root) = scratch();
    assert!(!is_account_root(&root));
}

fn enrol(root: &Path, id: &str) -> presence_device::Device {
    let ticket = mint_ticket(&test_key(), TicketPurpose::Enrol, "iphone", now()).expect("ticket");
    let new = NewDevice {
        credential_id: id.to_owned(),
        name: "iphone".to_owned(),
        public_key: "{}".to_owned(),
        sign_count: 0,
        backup_eligible: false,
        backup_state: false,
    };
    presence_device::enrol(root, &test_key(), &ticket, new, now()).expect("enrol")
}
