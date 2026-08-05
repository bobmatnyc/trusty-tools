//! Tests for `tm register` positional resolution and alias derivation (#4912).
//!
//! Why: three things must be pinned or they regress silently. The legacy
//! `<alias> <url>` order must still resolve correctly. The derived alias must be
//! HYPHEN-joined `owner-repo` for every real URL shape. And — the #4912 review's
//! HIGH — a lone argument that is NOT a clone-able URL must be REJECTED, because
//! `owner/repo` shorthand otherwise registers an unclonable URL under the wrong
//! alias with exit 0, and does not even collide with the later correct
//! registration.
//! What: unit tests over [`super::resolve_register_args`], the URL-shape test,
//! and the boundary path splitter, plus end-to-end tests through
//! `standalone::register_cmd` proving a collision refuses without mutating
//! `registry.json`.
//! Test: this file.

use super::{looks_like_url, path_segments, resolve_register_args};

// ---------------------------------------------------------------------------
// Positional order
// ---------------------------------------------------------------------------

/// New order: `tm register <url> <alias>`.
#[test]
fn two_args_url_first() {
    let (alias, url) =
        resolve_register_args("https://github.com/owner/repo", Some("my-alias")).unwrap();
    assert_eq!(alias, "my-alias");
    assert_eq!(url, "https://github.com/owner/repo");
}

/// Legacy order: `tm register <alias> <url>` must keep working (#4912).
#[test]
fn two_args_legacy_alias_first() {
    let (alias, url) =
        resolve_register_args("my-alias", Some("https://github.com/owner/repo")).unwrap();
    assert_eq!(alias, "my-alias");
    assert_eq!(url, "https://github.com/owner/repo");
}

/// Legacy order with the SSH form — the `git@` prefix is the URL signal.
#[test]
fn two_args_legacy_ssh_url_second() {
    let (alias, url) =
        resolve_register_args("proj", Some("git@github.com:owner/repo.git")).unwrap();
    assert_eq!(alias, "proj");
    assert_eq!(url, "git@github.com:owner/repo.git");
}

/// One positional: the alias is derived, not required.
#[test]
fn one_arg_derives() {
    let (alias, url) = resolve_register_args("https://github.com/owner/repo", None).unwrap();
    assert_eq!(alias, "owner-repo");
    assert_eq!(url, "https://github.com/owner/repo");
}

/// A single non-URL positional is a usage error, not a half-registration.
#[test]
fn one_arg_non_url_errors() {
    let err = resolve_register_args("my-alias", None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("is not a repository URL"), "unexpected: {err}");
}

/// THE #4912 REVIEW HIGH. `gh`-style `owner/repo` shorthand must be REJECTED,
/// not silently read as `host/repo`. Before the fix this registered alias
/// `trusty-tools` → URL `bobmatnyc/trusty-tools` with exit 0, and did not even
/// collide with the later correct `bobmatnyc-trusty-tools` registration.
///
/// It is rejected rather than expanded on purpose: expanding `owner/repo` to
/// `https://github.com/owner/repo` is a product decision about defaulting to
/// GitHub, and is out of scope for #4912.
#[test]
fn one_arg_shorthand_is_rejected() {
    for shorthand in ["bobmatnyc/trusty-tools", "owner/repo", "a/b/c"] {
        let err = resolve_register_args(shorthand, None)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("is not a repository URL"),
            "{shorthand} must be rejected, got: {err}"
        );
        // The message must show the way out, not just the failure.
        assert!(err.contains("https://github.com/"), "no remedy in: {err}");
    }
}

/// Two URL-shaped arguments are ambiguous — refuse rather than guess.
#[test]
fn two_urls_error() {
    let err = resolve_register_args("https://github.com/a/b", Some("https://github.com/c/d"))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("both arguments look like URLs"),
        "unexpected: {err}"
    );
}

/// Two alias-shaped arguments mean no URL was supplied at all. Since the review
/// fix, `owner/repo` shorthand lands here too rather than being taken as a URL.
#[test]
fn two_non_urls_error() {
    for (a, b) in [("alpha", "beta"), ("mine", "owner/repo")] {
        let err = resolve_register_args(a, Some(b)).unwrap_err().to_string();
        assert!(
            err.contains("neither argument looks like a repository URL"),
            "unexpected for ({a}, {b}): {err}"
        );
    }
}

/// The shape test must never classify a valid alias as a URL, and vice versa.
#[test]
fn looks_like_url_accepts_url_shapes() {
    assert!(looks_like_url("https://github.com/owner/repo"));
    assert!(looks_like_url("git@github.com:owner/repo.git"));
    assert!(looks_like_url("github.com/owner/repo"));
    assert!(looks_like_url("ssh://git@example.com:2222/owner/repo.git"));
    assert!(looks_like_url("localhost:3000/owner/repo"));
    // Local clone sources stay acceptable — they were before #4912.
    assert!(looks_like_url("/srv/git/repo.git"));
    assert!(looks_like_url("~/src/repo.git"));
}

