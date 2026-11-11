//! #8878 PR2b-1a Fail-Open Check (plan-pr2b1.md §5, items 1-9 and 12): every
//! error arm that could leave a grant written or a request consumed.

use std::path::Path;
use std::sync::Arc;

use super::*;
use crate::core::presence::{DeviceGrantPolicy, PresenceRefusal, check_with_policy, grant_path};
use crate::core::presence_device::devices_dir;
use crate::core::presence_enrol::{TicketPurpose, mint_ticket};
use crate::core::presence_request::{pending_dir, pending_path};
use crate::core::presence_test_support::{
    ARCHITECT, AlwaysAcceptVerifier, FakeDevice, FakeVerifier, NOW, architect_alive, grant_files,
    key, request, root, snapshot, verify_at,
};

fn setup(root: &Path) -> (FakeDevice, presence_request::Pending, Ceremonies, Challenge) {
    let phone = FakeDevice::new("cred-A", 1);
    phone.enrol(root, 3);
    let pending = request(root, "write .claude/settings.json");
    let ceremonies = Ceremonies::new();
    let challenge = ceremonies
        .begin(root, &key(), &pending.id, NOW)
        .expect("begin");
    (phone, pending, ceremonies, challenge)
}

/// A verifier that accepts like the fake device, then runs `sabotage` on
/// the anchor before the orchestrator continues.
struct Sabotage<'a>(&'a dyn Fn());

impl AssertionVerifier for Sabotage<'_> {
    fn verify(&self, c: &Challenge, d: &Device, a: &Assertion) -> Result<Verified, String> {
        let verified = FakeVerifier.verify(c, d, a)?;
        (self.0)();
        Ok(verified)
    }
}

/// 1: the grant cannot be written after the claim: an error, no grant, and
/// no request left to resubmit.
#[test]
fn mint_failure_leaves_no_grant_and_no_resubmittable_request_8878() {
    let (_d, root) = root();
    let (phone, pending, ceremonies, challenge) = setup(&root);
    let blocker = grant_path(&root, ARCHITECT.pid);
    let block = || std::fs::create_dir_all(blocker.join("full")).expect("block the grant path");
    let assertion = phone.sign(&challenge, &pending, 4, true);
    let failed = verify_at(&root, &ceremonies, &Sabotage(&block), &assertion, NOW);
    assert!(matches!(failed, Err(VerifyError::Grant(_))), "{failed:?}");
    assert!(
        blocker.is_dir(),
        "no grant file was written over the blocker"
    );
    assert!(!pending_path(&root, &pending.id).expect("path").exists());
    assert!(
        !pending_path(&root, &pending.id)
            .expect("path")
            .with_extension("claimed")
            .exists()
    );
    let again = verify_at(
        &root,
        &ceremonies,
        &FakeVerifier,
        &phone.sign(&challenge, &pending, 5, true),
        NOW,
    );
    assert!(again.is_err(), "the consumed request cannot be resubmitted");
}

/// 2: no key: refused before anything is read or consumed.
#[test]
fn key_unavailable_before_verify_grants_nothing_8878() {
    let (_d, root) = root();
    let (phone, pending, ceremonies, challenge) = setup(&root);
    let assertion = phone.sign(&challenge, &pending, 4, true);
    let before = snapshot(&root);
    let no_key = || None;
    let env = VerifyEnv {
        root: &root,
        key: &no_key,
        start_time_of: &architect_alive,
        now: NOW,
    };
    let refused = verify_and_grant(&env, &ceremonies, &FakeVerifier, &assertion);
    assert_eq!(refused, Err(VerifyError::NoKey));
    assert_eq!(snapshot(&root), before);
    // The request and challenge survive: the next verify with a key passes.
    verify_at(&root, &ceremonies, &FakeVerifier, &assertion, NOW).expect("then granted");
}

