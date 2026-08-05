//! Tests for `tm register` positional resolution and alias derivation (#4912).
//!
//! Why: the #4912 change moves the URL to the first positional and makes the
//! alias optional. Two things must be pinned by tests or they regress silently:
//! the legacy `<alias> <url>` order still resolving correctly (a wrong answer
//! there is a silent misregistration, not an error), and the derived alias being
//! HYPHEN-joined `owner-repo` for every real URL shape.
//! What: unit tests over [`super::resolve_register_args`] plus one end-to-end
//! test through `standalone::register_cmd` proving a collision with a different
//! URL refuses and leaves `registry.json` byte-identical.
//! Test: this file.

use super::{looks_like_url, resolve_register_args};

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
    assert!(
        err.contains("does not look like a repository URL"),
        "unexpected error: {err}"
    );
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

/// Two alias-shaped arguments mean no URL was supplied at all.
#[test]
fn two_non_urls_error() {
    let err = resolve_register_args("alpha", Some("beta"))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("neither argument looks like a repository URL"),
        "unexpected: {err}"
    );
}

/// The shape test must never classify a valid alias as a URL, and vice versa.
#[test]
fn looks_like_url_accepts_url_shapes() {
    assert!(looks_like_url("https://github.com/owner/repo"));
    assert!(looks_like_url("git@github.com:owner/repo.git"));
    assert!(looks_like_url("github.com/owner/repo"));
    assert!(looks_like_url("ssh://git@example.com:2222/owner/repo.git"));
}

/// Aliases are `^[a-z0-9][a-z0-9._-]*$` — none of them can look like a URL.
#[test]
fn looks_like_url_rejects_aliases() {
    for alias in ["my-alias", "proj", "a.b-c", "repo123", "owner-repo"] {
        assert!(!looks_like_url(alias), "{alias} misread as a URL");
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
/// #4912 decision; erroring here would reject valid self-hosted single-segment
/// paths).
#[test]
fn no_owner_falls_back_to_repo() {
    let (alias, _) = resolve_register_args("https://example.com/repo", None).unwrap();
    assert_eq!(alias, "repo");
}

/// A host-only URL yields nothing to name — error loudly with the explicit-alias
/// escape hatch in the message.
#[test]
fn host_only_url_errors() {
    let err = resolve_register_args("https://example.com/", None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("cannot derive an alias"), "unexpected: {err}");
    assert!(
        err.contains("<alias>"),
        "message must show the escape hatch: {err}"
    );
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
