//! `tm secrets unlock` and `lock` against the in-process server (#10524).
//!
//! A child of `tests.rs`, so it shares that harness. The presence check is a
//! fake; no test raises the real Touch ID prompt.
//!
//! Test: itself.

use std::sync::atomic::{AtomicUsize, Ordering};

use trusty_mpm::core::presence_local::{Authenticator, PresenceOutcome};

use super::*;

/// A presence check that answers `outcome` and counts its prompts.
struct FakeAuth(PresenceOutcome, AtomicUsize);

impl Authenticator for FakeAuth {
    fn authenticate(&self, _: &str) -> PresenceOutcome {
        self.1.fetch_add(1, Ordering::SeqCst);
        self.0.clone()
    }
}

fn ctx_of(h: &Harness) -> Ctx<'_> {
    Ctx {
        client: &h.client,
        project: &h.repo,
        clipboard: &Fixed(""),
        stdin: &Fixed(""),
        ci: false,
    }
}

/// Why: #10524 — a declined or unavailable presence prompt unlocks nothing:
/// no request reaches the server, so a later `lock` finds no grant.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unlock_declined_presence_sends_nothing() {
    let h = harness().await;
    for outcome in [
        PresenceOutcome::Declined("cancelled".to_owned()),
        PresenceOutcome::Unavailable("ssh".to_owned()),
    ] {
        let auth = FakeAuth(outcome, AtomicUsize::new(0));
        let mut out = Vec::new();
        let err = super::super::unlock::unlock(&ctx_of(&h), &auth, &mut out)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("nothing was unlocked"), "{err}");
        assert_eq!(auth.1.load(Ordering::SeqCst), 1);
        assert!(out.is_empty());
    }
    let mut out = Vec::new();
    super::super::unlock::lock(&ctx_of(&h), &mut out)
        .await
        .unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "no unlock grant was live\n"
    );
}

/// Why: #10524 — after presence, `unlock` opens the server's grant and
/// `lock` ends it. Off macOS the server has no power watcher, so it refuses
/// and nothing is unlocked (fail closed).
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unlock_granted_presence_opens_the_grant_or_reports_the_server() {
    let h = harness().await;
    let auth = FakeAuth(PresenceOutcome::Granted, AtomicUsize::new(0));
    let mut out = Vec::new();
    let unlocked = super::super::unlock::unlock(&ctx_of(&h), &auth, &mut out).await;
    assert_eq!(unlocked.is_ok(), cfg!(target_os = "macos"), "{unlocked:?}");
    let mut locked = Vec::new();
    super::super::unlock::lock(&ctx_of(&h), &mut locked)
        .await
        .unwrap();
    let locked = String::from_utf8(locked).unwrap();
    if cfg!(target_os = "macos") {
        let said = String::from_utf8(out).unwrap();
        assert!(said.starts_with("unlocked for 8h00m"), "{said}");
        assert_eq!(locked, "locked: the next secret read prompts again\n");
    } else {
        assert_eq!(locked, "no unlock grant was live\n");
    }
}

/// Why: #10524 — `unlock` never runs through `dispatch`, so no test path
/// can raise the real presence prompt; `lock` with no grant says so.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lock_with_no_grant_says_so() {
    let h = harness().await;
    let outcome = run(&h, "", &["lock"]).await;
    assert_eq!(outcome.err, None);
    assert_eq!(outcome.out, "no unlock grant was live\n");
    let refused = run(&h, "", &["unlock"]).await;
    let err = refused.err.unwrap_or_default();
    assert!(err.contains("only from the command line"), "{err}");
}

/// Why: #10524 — the harness's Keychain is a memory double with no vault
/// blob, so `migrate` is refused for every step and changes nothing; the
/// flags map to their steps.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn migrate_without_a_blob_store_is_refused() {
    use trusty_secrets::api::methods::MigrateStep;
    let h = harness().await;
    for args in [
        &["migrate", "--dry-run"][..],
        &["migrate"],
        &["migrate", "--finish"],
        &["migrate", "--rollback"],
    ] {
        let outcome = run(&h, "", args).await;
        assert!(outcome.err.is_some(), "{args:?} succeeded");
        assert_eq!(outcome.out, "", "{args:?}");
    }
    assert_eq!(
        super::super::unlock::step_of(false, false),
        MigrateStep::Migrate
    );
    assert_eq!(
        super::super::unlock::step_of(true, false),
        MigrateStep::Finish
    );
    assert_eq!(
        super::super::unlock::step_of(false, true),
        MigrateStep::Rollback
    );
}