/// Aliases are `^[a-z0-9][a-z0-9._-]*$` — none of them can look like a URL.
#[test]
fn looks_like_url_rejects_aliases() {
    for alias in ["my-alias", "proj", "a.b-c", "repo123", "owner-repo"] {
        assert!(!looks_like_url(alias), "{alias} misread as a URL");
    }
}

/// A `/` alone is not enough: without a host-shaped first segment this is
/// `gh` shorthand, not a URL (#4912 review HIGH).
#[test]
fn looks_like_url_rejects_gh_shorthand() {
    for s in ["owner/repo", "bobmatnyc/trusty-tools", "a/b/c", "owner/"] {
        assert!(!looks_like_url(s), "{s} misread as a URL");
    }
}

// ---------------------------------------------------------------------------
// Alias derivation
// ---------------------------------------------------------------------------

/// Every real URL shape derives the same HYPHEN-joined `owner-repo`.
#[test]
fn derives_from_every_url_shape() {
    let cases = [
        ("https://github.com/owner/repo", "owner-repo"),
        ("https://github.com/owner/repo.git", "owner-repo"),
        ("https://github.com/owner/repo/", "owner-repo"),
        ("https://github.com/owner/repo.git/", "owner-repo"),
        ("git@github.com:owner/repo.git", "owner-repo"),
        ("git@github.com:owner/repo", "owner-repo"),
        ("github.com/owner/repo", "owner-repo"),
        // Non-GitHub hosts derive identically — nothing here is GitHub-specific.
        ("https://gitlab.com/acme/widget.git", "acme-widget"),
        ("https://git.example.com/team/thing", "team-thing"),
        (
            "ssh://git@git.example.com:2222/team/thing.git",
            "team-thing",
        ),
    ];
    for (url, want) in cases {
        let (alias, got_url) = resolve_register_args(url, None).unwrap();
        assert_eq!(alias, want, "wrong alias for {url}");
        assert_eq!(got_url, url);
    }
}

/// No discernible owner → fall back to the repo segment alone (the stated
/// #4912 decision; erroring would reject valid self-hosted single-segment
/// paths).
#[test]
fn no_owner_falls_back_to_repo() {
    let (alias, _) = resolve_register_args("https://example.com/repo", None).unwrap();
    assert_eq!(alias, "repo");
}

/// A host-only URL yields nothing to name. BOTH forms must error — the
/// no-trailing-slash one silently derived `examplecom` before the #4912 review.
#[test]
fn host_only_url_errors() {
    for url in [
        "https://example.com/",
        "https://example.com",
        "https://example.com:8080",
        "git@github.com:",
    ] {
        let err = resolve_register_args(url, None).unwrap_err().to_string();
        assert!(
            err.contains("no owner/repo path"),
            "{url} must be rejected, got: {err}"
        );
    }
}

/// Browser pastes point INTO a repo and derive nonsense from the last two path
/// segments (`tree-main`, `pull-4914`). Reject with the repo-root form named.
#[test]
fn browser_paste_shapes_are_rejected() {
    let cases = [
        "https://github.com/owner/repo/tree/main",
        "https://github.com/owner/repo/pull/4914",
        "https://github.com/owner/repo/blob/main/README.md",
        "https://github.com/owner/repo/issues/12",
        "https://github.com/owner/repo/commit/abc123",
        "https://gitlab.com/group/sub/repo/-/blob/main/x",
    ];
    for url in cases {
        let err = resolve_register_args(url, None).unwrap_err().to_string();
        assert!(
            err.contains("points inside a repository"),
            "{url} must be rejected, got: {err}"
        );
        assert!(err.contains("repository root"), "no remedy in: {err}");
    }
}

/// The web-path words are only rejected in an interior position — a repo
/// actually NAMED `tree` (or a GitLab subgroup path ending in one) still works.
#[test]
fn repo_named_like_a_web_path_is_ok() {
    let (alias, _) = resolve_register_args("https://github.com/owner/tree", None).unwrap();
    assert_eq!(alias, "owner-tree");
    let (alias, _) = resolve_register_args("https://gitlab.com/group/sub/blob", None).unwrap();
    assert_eq!(alias, "sub-blob");
}

/// A query string or fragment is never part of a clone URL, and both corrupt the
/// derived alias. Strip them from the stored URL rather than storing an
/// unclonable string.
#[test]
fn query_and_fragment_are_stripped() {
    let (alias, url) =
        resolve_register_args("https://github.com/owner/repo?tab=readme-ov-file", None).unwrap();
    assert_eq!(alias, "owner-repo");
    assert_eq!(url, "https://github.com/owner/repo");

    let (alias, url) = resolve_register_args("https://github.com/owner/repo#readme", None).unwrap();
    assert_eq!(alias, "owner-repo");
    assert_eq!(url, "https://github.com/owner/repo");
}

