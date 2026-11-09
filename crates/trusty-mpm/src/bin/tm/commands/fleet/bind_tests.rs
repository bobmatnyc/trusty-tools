//! Tests for `tm fleet bind` (`bind.rs`), #8878 PR2a. A fake authenticator
//! stands in for Touch ID; every refusal asserts nothing was recorded.

use trusty_mpm::core::architect_launch::{ARCHITECT_RECORDS, architect_session_name};
use trusty_mpm::core::presence::{self, grant_path};
use trusty_mpm::core::presence_local::PresenceOutcome;
use trusty_mpm::core::twin_arming::process_facts;
use trusty_mpm::core::twin_identity::ClaudeProcess;

use super::*;
use crate::commands::pm_guard_trust_anchor::tests::{
    ARCHITECT, Fixture, allowlist, architect_table, fixture, pm_env, spoof_env, table,
};
use crate::commands::pm_guard_trust_anchor::{ANCHOR_ROOT, ClaudeLookup};
use crate::commands::presence::tests::Fake;

/// The unbound Architect: every check passes but the launch record.
fn unbound(fx: &Fixture) -> HookEnv {
    HookEnv {
        claude: table(architect_table()),
        ..spoof_env(fx)
    }
}

fn pane(claude: Option<u32>) -> impl FnOnce() -> anyhow::Result<Pane> {
    move || {
        Ok(Pane {
            session: "tm-architect".to_owned(),
            claude,
        })
    }
}

fn assert_nothing_written(fx: &Fixture) {
    let root = fx.home.join(ANCHOR_ROOT);
    assert!(
        ARCHITECT_RECORDS
            .read(&root, ARCHITECT.pid)
            .expect("read")
            .is_none()
    );
    assert!(!grant_path(&root, ARCHITECT.pid).exists());
}

/// Spoof: a PM, or a session with no `claude`, gets no prompt and no record.
#[test]
fn a_pm_calling_fleet_bind_is_refused_before_any_prompt_8878() {
    let fx = fixture();
    let root = fx.home.join(ANCHOR_ROOT);
    let fake = Fake::new(PresenceOutcome::Granted);
    let source = KeySource::for_root(&root, true);
    for env in [pm_env(&fx), spoof_env(&fx)] {
        let out = bind(
            &env,
            || allowlist(&fx),
            &fake,
            &source,
            &root,
            pane(Some(60)),
        );
        assert!(out.is_err(), "{out:?}");
    }
    assert_eq!(fake.prompts.get(), 0);
    assert_nothing_written(&fx);
}

#[test]
fn fleet_bind_refuses_a_claude_that_is_not_the_panes_8878() {
    let fx = fixture();
    let root = fx.home.join(ANCHOR_ROOT);
    let source = KeySource::for_root(&root, true);
    let fake = Fake::new(PresenceOutcome::Granted);
    for claude in [None, Some(61)] {
        let env = unbound(&fx);
        assert!(bind(&env, || allowlist(&fx), &fake, &source, &root, pane(claude)).is_err());
    }
    let declined = Fake::new(PresenceOutcome::Declined("cancelled".into()));
    let env = unbound(&fx);
    assert!(
        bind(
            &env,
            || allowlist(&fx),
            &declined,
            &source,
            &root,
            pane(Some(60))
        )
        .is_err()
    );
    assert_eq!((fake.prompts.get(), declined.prompts.get()), (0, 1));
    assert_nothing_written(&fx);
}

/// Happy path: the unbound Architect is recorded and granted after one
/// prompt; a second run changes nothing.
#[test]
fn fleet_bind_records_and_grants_after_presence_8878() {
    let fx = fixture();
    let root = fx.home.join(ANCHOR_ROOT);
    let source = KeySource::for_root(&root, true);
    let fake = Fake::new(PresenceOutcome::Granted);
    // The record writer reads the live table, so the `claude` is this process.
    let pid = std::process::id();
    let start_time = process_facts(pid).expect("own facts").start_time;
    let me = ClaudeProcess { pid, start_time };
    let env = HookEnv {
        claude: ClaudeLookup::new(move || Ok(Some(me))),
        ..spoof_env(&fx)
    };
    let line = bind(
        &env,
        || allowlist(&fx),
        &fake,
        &source,
        &root,
        pane(Some(pid)),
    )
    .expect("bound");
    assert!(line.contains(&format!("Bound claude pid {pid}")), "{line}");
    let record = ARCHITECT_RECORDS
        .read(&root, pid)
        .expect("read")
        .expect("record");
    assert_eq!((record.pid, record.start_time), (pid, start_time));
    assert_eq!(
        architect_session_name(&root, pid).expect("session"),
        "tm-architect"
    );
    let key = source.load().expect("key").expect("made");
    let now = presence::unix_secs(std::time::SystemTime::now());
    assert!(presence::check(&root, &me, || Some(key), now).is_ok());
    assert_eq!(fake.prompts.get(), 1);
    // Bound now: a second run changes nothing and shows no prompt.
    let again = bind(
        &env,
        || allowlist(&fx),
        &fake,
        &source,
        &root,
        pane(Some(pid)),
    )
    .expect("already bound");
    assert!(again.contains("already bound"), "{again}");
    assert_eq!(fake.prompts.get(), 1);
}
