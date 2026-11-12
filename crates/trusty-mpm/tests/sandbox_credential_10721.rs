//! The sandbox's one admitted credential, for real, in a process of its own
//! (#10721).
//!
//! Why: `secret_source::enter_sandbox_with_credential` sets the one-way latch
//! and a set-once slot, and removes a variable from the process environment.
//! Inside a shared test binary any of the three would change what a sibling
//! test reads, so this target is its own `[[test]]` binary.
//! What: one `#[serial]` test inside the credential sandbox, with FAKE values
//! only. It admits `ANTHROPIC_API_KEY`, then proves: the variable has left the
//! environment, a spawned child inherits neither its name nor its value, the
//! admitted key resolves and reaches the provider registry, every other
//! credential stays `None` under the latch, and a second admission or a name
//! off the list refuses. No assertion prints a resolved value.
//! Test: this file IS the test module.

// #8545: `common` arms the home-write fence before `main`.
mod common;

use std::process::Command;

use serial_test::serial;
use trusty_common::credentials::test_sandbox::{CredentialSandbox, assert_secret_eq};
use trusty_mpm::core::sm::providers::ProviderRegistry;
use trusty_mpm::secret_source::{
    SandboxCredentialError, enter_sandbox_with_credential, resolve_secret, sandboxed,
};

/// A fake provider key. Never a real credential.
const FAKE_ADMITTED: &str = "sk-ant-fake-10721-admitted-never-real";

/// A fake key that must stay unreachable. Never a real credential.
const FAKE_OTHER: &str = "sk-or-v1-fake-10721-other-never-real";

/// A fake bot token that must stay unreachable. Never a real credential.
const FAKE_TOKEN: &str = "10721000:fake-telegram-token-never-real";

/// Why (#10721, Architect ruling Q2): the daemon reads its one credential once
/// and removes it, so the tmux and claude children it spawns cannot inherit
/// it; only the admitted name resolves under the latch.
/// What: one test, because the latch and the slot are set once per process.
/// Test: this test.
#[test]
#[serial]
fn the_admitted_credential_resolves_and_leaves_the_environment() {
    let mut sandbox = CredentialSandbox::enter();
    sandbox.set("ANTHROPIC_API_KEY", FAKE_ADMITTED);
    sandbox.set("TELEGRAM_BOT_TOKEN", FAKE_TOKEN);

    // A name off the list refuses, latches, and leaves the env untouched.
    assert_eq!(
        enter_sandbox_with_credential("TELEGRAM_BOT_TOKEN"),
        Err(SandboxCredentialError::NameNotAllowed(
            "TELEGRAM_BOT_TOKEN".to_string()
        ))
    );
    assert!(sandboxed(), "a refused admission did not latch");
    assert!(resolve_secret("TELEGRAM_BOT_TOKEN").is_none());

    assert_eq!(enter_sandbox_with_credential("ANTHROPIC_API_KEY"), Ok(()));

    assert!(
        std::env::var_os("ANTHROPIC_API_KEY").is_none(),
        "the admitted credential is still in the process environment"
    );
    // The children the daemon spawns inherit this process's environment.
    let child = Command::new("/usr/bin/env").output().expect("spawn env");
    let child_env = String::from_utf8_lossy(&child.stdout);
    assert!(
        !child_env.contains("ANTHROPIC_API_KEY") && !child_env.contains(FAKE_ADMITTED),
        "a spawned child inherited the admitted credential"
    );

    assert_secret_eq(
        resolve_secret("ANTHROPIC_API_KEY").as_deref(),
        Some(FAKE_ADMITTED),
        "the admitted credential",
    );
    let registry = ProviderRegistry::from_env();
    assert_secret_eq(
        registry.anthropic_api_key.as_deref(),
        Some(FAKE_ADMITTED),
        "the provider registry's Anthropic key",
    );
    assert!(registry.openrouter_api_key.is_none());
    assert!(resolve_secret("TELEGRAM_BOT_TOKEN").is_none());

    // A second admission refuses and still removes its variable.
    sandbox.set("OPENROUTER_API_KEY", FAKE_OTHER);
    assert_eq!(
        enter_sandbox_with_credential("OPENROUTER_API_KEY"),
        Err(SandboxCredentialError::AlreadyAdmitted)
    );
    assert!(std::env::var_os("OPENROUTER_API_KEY").is_none());
    assert!(
        resolve_secret("OPENROUTER_API_KEY").is_none(),
        "a credential that was not admitted resolved"
    );
}
