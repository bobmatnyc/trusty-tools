//! Tests for session-start registration and the checkout lookup (#10368).
//!
//! Every test uses its own temp registry directory; none reads or writes the
//! real `~/.trusty-mpm` state.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;
use crate::project::store::ProjectStore;

/// `git -C dir args…`, panicking on failure.
fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

/// A git checkout at `dir` whose origin is `remote`, canonicalized.
fn checkout(dir: &Path, remote: &str) -> PathBuf {
    std::fs::create_dir_all(dir).expect("mkdir checkout");
    git(dir, &["init", "-q"]);
    git(dir, &["remote", "add", "origin", remote]);
    dir.canonicalize().expect("canonical checkout")
}

/// The raw bytes of the registry file, or `None` when absent.
fn registry_bytes(registry: &Path) -> Option<Vec<u8>> {
    std::fs::read(registry.join("projects.json")).ok()
}

/// The regression: a session start in a subdirectory of a checkout registers
/// the checkout root, and the `owner/repo` lookup `tm secrets assign` uses
/// finds it. Before #10368 nothing wrote the row, so the lookup found nothing.
#[tokio::test]
async fn session_start_registers_the_checkout_and_assign_resolves_it() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = tmp.path().join("registry");
    let root = checkout(&tmp.path().join("widget"), "git@github.com:Acme/Widget.git");
    let sub = root.join("src");
    std::fs::create_dir(&sub).expect("mkdir sub");

    let outcome = register_session_checkout(&registry, &sub, None)
        .await
        .expect("register");

    assert_eq!(
        outcome,
        SessionRegistration::Registered {
            name: "widget".into()
        }
    );
    assert_eq!(
        registered_checkout(&registry, "acme/widget").expect("resolves"),
        root
    );
    assert!(matches!(
        registered_checkout(&registry, "acme/other"),
        Err(CheckoutLookupError::NotRegistered(_))
    ));
}

/// A second start writes nothing: no duplicate row, and a row someone else
/// configured for the same repository keeps every field it had.
#[tokio::test]
async fn session_start_registration_is_idempotent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = tmp.path().join("registry");
    let root = checkout(&tmp.path().join("widget"), "https://github.com/acme/widget");

    register_session_checkout(&registry, &root, None)
        .await
        .expect("first");
    let after_first = registry_bytes(&registry).expect("written");
    let again = register_session_checkout(&registry, &root, None)
        .await
        .expect("second");
    assert_eq!(
        again,
        SessionRegistration::AlreadyRegistered {
            name: "widget".into(),
            checkout: Some(root.clone()),
        }
    );
    assert_eq!(registry_bytes(&registry), Some(after_first));

    // A row an operator registered by URL, under another name, is kept as is.
    let other = tmp.path().join("other-registry");
    let mut store = ProjectStore::load(&other).await.expect("store");
    let mut configured = session_project("acme-widget", "https://github.com/acme/widget");
    configured.gh_account = Some("acme-bot".into());
    configured.description = Some("configured elsewhere".into());
    store.upsert(configured).await.expect("seed");
    let before = registry_bytes(&other).expect("seeded");
    let kept = register_session_checkout(&other, &root, None)
        .await
        .expect("kept");
    assert!(
        matches!(&kept, SessionRegistration::AlreadyRegistered { name, .. } if name == "acme-widget"),
        "{kept:?}"
    );
    assert_eq!(registry_bytes(&other), Some(before));
}

/// Fail closed: a corrupt registry is an error from the session start and from
/// the lookup, never a registration, and the file is left as it was.
#[tokio::test]
async fn session_start_registration_failure_is_an_error_not_a_registration() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = tmp.path().join("registry");
    std::fs::create_dir(&registry).expect("mkdir registry");
    std::fs::write(registry.join("projects.json"), b"{ not json").expect("corrupt");
    let root = checkout(&tmp.path().join("widget"), "https://github.com/acme/widget");

    let outcome = register_session_checkout(&registry, &root, None).await;

    assert!(
        matches!(
            outcome,
            Err(SessionRegistrationError::Store(
                ProjectStoreError::Serialize(_)
            ))
        ),
        "{outcome:?}"
    );
    assert!(matches!(
        registered_checkout(&registry, "acme/widget"),
        Err(CheckoutLookupError::Store(_))
    ));
    assert_eq!(
        registry_bytes(&registry).as_deref(),
        Some(&b"{ not json"[..])
    );
}

/// D50: a home directory that is itself a git repository, or sits inside one,
/// is never registered; a name another repository holds is an error too.
#[tokio::test]
async fn session_start_never_registers_home_or_its_ancestors() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = tmp.path().join("registry");
    let home = checkout(&tmp.path().join("home"), "https://github.com/me/dotfiles");
    let scratch = home.join("scratch");
    std::fs::create_dir(&scratch).expect("mkdir scratch");

    let outcome = register_session_checkout(&registry, &scratch, Some(&home)).await;
    assert!(
        matches!(&outcome, Err(SessionRegistrationError::Unregistrable(p)) if *p == home),
        "{outcome:?}"
    );
    let inner = home.join("deeper");
    std::fs::create_dir(&inner).expect("mkdir deeper");
    let outcome =
        register_session_checkout(&registry, &tmp.path().join("home"), Some(&inner)).await;
    assert!(
        matches!(outcome, Err(SessionRegistrationError::Unregistrable(_))),
        "{outcome:?}"
    );
    assert_eq!(registry_bytes(&registry), None);

    // Same basename, another repository: refused, and the lookup finds nothing.
    let first = checkout(&tmp.path().join("a/api"), "https://github.com/acme/api");
    let second = checkout(&tmp.path().join("b/api"), "https://github.com/other/api");
    register_session_checkout(&registry, &first, None)
        .await
        .expect("first api");
    let clash = register_session_checkout(&registry, &second, None).await;
    assert!(
        matches!(&clash, Err(SessionRegistrationError::NameTaken { name, .. }) if name == "api"),
        "{clash:?}"
    );
    assert!(matches!(
        registered_checkout(&registry, "other/api"),
        Err(CheckoutLookupError::NotRegistered(_))
    ));
}

/// Two legacy rows (no stored key) naming two checkouts of one repository
/// resolve to neither.
#[test]
fn registered_checkout_refuses_two_checkouts() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = tmp.path().join("registry");
    let one = checkout(&tmp.path().join("one"), "https://github.com/acme/web");
    let two = checkout(&tmp.path().join("two"), "https://github.com/acme/web");
    let rows: std::collections::HashMap<_, _> = [("one", &one), ("two", &two)]
        .into_iter()
        .map(|(name, dir)| (name, session_project(name, &dir.to_string_lossy())))
        .collect();
    std::fs::create_dir(&registry).expect("mkdir registry");
    let doc = serde_json::json!({ "projects": rows });
    std::fs::write(registry.join("projects.json"), doc.to_string()).expect("write");

    assert!(matches!(
        registered_checkout(&registry, "Acme/Web"),
        Err(CheckoutLookupError::Ambiguous(_))
    ));
}
