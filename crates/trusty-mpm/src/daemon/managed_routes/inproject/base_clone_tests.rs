//! Which account the managed base clone runs as (#9091).
//!
//! Why: the clone used to read the `[accounts]` table and the registry through
//! host state, so no test could drive it. Every case here injects the
//! registry pin, the table and the clone-credential step. The credential step
//! records the login and refuses, so no case reaches `git clone` or the network.
//! Test: this IS the test module.

use std::cell::RefCell;
use std::path::Path;

use super::*;

/// A github.com origin whose org `[accounts]` maps in [`mapped`].
const ORIGIN: &str = "https://github.com/Acme-9091/widget.git";

/// A table mapping `acme-9091` to `octo-mapped`.
fn mapped() -> Result<OrgAccounts, OrgAccountsError> {
    OrgAccounts::from_toml(
        "[accounts]\nacme-9091 = \"octo-mapped\"\n",
        Path::new("/home/u/.trusty-mpm/config.toml"),
    )
}

/// Clone `ORIGIN` into `base` with the given pin and table; return the clone's
/// error and the login the credential step was asked for.
fn clone_with(
    base: &Path,
    pin: impl FnOnce() -> Result<Option<RegistryPin>, String>,
    load: impl FnOnce() -> Result<OrgAccounts, OrgAccountsError>,
) -> (String, Option<String>) {
    let asked = RefCell::new(None);
    let err = ensure_base_clone_with(ORIGIN, base, None, pin, load, |login| {
        *asked.borrow_mut() = Some(login.to_string());
        Err("refused by the test before any network".to_string())
    })
    .expect_err("the test's credential step refuses");
    (err, asked.into_inner())
}

/// 🔴 #9091: an unpinned origin clones as its org's `[accounts]` login.
#[test]
fn a_mapped_org_clones_as_the_mapped_account() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let (err, asked) = clone_with(&tmp.path().join("acme/widget"), || Ok(None), mapped);
    assert_eq!(asked.as_deref(), Some("octo-mapped"));
    assert!(err.contains("cannot clone as octo-mapped"), "{err}");
}

/// 🔴 #9091 (config-convention.md "registry pin wins"): a pinned project
/// clones as the pin even when its org maps to a different login, so clone and
/// spawn use the same account.
#[test]
fn a_pinned_project_clones_as_the_pin_not_the_map() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let pin = || {
        Ok(Some(RegistryPin {
            account: Some("octo-pinned".into()),
            ..Default::default()
        }))
    };
    let (_, asked) = clone_with(&tmp.path().join("acme/widget"), pin, || {
        panic!("a pinned origin must not read the [accounts] table")
    });
    assert_eq!(asked.as_deref(), Some("octo-pinned"));
}

/// 🔴 #9091 Fail-Open Check: a broken table refuses the clone, and the refusal
/// leaves the disk as it was — an old-layout dir is not migrated aside, a
/// missing parent is not created, and no credential is built.
#[test]
fn a_broken_accounts_table_refuses_the_clone_and_touches_nothing() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let broken = || {
        OrgAccounts::from_toml(
            "[accounts]\nacme-9091 = octo\n",
            Path::new("/home/u/.trusty-mpm/config.toml"),
        )
    };
    let old_layout = tmp.path().join("acme/widget");
    std::fs::create_dir_all(&old_layout).expect("mkdir");
    std::fs::write(old_layout.join("marker"), b"old").expect("write");
    let fresh = tmp.path().join("never/widget");

    for base in [&old_layout, &fresh] {
        let err = ensure_base_clone_with(
            ORIGIN,
            base,
            None,
            || Ok(None),
            broken,
            |_| panic!("no credential is built for a refused clone"),
        )
        .expect_err("a broken table must refuse the clone");
        assert!(err.contains("config.toml"), "{err}");
    }
    assert!(old_layout.join("marker").exists(), "the old layout moved");
    let entries: Vec<_> = std::fs::read_dir(tmp.path().join("acme"))
        .expect("read")
        .collect();
    assert_eq!(entries.len(), 1, "nothing was migrated aside: {entries:?}");
    assert!(!tmp.path().join("never").exists(), "the parent was created");
}

/// 🔴 #9124: an origin carrying `x-access-token:<token>@` never reaches the
/// daemon log or the clone error. Both used to quote it verbatim.
///
/// The clone dials `127.0.0.1:9`, where nothing listens, so it fails at once
/// without the network. It runs inside the credential sandbox with no system
/// git config, so no credential helper or global config is in reach. No
/// assertion prints a captured line: on a regression that line holds the token.
#[test]
#[serial_test::serial]
fn a_credentialed_origin_never_reaches_the_log_or_the_error() {
    use tracing_subscriber::layer::SubscriberExt;
    use trusty_common::log_buffer::{LogBuffer, LogBufferLayer};

    const TOKEN: &str = "ghp_9124SyntheticNotARealToken0000";
    let origin = format!("https://x-access-token:{TOKEN}@127.0.0.1:9/acme/widget.git");
    let mut sandbox = trusty_common::credentials::test_sandbox::CredentialSandbox::enter();
    sandbox.set("GIT_CONFIG_NOSYSTEM", "1");
    sandbox.set("GIT_TERMINAL_PROMPT", "0");
    let base = sandbox.root().join("acme/widget");

    crate::test_support::enable_event_capture();
    let buffer = LogBuffer::new(64);
    let subscriber = tracing_subscriber::registry().with(LogBufferLayer::new(buffer.clone()));
    let err = tracing::subscriber::with_default(subscriber, || {
        ensure_base_clone_with(
            &origin,
            &base,
            None,
            || Ok(None),
            || Ok(OrgAccounts::default()),
            |_| panic!("an unmapped origin builds no account credential"),
        )
    })
    .expect_err("nothing listens on 127.0.0.1:9");

    let lines = buffer.tail(64);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("inproject: cloning base repo")),
        "the clone announcement was not captured ({} lines)",
        lines.len()
    );
    assert!(
        lines.iter().all(|l| !l.contains(TOKEN)),
        "a daemon log line carries the origin's token"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("https://***@127.0.0.1:9/acme/widget.git")),
        "the clone announcement does not name the redacted origin"
    );
    assert!(
        !err.contains(TOKEN),
        "the clone error carries the origin's token"
    );
}