/// 3: the counter cannot be saved after a verified assertion: no grant,
/// else the same assertion would verify again.
#[test]
fn sign_count_persist_failure_refuses_the_grant_8878() {
    let (_d, root) = root();
    let (phone, pending, ceremonies, challenge) = setup(&root);
    let devices = devices_dir(&root);
    let aside = root.join("devices-aside");
    let swap = || {
        std::fs::rename(&devices, &aside).expect("move devices");
        std::fs::write(&devices, b"not a directory").expect("block devices");
    };
    let assertion = phone.sign(&challenge, &pending, 4, true);
    let refused = verify_at(&root, &ceremonies, &Sabotage(&swap), &assertion, NOW);
    assert!(
        matches!(refused, Err(VerifyError::Persist(_))),
        "{refused:?}"
    );
    assert!(grant_files(&root).is_empty());
    assert!(
        pending_path(&root, &pending.id).expect("path").exists(),
        "not consumed"
    );
}

/// 4: an unreadable, malformed or symlinked device record refuses.
#[test]
fn unreadable_or_malformed_device_record_refuses_8878() {
    let (_d, root) = root();
    let (phone, pending, ceremonies, challenge) = setup(&root);
    let record = presence_device::device_path(&root, "cred-A").expect("path");
    let assertion = phone.sign(&challenge, &pending, 4, true);
    std::fs::write(&record, b"{not json").expect("corrupt");
    let before = snapshot(&root);
    let malformed = verify_at(&root, &ceremonies, &FakeVerifier, &assertion, NOW);
    assert_eq!(malformed, Err(VerifyError::Device(DeviceError::Malformed)));
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    let open = verify_at(&root, &ceremonies, &FakeVerifier, &assertion, NOW);
    assert!(
        matches!(open, Err(VerifyError::Device(DeviceError::Unsafe(_)))),
        "{open:?}"
    );
    std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    assert_eq!(snapshot(&root), before);
}

#[test]
fn device_record_swapped_for_symlink_refuses_8878() {
    let (dir, root) = root();
    let (phone, pending, ceremonies, challenge) = setup(&root);
    let record = presence_device::device_path(&root, "cred-A").expect("path");
    let real = dir.path().join("elsewhere.json");
    std::fs::rename(&record, &real).expect("move");
    std::os::unix::fs::symlink(&real, &record).expect("symlink");
    let before = snapshot(&root);
    let refused = verify_at(
        &root,
        &ceremonies,
        &FakeVerifier,
        &phone.sign(&challenge, &pending, 4, true),
        NOW,
    );
    assert!(
        matches!(refused, Err(VerifyError::Device(DeviceError::Unsafe(_)))),
        "{refused:?}"
    );
    assert_eq!(snapshot(&root), before);
}

/// 5: a symlinked, oversized or malformed request refuses and stays.
#[test]
fn pending_symlink_oversized_or_malformed_refuses_and_is_not_consumed_into_a_grant_8878() {
    let (dir, root) = root();
    let (phone, pending, ceremonies, challenge) = setup(&root);
    let path = pending_path(&root, &pending.id).expect("path");
    let assertion = phone.sign(&challenge, &pending, 4, true);
    let good = std::fs::read(&path).expect("read");
    let write = |bytes: &[u8]| {
        let _ = std::fs::remove_file(&path);
        crate::core::architect_session::write_owner_only(&path, bytes).expect("write");
    };
    for bad in [b"{not json".to_vec(), vec![b' '; 9 << 10]] {
        write(&bad);
        let refused = verify_at(&root, &ceremonies, &FakeVerifier, &assertion, NOW);
        assert!(
            matches!(refused, Err(VerifyError::Request(_))),
            "{refused:?}"
        );
        assert_eq!(std::fs::read(&path).expect("still there"), bad);
    }
    let real = dir.path().join("real.json");
    std::fs::remove_file(&path).expect("rm");
    std::fs::write(&real, &good).expect("write");
    std::os::unix::fs::symlink(&real, &path).expect("symlink");
    let refused = verify_at(&root, &ceremonies, &FakeVerifier, &assertion, NOW);
    assert!(
        matches!(refused, Err(VerifyError::Request(_))),
        "{refused:?}"
    );
    assert!(std::fs::symlink_metadata(&path).is_ok() && real.exists());
    assert!(grant_files(&root).is_empty());
}

