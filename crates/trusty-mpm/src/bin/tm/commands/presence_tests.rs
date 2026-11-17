//! Tests for `tm presence local` (`presence.rs`), #8878 PR2a. A fake
//! authenticator stands in for Touch ID; every refusal asserts no
//! `.presence` was written.

use std::cell::Cell;

use trusty_mpm::core::presence::grant_path;
use trusty_mpm::core::presence_local::{Authenticator, PresenceOutcome};

use super::*;
use crate::commands::pm_guard_trust_anchor::tests::{
    ARCHITECT, allowlist, bound_env, fixture, pm_env, spoof_env,
};

/// A fake authenticator: answers `outcome` and counts its prompts.
pub(crate) struct Fake {
    pub(crate) outcome: PresenceOutcome,
    pub(crate) prompts: Cell<usize>,
}

impl Fake {
    pub(crate) fn new(outcome: PresenceOutcome) -> Self {
        Self {
            outcome,
            prompts: Cell::new(0),
        }
    }
}

impl Authenticator for Fake {
    fn authenticate(&self, _reason: &str) -> PresenceOutcome {
        self.prompts.set(self.prompts.get() + 1);
        self.outcome.clone()
    }
}

fn source(root: &Path) -> KeySource {
    KeySource::for_root(root, true)
}

/// Spoof: a PM (and an unbound or subagent caller) gets no prompt and no grant.
#[test]
fn a_pm_calling_presence_local_is_refused_before_any_prompt_8878() {
    let fx = fixture();
    let root = fx.home.join(ANCHOR_ROOT);
    let fake = Fake::new(PresenceOutcome::Granted);
    let subagent = HookEnv {
        sub_agent: true,
        ..bound_env(&fx)
    };
    for env in [pm_env(&fx), spoof_env(&fx), subagent] {
        let err =
            local(&env, || allowlist(&fx), &fake, &source(&root), &root).expect_err("refused");
        assert!(err.to_string().contains("presence unavailable"), "{err}");
    }
    assert_eq!(fake.prompts.get(), 0, "no prompt was shown");
    assert!(
        !grant_path(&root, ARCHITECT.pid).exists(),
        "no grant was written"
    );
}

#[test]
fn a_declined_or_unavailable_prompt_writes_no_grant_8878() {
    let fx = fixture();
    let root = fx.home.join(ANCHOR_ROOT);
    for outcome in [
        PresenceOutcome::Declined("cancelled".into()),
        PresenceOutcome::Unavailable("no GUI session".into()),
    ] {
        let fake = Fake::new(outcome);
        let env = bound_env(&fx);
        assert!(local(&env, || allowlist(&fx), &fake, &source(&root), &root).is_err());
        assert_eq!(fake.prompts.get(), 1);
        assert!(!grant_path(&root, ARCHITECT.pid).exists());
    }
}

/// Happy path: the bound Architect, a granted prompt, a grant the gate accepts.
#[test]
fn presence_local_grants_the_bound_architect_8878() {
    let fx = fixture();
    let root = fx.home.join(ANCHOR_ROOT);
    let fake = Fake::new(PresenceOutcome::Granted);
    let path = local(
        &bound_env(&fx),
        || allowlist(&fx),
        &fake,
        &source(&root),
        &root,
    )
    .expect("granted");
    assert_eq!(path, grant_path(&root, ARCHITECT.pid));
    let key = source(&root)
        .load()
        .expect("key")
        .expect("made on first use");
    let now = presence::unix_secs(std::time::SystemTime::now());
    assert!(presence::check(&root, &ARCHITECT, || Some(key), now).is_ok());
}
