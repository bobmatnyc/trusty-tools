//! Tests for `tm register` positional resolution and alias derivation (#4912).
//!
//! Why: four things must be pinned or they regress silently.
//! - `owner/repo` is the primary form and must resolve to the GitHub URL with
//!   the canonical `owner-repo` alias. Before the review it registered the
//!   literal string as a URL whose host was `owner` — unclonable, alias
//!   `trusty-tools`, exit 0, and no collision with the later correct entry.
//! - The legacy `<alias> <url>` order must still resolve correctly.
//! - The derived alias must be HYPHEN-joined `owner-repo` for every URL shape.
//! - The shapes that should NOT resolve — relative paths, host-only URLs,
//!   browser pastes — must still be refused rather than swept into shorthand.
//!
//! What: unit tests over [`super::classify`], [`super::resolve_register_args`],
//! and the boundary path splitter, plus end-to-end tests through
//! `standalone::register_cmd` proving a refusal never touches `registry.json`.
//! Test: this file.

use super::{Positional, classify, looks_like_repo, path_segments, resolve_register_args};

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

/// Every shape lands in exactly one case. The order of the arms is load-bearing:
/// `./repo` must be caught as a path BEFORE the host-shape test, which it would
/// otherwise pass on the `.` in `.`.
#[test]
fn classify_sorts_every_shape() {
    // Full URLs.
    for s in [
        "https://github.com/owner/repo",
        "git@github.com:owner/repo.git",
        "ssh://git@git.example.com:2222/team/thing.git",
        "github.com/owner/repo",
        "localhost:3000/owner/repo",
        "/srv/git/repo.git",
        "~/src/repo.git",
    ] {
        assert!(matches!(classify(s), Positional::Url(_)), "{s} not a URL");
    }

    // GitHub shorthand — the primary form.
    assert_eq!(
        classify("bobmatnyc/trusty-tools"),
        Positional::Shorthand {
            owner: "bobmatnyc",
            repo: "trusty-tools"
        }
    );
    assert_eq!(
        classify("123/repo"),
        Positional::Shorthand {
            owner: "123",
            repo: "repo"
        }
    );
    // A repo name may carry a dot; an owner may not, which is what keeps
    // `github.com/o/r` on the URL side without a host list.
    assert_eq!(
        classify("owner/repo.js"),
        Positional::Shorthand {
            owner: "owner",
            repo: "repo.js"
        }
    );

    // Relative paths — never shorthand.
    for s in ["./repo", "../repo", ".", ".."] {
        assert_eq!(classify(s), Positional::RelativePath, "{s} misclassified");
    }

    // Neither.
    for s in ["my-alias", "owner/", "a/b/c", "owner/re po", "owner:repo"] {
        assert_eq!(classify(s), Positional::Other, "{s} misclassified");
    }
}

/// Aliases are `^[a-z0-9][a-z0-9._-]*$` — none can be read as a repo, so the
/// two-positional routing can never mistake one for the other.
#[test]
fn looks_like_repo_rejects_aliases() {
    for alias in ["my-alias", "proj", "a.b-c", "repo123", "owner-repo"] {
        assert!(!looks_like_repo(alias), "{alias} misread as a repo");
    }
}

// ---------------------------------------------------------------------------
// GitHub shorthand — the primary form
// ---------------------------------------------------------------------------

/// THE #4912 PRIMARY FORM. `owner/repo` resolves to the GitHub URL and derives
/// the canonical `owner-repo` alias. GitHub is assumed, matching the
/// `is_github_remote` gate `tm launch` already applies.
#[test]
fn shorthand_resolves_to_github() {
    let (alias, url) = resolve_register_args("bobmatnyc/trusty-tools", None).unwrap();
    assert_eq!(url, "https://github.com/bobmatnyc/trusty-tools");
    assert_eq!(alias, "bobmatnyc-trusty-tools");

    // The alias must match what the full URL derives — one derivation path.
    let (from_url, _) =
        resolve_register_args("https://github.com/bobmatnyc/trusty-tools", None).unwrap();
    assert_eq!(alias, from_url);
}

/// An all-numeric owner is valid shorthand, and the port-drop in the path
/// splitter must not eat it.
#[test]
fn shorthand_numeric_owner() {
    let (alias, url) = resolve_register_args("123/repo", None).unwrap();
    assert_eq!(url, "https://github.com/123/repo");
    assert_eq!(alias, "123-repo");
}