/// The `a/b-c` vs `a-b/c` flattening ambiguity: both derive the SAME alias.
/// That is accepted, not special-cased — the second registration is caught by
/// the ordinary different-URL collision refusal (see
/// `collision_refuses_and_leaves_registry_untouched`).
#[test]
fn hyphen_flattening_ambiguity_is_accepted_and_collides() {
    let (a, _) = resolve_register_args("https://github.com/a/b-c", None).unwrap();
    let (b, _) = resolve_register_args("https://github.com/a-b/c", None).unwrap();
    assert_eq!(a, "a-b-c");
    assert_eq!(b, "a-b-c");
}

// ---------------------------------------------------------------------------
// Boundary path splitter
// ---------------------------------------------------------------------------

/// The local validator must agree with the shared deriver on where the host
/// ends, for every shape both see — otherwise a rejection fires on a URL the
/// deriver would have handled, or vice versa.
#[test]
fn path_segments_matches_every_url_shape() {
    let cases: [(&str, Vec<&str>); 8] = [
        ("https://github.com/owner/repo", vec!["owner", "repo"]),
        ("https://github.com/owner/repo/", vec!["owner", "repo"]),
        ("github.com/owner/repo", vec!["owner", "repo"]),
        ("git@github.com:owner/repo.git", vec!["owner", "repo.git"]),
        (
            "ssh://git@git.example.com:2222/team/thing.git",
            vec!["team", "thing.git"],
        ),
        ("https://example.com:8080/o/r", vec!["o", "r"]),
        ("https://example.com/repo", vec!["repo"]),
        (
            "https://github.com/o/r/tree/main",
            vec!["o", "r", "tree", "main"],
        ),
    ];
    for (url, want) in cases {
        assert_eq!(path_segments(url), want, "wrong segments for {url}");
    }
}

/// The port-drop must key off the `:` host terminator, not "the first segment
/// is digits" — otherwise an all-numeric owner is silently swallowed.
#[test]
fn path_segments_keeps_numeric_owner() {
    assert_eq!(
        path_segments("https://github.com/123/repo"),
        ["123", "repo"]
    );
    let (alias, _) = resolve_register_args("https://github.com/123/repo", None).unwrap();
    assert_eq!(alias, "123-repo");
}

/// A host with no path yields no segments — with or without a trailing slash.
#[test]
fn path_segments_host_only() {
    for url in ["https://example.com", "https://example.com/", "example.com"] {
        assert!(
            path_segments(url).is_empty(),
            "{url} must yield no path segments"
        );
    }
}

// ---------------------------------------------------------------------------
// Collision behaviour, end to end through the command handler
// ---------------------------------------------------------------------------

/// A derived alias already bound to a DIFFERENT URL must refuse and leave the
/// registry file byte-identical. Silent rebinding is the failure shape #4912
/// explicitly forbids.
#[test]
fn collision_refuses_and_leaves_registry_untouched() {
    let dir = tempfile::TempDir::new().unwrap();
    let paths = crate::commands::managed_root::ManagedPaths::from_root(dir.path().to_path_buf());

    // First registration derives `a-b-c` from `a/b-c`.
    crate::commands::standalone::register_cmd(&paths, "https://github.com/a/b-c", None, false)
        .unwrap();
    let registry_path = dir.path().join("registry.json");
    let before = std::fs::read(&registry_path).unwrap();

    // `a-b/c` flattens to the same alias but is a different URL.
    let err =
        crate::commands::standalone::register_cmd(&paths, "https://github.com/a-b/c", None, false)
            .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("already registered"),
        "collision must be loud: {msg}"
    );

    let after = std::fs::read(&registry_path).unwrap();
    assert_eq!(
        before, after,
        "a refused registration must not mutate the registry"
    );
}

/// A rejected URL must not create a registry file at all — the refusal happens
/// before any load or save.
#[test]
fn rejected_url_writes_nothing() {
    let dir = tempfile::TempDir::new().unwrap();
    let paths = crate::commands::managed_root::ManagedPaths::from_root(dir.path().to_path_buf());
    assert!(
        crate::commands::standalone::register_cmd(&paths, "bobmatnyc/trusty-tools", None, false)
            .is_err()
    );
    assert!(
        !dir.path().join("registry.json").exists(),
        "a rejected URL must not write a registry"
    );
}

/// Re-registering the SAME url under the same derived alias is idempotent, not
/// an error — nothing is being rebound.
#[test]
fn same_url_reregistration_is_idempotent() {
    let dir = tempfile::TempDir::new().unwrap();
    let paths = crate::commands::managed_root::ManagedPaths::from_root(dir.path().to_path_buf());
    let url = "https://github.com/owner/repo";
    crate::commands::standalone::register_cmd(&paths, url, None, false).unwrap();
    crate::commands::standalone::register_cmd(&paths, url, None, false).unwrap();

    let registry =
        trusty_mpm::core::standalone::registry::ManagedRegistry::load(dir.path()).unwrap();
    let entries = registry.list();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].alias, "owner-repo");
}
