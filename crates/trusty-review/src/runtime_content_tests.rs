//! Tests for trusty-review's content resolution (#9015): the checkout
//! override is opt-in and names its root, and every refusal names a remedy.

use std::ffi::OsStr;

use trusty_common::content::{ContentSource, DevOverride};

use super::test_support::{fake_checkout, installed_without, repo_content_files, staged};
use super::*;

/// Why (#9015): a checkout must serve content only when the operator names
/// it; the code under review is a checkout too.
/// What: unset and empty are `Off`, and with no installed bundle that is
/// `NotInstalled`; an absolute path is `At`, and a trusted checkout there
/// serves its working tree.
#[test]
fn the_checkout_override_is_opt_in() {
    assert_eq!(dev_override(None).expect("unset"), DevOverride::Off);
    assert_eq!(
        dev_override(Some(OsStr::new(""))).expect("empty"),
        DevOverride::Off
    );
    let cache = tempfile::tempdir().expect("cache");
    let err = resolve_with(None, Some(cache.path().to_path_buf())).expect_err("nothing");
    assert!(
        matches!(err, PromptContentError::NotInstalled { .. }),
        "{err:?}"
    );

    let root = tempfile::tempdir().expect("root");
    let files: Vec<_> = repo_content_files()
        .into_iter()
        .map(|(path, bytes)| (format!("content/{path}"), bytes))
        .collect();
    fake_checkout(root.path(), &files);
    let named = Some(root.path().as_os_str());
    let content = resolve_with(named, Some(cache.path().to_path_buf())).expect("named checkout");
    assert!(
        matches!(content.source(), ContentSource::DevCheckout { root: r } if r == root.path()),
        "{:?}",
        content.source()
    );
    let stock = content_path("prompts/system_prompt_stock.md");
    assert!(!require_text(&content, &stock).expect("stock").is_empty());
}

/// Why: a relative value names a directory under the cwd, which is exactly
/// the source the override must never reach implicitly.
/// What: `CheckoutOverride`, naming the variable and the installed remedy.
#[test]
fn a_relative_checkout_override_is_refused() {
    let err = dev_override(Some(OsStr::new("."))).expect_err("relative");
    assert!(
        matches!(err, PromptContentError::CheckoutOverride { .. }),
        "{err:?}"
    );
    let text = err.to_string();
    assert!(text.contains(CHECKOUT_ENV), "{text}");
    assert!(text.contains("tm content update"), "{text}");
}

/// Why: an opted-in path that is not a trusted checkout must refuse rather
/// than fall through to the installed bundle the operator did not ask for.
/// What: an empty directory is `CheckoutOverride` naming the variable.
#[test]
fn an_untrusted_checkout_override_is_refused_naming_the_variable() {
    let cache = tempfile::tempdir().expect("cache");
    let root = tempfile::tempdir().expect("not a checkout");
    let err = resolve_with(
        Some(root.path().as_os_str()),
        Some(cache.path().to_path_buf()),
    )
    .expect_err("not a checkout");
    let PromptContentError::CheckoutOverride { value, .. } = &err else {
        panic!("expected CheckoutOverride, got {err:?}");
    };
    assert_eq!(value, root.path());
    assert!(err.to_string().contains(CHECKOUT_ENV), "{err}");
}

/// Why: a file missing from an opted-in checkout is fixed in that checkout;
/// the remedy says so, and how to stop using it.
/// What: `Missing` from a dev-checkout origin names `git pull` and the
/// variable; from an installed release it names `tm content update`.
#[test]
fn a_missing_file_names_the_remedy_for_its_source() {
    let path = content_path("voices/duetto/voice.toml");
    let dev = PromptContentError::Missing {
        origin: format!("{DEV_ORIGIN}/src/trusty-tools"),
        path: path.clone(),
    };
    assert!(dev.to_string().contains("git pull"), "{dev}");
    assert!(dev.to_string().contains(CHECKOUT_ENV), "{dev}");
    let installed = require_text(&staged(), &content_path("absent.md")).expect_err("absent");
    assert!(
        installed.to_string().contains("tm content update"),
        "{installed}"
    );
}

/// Why (#10588): the list is what an installer checks a release against, so
/// a content file this binary reads must not be left off it.
/// What: [`REQUIRED_FILES`] under [`CONTENT_DIR`] is exactly the checkout's
/// `content/instructions/trusty-review/` tree.
#[test]
fn the_required_files_are_the_checkout_content_tree() {
    let mut required: Vec<String> = REQUIRED_FILES.iter().map(|r| content_path(r)).collect();
    required.sort();
    let tree: Vec<String> = repo_content_files().into_iter().map(|(p, _)| p).collect();
    assert_eq!(required, tree);
}

/// Why (#10588): content released before this binary needed a file must be
/// refused naming that file, not passed.
/// What: a release without the principles addendum yields one `Missing`
/// naming its path.
#[test]
fn check_required_names_every_file_older_content_lacks() {
    let path = content_path("prompts/principles_addendum.md");
    let problems = check_required(&installed_without(&path));
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        matches!(&problems[0], PromptContentError::Missing { path: p, .. } if *p == path),
        "{problems:?}"
    );
}

/// What (#10588): the checkout's own content passes the check.
#[test]
fn check_required_passes_the_checkout_content() {
    let problems = check_required(&staged());
    assert!(problems.is_empty(), "{problems:?}");
}