/// Shorthand with an explicit alias, in both positional orders.
#[test]
fn shorthand_with_explicit_alias() {
    let (alias, url) = resolve_register_args("owner/repo", Some("shiny")).unwrap();
    assert_eq!(alias, "shiny");
    assert_eq!(url, "https://github.com/owner/repo");

    let (alias, url) = resolve_register_args("shiny", Some("owner/repo")).unwrap();
    assert_eq!(alias, "shiny");
    assert_eq!(url, "https://github.com/owner/repo");
}

/// Relative paths are paths, not shorthand — they resolve against the process
/// cwd but would be cloned from elsewhere. Each shape refused explicitly.
#[test]
fn relative_paths_are_never_shorthand() {
    for s in ["./repo", "../repo", "./owner/repo", "../a/b"] {
        let err = resolve_register_args(s, None).unwrap_err().to_string();
        assert!(
            err.contains("relative path"),
            "{s} must be refused as a path, got: {err}"
        );
    }
}

/// Absolute and home-relative paths stay accepted as local clone sources — they
/// worked before #4912 and are unambiguous, unlike the relative forms.
#[test]
fn absolute_and_home_paths_are_accepted() {
    assert!(resolve_register_args("/srv/git/repo.git", Some("local")).is_ok());
    assert!(resolve_register_args("~/src/repo.git", Some("home")).is_ok());
}

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

/// A lone argument naming nothing is a usage error, not a half-registration.
/// The message must show both accepted forms.
#[test]
fn one_arg_non_repo_errors() {
    for s in ["my-alias", "owner/", "a/b/c"] {
        let err = resolve_register_args(s, None).unwrap_err().to_string();
        assert!(
            err.contains("does not name a repository"),
            "{s}: unexpected {err}"
        );
        assert!(err.contains("<owner>/<repo>"), "no remedy in: {err}");
    }
}

/// Two repo-shaped arguments are ambiguous — refuse rather than guess.
#[test]
fn two_urls_error() {
    for (a, b) in [
        ("https://github.com/a/b", "https://github.com/c/d"),
        ("owner/repo", "other/repo"),
    ] {
        let err = resolve_register_args(a, Some(b)).unwrap_err().to_string();
        assert!(
            err.contains("both arguments name a repository"),
            "({a}, {b}): unexpected {err}"
        );
    }
}

/// Two alias-shaped arguments mean no repo was supplied at all.
#[test]
fn two_non_urls_error() {
    let err = resolve_register_args("alpha", Some("beta"))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("neither argument names a repository"),
        "unexpected: {err}"
    );
}

// ---------------------------------------------------------------------------
// Alias derivation from full URLs
// ---------------------------------------------------------------------------

/// Every real URL shape derives the same HYPHEN-joined `owner-repo`. Full
/// non-GitHub URLs keep working — only non-GitHub *shorthand* is deferred.
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
        ("git@bitbucket.org:team/thing.git", "team-thing"),
    ];
    for (url, want) in cases {
        let (alias, got_url) = resolve_register_args(url, None).unwrap();
        assert_eq!(alias, want, "wrong alias for {url}");
        assert_eq!(got_url, url, "URL must be stored verbatim");
    }
}

/// No discernible owner → fall back to the repo segment alone (erroring would
/// reject valid self-hosted single-segment paths).
#[test]
fn no_owner_falls_back_to_repo() {
    let (alias, _) = resolve_register_args("https://example.com/repo", None).unwrap();
    assert_eq!(alias, "repo");
}

/// A host-only URL names nothing. BOTH forms must error — the no-trailing-slash
/// one silently derived `examplecom` before the review.
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
            "{url} must be refused, got: {err}"
        );
    }
}

/// Browser pastes point INTO a repo and derive nonsense from the last two path
/// segments (`tree-main`, `pull-4914`). Refuse with the repo-root form named.
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
            "{url} must be refused, got: {err}"
        );
        assert!(err.contains("repository root"), "no remedy in: {err}");
    }
}

/// The web-path words are only refused in an interior position — a repo actually
/// NAMED `tree` (or a GitLab subgroup path ending in one) still works.
#[test]
fn repo_named_like_a_web_path_is_ok() {
    let (alias, _) = resolve_register_args("https://github.com/owner/tree", None).unwrap();
    assert_eq!(alias, "owner-tree");
    let (alias, _) = resolve_register_args("https://gitlab.com/group/sub/blob", None).unwrap();
    assert_eq!(alias, "sub-blob");
}

