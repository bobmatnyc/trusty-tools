//! Unit tests for the session-ref privacy gate (See #TBD-pause-ref).

use std::path::Path;

use super::*;

/// A lookup failure and garbled output are `Unknown`, never `Private`.
/// Test: itself.
#[test]
fn gh_output_maps_to_visibility() {
    let cases: [(Result<String, String>, RepoVisibility); 5] = [
        (Ok("true\n".into()), RepoVisibility::Private),
        (Ok("false".into()), RepoVisibility::Public("o/r".into())),
        (
            Ok("null".into()),
            RepoVisibility::Unknown("`gh api` answered \"null\"".into()),
        ),
        (
            Err("HTTP 404".into()),
            RepoVisibility::Unknown("HTTP 404".into()),
        ),
        (
            Err("not logged in".into()),
            RepoVisibility::Unknown("not logged in".into()),
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(
            visibility_from_gh("o/r", input.clone()),
            expected,
            "{input:?}"
        );
    }
}

/// Only a confirmed private repository lets the publish run.
/// Test: itself.
#[test]
fn only_a_private_repo_is_allowed() {
    assert_eq!(RepoVisibility::Private.refusal(), None);
    let public = RepoVisibility::Public("o/r".into())
        .refusal()
        .unwrap_or_default();
    assert!(public.contains("o/r is a public repository"), "{public}");
    let unknown = RepoVisibility::Unknown("HTTP 404".into())
        .refusal()
        .unwrap_or_default();
    assert!(
        unknown.contains("could not be confirmed as private"),
        "{unknown}"
    );
}

/// The home directory becomes `~`; a missing home changes nothing.
/// Test: itself.
#[test]
fn home_paths_are_redacted() {
    let text = "worktree /home/ada/src/x and /home/ada/.trusty-mpm/s.md";
    assert_eq!(
        redact_home(text, Some(Path::new("/home/ada/"))),
        "worktree ~/src/x and ~/.trusty-mpm/s.md"
    );
    assert_eq!(redact_home(text, None), text);
    assert_eq!(redact_home(text, Some(Path::new("/"))), text);
}

/// The id is deterministic, ref-safe, and carries no part of the input.
/// Test: itself.
#[test]
fn anonymous_user_id_is_stable_and_non_personal() {
    let id = anonymous_user_id("RobertMasaMatsuoka");
    assert_eq!(id, anonymous_user_id("RobertMasaMatsuoka"));
    assert_ne!(id, anonymous_user_id("octocat"));
    assert_eq!(id.len(), 13);
    assert!(!id.contains("Robert"), "{id}");
    assert_eq!(
        crate::core::session_ref_publish::sanitize_ref_component(&id).as_deref(),
        Some(id.as_str())
    );
}