/// 6: two verifies racing for one request mint exactly one grant, and a
/// claim of a request already claimed is refused.
#[test]
fn two_concurrent_verifies_for_one_request_mint_exactly_one_grant_8878() {
    for _ in 0..8 {
        let (_d, root) = root();
        let phone = FakeDevice::new("cred-S", 2);
        phone.enrol(&root, 0);
        let pending = request(&root, "write .claude/settings.json");
        let ceremonies = Arc::new(Ceremonies::new());
        let challenge = ceremonies
            .begin(&root, &key(), &pending.id, NOW)
            .expect("begin");
        // A synced passkey (0 on 0), so the counter does not decide the race.
        let assertion = phone.sign(&challenge, &pending, 0, true);
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let wins: usize = std::thread::scope(|s| {
            let runs: Vec<_> = (0..2)
                .map(|_| {
                    let (root, c, a, b) = (&root, &ceremonies, &assertion, barrier.clone());
                    s.spawn(move || {
                        b.wait();
                        verify_at(root, c, &FakeVerifier, a, NOW).is_ok()
                    })
                })
                .collect();
            runs.into_iter()
                .map(|h| usize::from(h.join().expect("join")))
                .sum()
        });
        assert_eq!(wins, 1);
        assert_eq!(grant_files(&root).len(), 1);
    }
    // The claim itself: the second rename finds nothing.
    let (_d, root) = root();
    let pending = request(&root, "x");
    presence_request::claim(&root, &key(), &pending).expect("first claim");
    assert_eq!(
        presence_request::claim(&root, &key(), &pending),
        Err(RequestError::Claimed)
    );
}

/// 7: the PID was reused between request and verify.
#[test]
fn claude_pid_reuse_between_request_and_verify_refuses_8878() {
    let (_d, root) = root();
    let (phone, pending, ceremonies, challenge) = setup(&root);
    let before = snapshot(&root);
    let reused = |pid: u32| (pid == ARCHITECT.pid).then_some(ARCHITECT.start_time + 9);
    let key_fn = || Some(key());
    let env = VerifyEnv {
        root: &root,
        key: &key_fn,
        start_time_of: &reused,
        now: NOW,
    };
    let assertion = phone.sign(&challenge, &pending, 4, true);
    let refused = verify_and_grant(&env, &ceremonies, &FakeVerifier, &assertion);
    assert_eq!(refused, Err(VerifyError::ProcessGone));
    assert_eq!(snapshot(&root), before);
}

/// 8: a clock before the request's creation refuses.
#[test]
fn clock_before_created_at_refuses_8878() {
    let (_d, root) = root();
    let (phone, pending, _, _) = setup(&root);
    let ceremonies = Ceremonies::new();
    let early = NOW - 10;
    let challenge = Challenge {
        request_id: pending.id.clone(),
        bytes: [3; 32],
        issued_at: early,
    };
    ceremonies
        .lock()
        .insert(pending.id.clone(), challenge.clone());
    let before = snapshot(&root);
    let refused = verify_at(
        &root,
        &ceremonies,
        &FakeVerifier,
        &phone.sign(&challenge, &pending, 4, true),
        early,
    );
    assert_eq!(refused, Err(VerifyError::Request(RequestError::BadDates)));
    assert_eq!(snapshot(&root), before);
    // A challenge issued after now is refused too.
    let refused = verify_at(
        &root,
        &ceremonies,
        &FakeVerifier,
        &phone.sign(&challenge, &pending, 4, true),
        early - 1,
    );
    assert_eq!(refused, Err(VerifyError::ChallengeExpired));
}