/// A query string or fragment is never part of a clone URL, and both corrupt the
/// derived alias. Strip them rather than storing an unclonable string.
#[test]
fn query_and_fragment_are_stripped() {
    for input in [
        "https://github.com/owner/repo?tab=readme-ov-file",
        "https://github.com/owner/repo#readme",
    ] {
        let (alias, url) = resolve_register_args(input, None).unwrap();
        assert_eq!(alias, "owner-repo", "for {input}");
        assert_eq!(url, "https://github.com/owner/repo", "for {input}");
    }
}

/// The `a/b-c` vs `a-b/c` flattening ambiguity: both derive the SAME alias.
/// Accepted, not special-cased — the second registration is caught by the
/// ordinary different-URL collision refusal.
#[test]
fn hyphen_flattening_ambiguity_is_accepted_and_collides() {
    let (a, _) = resolve_register_args("a/b-c", None).unwrap();
    let (b, _) = resolve_register_args("a-b/c", None).unwrap();
    assert_eq!(a, "a-b-c");
    assert_eq!(b, "a-b-c");
}

// ---------------------------------------------------------------------------
// Boundary path splitter
// ---------------------------------------------------------------------------

/// The local validator must agree with the shared deriver on where the host
/// ends, for every shape both see — otherwise a refusal fires on a URL the
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

/// The port-drop must key off the `:` host terminator, not "the first segment is
/// digits" — otherwise an all-numeric owner is swallowed. That matters more now
/// that `123/repo` is valid shorthand.
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
    crate::commands::standalone::register_cmd(&paths, "a/b-c", None, false).unwrap();
    let registry_path = dir.path().join("registry.json");
    let before = std::fs::read(&registry_path).unwrap();

    // `a-b/c` flattens to the same alias but is a different URL.
    let err = crate::commands::standalone::register_cmd(&paths, "a-b/c", None, false).unwrap_err();
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

/// A refused argument must not create a registry file at all — the refusal
/// happens before any load or save.
#[test]
fn rejected_url_writes_nothing() {
    let dir = tempfile::TempDir::new().unwrap();
    let paths = crate::commands::managed_root::ManagedPaths::from_root(dir.path().to_path_buf());
    for bad in ["./repo", "https://example.com", "a/b/c"] {
        assert!(
            crate::commands::standalone::register_cmd(&paths, bad, None, false).is_err(),
            "{bad} must be refused"
        );
    }
    assert!(
        !dir.path().join("registry.json").exists(),
        "a refused argument must not write a registry"
    );
}

/// Shorthand registers end to end and stores the resolved GitHub URL.
#[test]
fn shorthand_registers_end_to_end() {
    let dir = tempfile::TempDir::new().unwrap();
    let paths = crate::commands::managed_root::ManagedPaths::from_root(dir.path().to_path_buf());
    crate::commands::standalone::register_cmd(&paths, "bobmatnyc/trusty-tools", None, false)
        .unwrap();

    let registry =
        trusty_mpm::core::standalone::registry::ManagedRegistry::load(dir.path()).unwrap();
    let entries = registry.list();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].alias, "bobmatnyc-trusty-tools");
    assert_eq!(entries[0].url, "https://github.com/bobmatnyc/trusty-tools");
}

/// Re-registering the SAME repo is idempotent, and the shorthand and full-URL
/// forms are the same registration — not two.
#[test]
fn same_url_reregistration_is_idempotent() {
    let dir = tempfile::TempDir::new().unwrap();
    let paths = crate::commands::managed_root::ManagedPaths::from_root(dir.path().to_path_buf());
    crate::commands::standalone::register_cmd(&paths, "owner/repo", None, false).unwrap();
    crate::commands::standalone::register_cmd(&paths, "https://github.com/owner/repo", None, false)
        .unwrap();

    let registry =
        trusty_mpm::core::standalone::registry::ManagedRegistry::load(dir.path()).unwrap();
    let entries = registry.list();
    assert_eq!(entries.len(), 1, "shorthand and full URL are one repo");
    assert_eq!(entries[0].alias, "owner-repo");
}
