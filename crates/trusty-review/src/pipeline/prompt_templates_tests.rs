//! Tests for the reviewer prompts read from content (#9015): the checkout's
//! text, and one test per fail-closed arm.

use std::path::Path;

use trusty_common::content::{
    ContentError, ContentLock, DevOverride, LOCK_FILE_NAME, ResolveOptions, resolve,
};
use trusty_common::integrity::Sha256Digest;

use super::test_support::{fake_checkout, prompts, repo_prompt_files};
use super::*;

/// Loads the prompts from a staged checkout holding `files`.
fn load_from_checkout(
    root: &Path,
    files: &[(String, Vec<u8>)],
) -> Result<ReviewerPrompts, PromptContentError> {
    fake_checkout(root, files);
    let options = ResolveOptions::new(root, DevOverride::At(root.to_path_buf()));
    ReviewerPrompts::from_content(&resolve(&options)?)
}

/// Pin the prompt bytes so the move into `content/` can never silently drift.
///
/// Why: the prompts feed the reviewer LLM and are part of the crate's
/// observable contract; #9015 moved them, and the bytes must not change.
/// What: asserts the exact opening line, exact closing text, and the frozen
/// byte lengths of both prompts as the resolver serves them.
#[test]
fn the_repository_prompts_load_byte_identical() {
    let p = prompts();
    let opening = "You are a senior software engineer performing a pull-request code review.";
    assert!(p.stock().starts_with(opening));
    assert!(p.coverage_gating().starts_with(opening));

    // Both end with the same closing sentence and no trailing newline (the
    // original literals closed `"#;` on the same line).
    let closing = "`findings` may be an empty array if there are no issues.";
    assert!(p.stock().ends_with(closing));
    assert!(p.coverage_gating().ends_with(closing));
    assert!(!p.stock().ends_with('\n'));
    assert!(!p.coverage_gating().ends_with('\n'));

    // Frozen byte lengths (last changed by the quote-the-cited-line rule, #8905).
    assert_eq!(p.stock().len(), 14239);
    assert_eq!(p.coverage_gating().len(), 14555);

    // The coverage-gating variant is distinguished by its coverage-floor note.
    assert!(
        p.coverage_gating()
            .contains("deterministic\ncoverage floor")
    );
    assert!(!p.stock().contains("deterministic\ncoverage floor"));
    assert_eq!(p.for_coverage(true), p.coverage_gating());
    assert_eq!(p.for_coverage(false), p.stock());
}

/// No checkout and no lock: `NotInstalled`, naming the fetch and the offline
/// install.
#[test]
fn no_content_is_not_installed_naming_the_remedy() {
    let cache = tempfile::tempdir().expect("cache");
    let err = resolve(&ResolveOptions::new(cache.path(), DevOverride::Off))
        .map_err(PromptContentError::from)
        .and_then(|c| ReviewerPrompts::from_content(&c))
        .expect_err("nothing is installed");
    assert!(
        matches!(err, PromptContentError::NotInstalled { .. }),
        "{err:?}"
    );
    let text = err.to_string();
    assert!(text.contains("tm content update"), "{text}");
    assert!(text.contains("tm content install --from"), "{text}");
}

/// A bundle whose sha256 does not match the lock is a `Content` error, and
/// its text still names `tm content update`.
#[test]
fn an_unverifiable_bundle_is_a_content_error_naming_the_remedy() {
    let cache = tempfile::tempdir().expect("cache");
    let lock = ContentLock::new("content-v0.0.1", Sha256Digest::of_bytes(b"pinned")).expect("lock");
    std::fs::write(cache.path().join(lock.bundle_file_name()), b"tampered").expect("bundle");
    lock.store(&cache.path().join(LOCK_FILE_NAME))
        .expect("store");
    let err: PromptContentError = resolve(&ResolveOptions::new(cache.path(), DevOverride::Off))
        .expect_err("the checksum does not match")
        .into();
    assert!(matches!(err, PromptContentError::Content { .. }), "{err:?}");
    assert!(err.to_string().contains("tm content update"), "{err}");
}

/// Content released before #9015 has no prompts: `Missing`, naming the path.
/// From a checkout the remedy is `git pull`, not the cache.
#[test]
fn a_release_without_the_prompts_is_missing_naming_the_path() {
    let root = tempfile::tempdir().expect("root");
    let mut files = repo_prompt_files();
    files.retain(|(rel, _)| !rel.ends_with(COVERAGE_GATING_FILE));
    let err = load_from_checkout(root.path(), &files).expect_err("a prompt is absent");
    let PromptContentError::Missing { path, .. } = &err else {
        panic!("expected Missing, got {err:?}");
    };
    assert_eq!(path, &prompt_path(COVERAGE_GATING_FILE));
    let text = err.to_string();
    assert!(text.contains(&prompt_path(COVERAGE_GATING_FILE)), "{text}");
    assert!(text.contains("git pull"), "{text}");
}

/// An empty prompt is `Invalid`, never a review with an empty system prompt.
#[test]
fn an_empty_prompt_is_invalid() {
    let root = tempfile::tempdir().expect("root");
    let mut files = repo_prompt_files();
    for (rel, body) in &mut files {
        if rel.ends_with(STOCK_FILE) {
            *body = b"  \n".to_vec();
        }
    }
    let err = load_from_checkout(root.path(), &files).expect_err("a prompt is empty");
    assert!(
        matches!(&err, PromptContentError::Invalid { path, .. } if path == &prompt_path(STOCK_FILE)),
        "{err:?}"
    );
}

/// Every variant's text carries a remedy (#9015: there is no write arm).
#[test]
fn every_prompt_content_error_names_its_remedy() {
    let installed = "content-v2.2.2".to_owned();
    let path = prompt_path(STOCK_FILE);
    let cases = [
        PromptContentError::NotInstalled {
            source: ContentError::NotInstalled {
                lock_path: "/nowhere/content-lock.toml".into(),
            },
        },
        PromptContentError::NoCacheDir,
        PromptContentError::Content {
            source: ContentError::NotFound { path: path.clone() },
        },
        PromptContentError::Missing {
            origin: installed.clone(),
            path: path.clone(),
        },
        PromptContentError::Invalid {
            origin: installed,
            path,
            reason: "the file is empty".to_owned(),
        },
        PromptContentError::CheckoutOverride {
            value: "relative".into(),
            reason: "it is not an absolute path".to_owned(),
        },
    ];
    for err in cases {
        assert!(err.to_string().contains("tm content update"), "{err}");
    }
}