/// 9: the record is removed but a grant cannot be: an error, and `check`
/// still refuses the grant because it re-reads the record.
#[test]
fn revoke_partial_failure_is_an_error_and_check_still_refuses_8878() {
    use std::os::unix::fs::PermissionsExt as _;
    let (_d, root) = root();
    FakeDevice::new("cred-A", 1).enrol(&root, 0);
    crate::core::presence::mint(&root, &ARCHITECT, "device:cred-A", &key(), NOW).expect("mint");
    let anchor = root.join(crate::core::architect_launch::ARCHITECT_DIR);
    std::fs::set_permissions(&anchor, std::fs::Permissions::from_mode(0o500)).expect("chmod");
    let ticket = mint_ticket(&key(), TicketPurpose::Revoke, "cred-A", NOW).expect("ticket");
    let revoked = presence_device::revoke(&root, &key(), &ticket, "cred-A", NOW);
    std::fs::set_permissions(&anchor, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    assert!(matches!(revoked, Err(DeviceError::Write(_))), "{revoked:?}");
    assert!(
        grant_path(&root, ARCHITECT.pid).exists(),
        "the grant could not be removed"
    );
    let refused = check_with_policy(
        &root,
        &ARCHITECT,
        || Some(key()),
        NOW,
        DeviceGrantPolicy::Honour,
    );
    assert_eq!(refused, Err(PresenceRefusal::DeviceRevoked));
}

/// 12: a daemon restart drops every open challenge.
#[test]
fn daemon_restart_drops_ceremony_state_8878() {
    let (_d, root) = root();
    let (phone, pending, _old, challenge) = setup(&root);
    let restarted = Ceremonies::new();
    let before = snapshot(&root);
    let refused = verify_at(
        &root,
        &restarted,
        &FakeVerifier,
        &phone.sign(&challenge, &pending, 4, true),
        NOW,
    );
    assert_eq!(refused, Err(VerifyError::NoCeremony));
    // Even a verifier that accepts anything cannot pass without the state.
    let assertion = phone.sign(&challenge, &pending, 4, true);
    let refused = verify_at(&root, &restarted, &AlwaysAcceptVerifier, &assertion, NOW);
    assert_eq!(refused, Err(VerifyError::NoCeremony));
    assert_eq!(snapshot(&root), before);
    assert!(
        pending_dir(&root)
            .join(format!("{}.json", pending.id))
            .exists()
    );
}

/// #8878: a revoke while the verifier runs wins: the verify fails before the
/// claim, so the record stays gone, no grant is written, and the request and
/// challenge stay in place.
#[test]
fn revoke_during_verify_does_not_resurrect_the_device_8878() {
    let (_d, root) = root();
    let (phone, pending, ceremonies, challenge) = setup(&root);
    let ticket = mint_ticket(&key(), TicketPurpose::Revoke, "cred-A", NOW).expect("ticket");
    let revoke = || {
        presence_device::revoke(&root, &key(), &ticket, "cred-A", NOW).expect("revoke");
    };
    let assertion = phone.sign(&challenge, &pending, 4, true);
    let refused = verify_at(&root, &ceremonies, &Sabotage(&revoke), &assertion, NOW);
    assert_eq!(refused, Err(VerifyError::Device(DeviceError::Unknown)));
    assert_eq!(
        presence_device::load(&root, &key(), "cred-A"),
        Err(DeviceError::Unknown),
        "the revoked device stays revoked"
    );
    assert!(grant_files(&root).is_empty(), "no grant names it");
    assert!(
        pending_path(&root, &pending.id).expect("path").exists(),
        "not consumed"
    );
}

/// #8878: a verify whose verifier saw an older counter cannot save it over
/// the higher one another verify saved meanwhile.
#[test]
fn concurrent_verifies_never_move_the_sign_count_backwards_8878() {
    let (_d, root) = root();
    let (phone, first, ceremonies, challenge) = setup(&root);
    let later = NOW + presence_request::RATE_WINDOW.as_secs();
    let second = crate::core::presence_test_support::request_for(&root, ARCHITECT, "b", later);
    let second_challenge = ceremonies
        .begin(&root, &key(), &second.id, later)
        .expect("begin");
    let other = phone.sign(&second_challenge, &second, 7, true);
    let race = || {
        verify_at(&root, &ceremonies, &FakeVerifier, &other, later).expect("the other verify");
    };
    let stale = phone.sign(&challenge, &first, 5, true);
    let refused = verify_at(&root, &ceremonies, &Sabotage(&race), &stale, NOW);
    assert_eq!(refused, Err(VerifyError::CounterRegressed));
    let stored = presence_device::load(&root, &key(), "cred-A").expect("device");
    assert_eq!(stored.sign_count, 7, "the counter never moves backwards");
    assert!(
        pending_path(&root, &first.id).expect("path").exists(),
        "not consumed"
    );
}
