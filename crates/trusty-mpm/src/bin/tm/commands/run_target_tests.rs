//! Unit tests for `tm run` target classification (#4990).
//!
//! Why: classification decides which subsystem a `tm run` invocation reaches,
//! and it must happen before any network call — a bad `owner/repo` string has
//! to be rejected with a usable message, not become a `git clone` that fails
//! minutes later. It must also agree with `tm register`, or the same string
//! would name two different things depending on which command saw it.
//! What: the alias/repo split, shorthand and full-URL resolution, the
//! rejections inherited from `tm register`, and the agreement between the two
//! identity primitives.
//! Test: this file IS the test module.

use super::*;

/// The routing decision itself: an alias goes to the standalone driver, a
/// repo shape goes to the daemon-managed cold start.
///
/// Why: the two arms are disjoint by construction — a DOC-24 alias matches
/// `^[a-z0-9][a-z0-9._-]*$`, so it can never contain the `/` a repo form
/// requires. This pins that disjointness rather than trusting it.
/// Test: itself.
#[test]
fn classify_sorts_alias_from_repo() {
    for alias in ["my-project", "trusty-tools", "a", "x.y_z-1"] {
        assert_eq!(
            classify_run_target(alias).expect("alias classifies"),
            RunTarget::Alias(alias.to_string()),
            "{alias} must route to the standalone driver"
        );
    }

    let target = classify_run_target("bobmatnyc/trusty-tools").expect("shorthand classifies");
    assert!(
        matches!(target, RunTarget::Repo { .. }),
        "owner/repo must route to the managed cold start, got {target:?}"
    );
}

/// `owner/repo` resolves to the GitHub URL `tm register` would store.
///
/// Why: the brief's headline case, and the exact string this later hands to
/// `git clone`.
/// Test: itself.
#[test]
fn classify_resolves_shorthand() {
    let target = classify_run_target("bobmatnyc/trusty-tools").expect("classifies");
    assert_eq!(
        target,
        RunTarget::Repo {
            owner: "bobmatnyc".into(),
            repo: "trusty-tools".into(),
            clone_url: "https://github.com/bobmatnyc/trusty-tools".into(),
        }
    );
}

/// A full URL — SSH or HTTPS, any host — resolves to the same identity shape.
///
/// Why: `parse_owner_repo` refuses URL-shaped input by design, so this is the
/// `parse_github_path` fallback arm. Without it a full URL would be rejected.
/// Test: itself.
#[test]
fn classify_resolves_full_urls() {
    let ssh = classify_run_target("git@github.com:bobmatnyc/trusty-tools.git").expect("ssh");
    assert_eq!(
        ssh,
        RunTarget::Repo {
            owner: "bobmatnyc".into(),
            repo: "trusty-tools".into(),
            clone_url: "git@github.com:bobmatnyc/trusty-tools.git".into(),
        }
    );

    let https = classify_run_target("https://gitlab.com/acme/widget.git").expect("https");
    assert_eq!(
        https,
        RunTarget::Repo {
            owner: "acme".into(),
            repo: "widget".into(),
            clone_url: "https://gitlab.com/acme/widget.git".into(),
        }
    );
}

/// An absolute local path is a clone source, and its identity comes from the
/// last TWO path segments.
///
/// Why: `classify` sorts a leading `/` as a `Url`, so `tm run
/// /Users/me/code/app` is accepted and clones from that local repo. The
/// identity is then `code/app` — `owner` is the PARENT DIRECTORY name, not a
/// GitHub owner — so the managed checkout lands at
/// `<repos_root>/code/app`. That is deterministic and harmless, but it is
/// surprising enough to pin: a future change to the fallback order would
/// silently relocate an existing operator's checkout.
/// Test: itself.
#[test]
fn absolute_path_derives_identity_from_the_last_two_segments() {
    let target = classify_run_target("/Users/me/code/app").expect("absolute path classifies");
    assert_eq!(
        target,
        RunTarget::Repo {
            owner: "code".into(),
            repo: "app".into(),
            clone_url: "/Users/me/code/app".into(),
        }
    );
}

/// AGREEMENT: for shorthand, the identity primitive this module uses matches
/// the one derived from the resolved URL.
///
/// Why: `classify_run_target` prefers `parse_owner_repo` and falls back to
/// `parse_github_path`. Two derivations is one more than the number that can
/// silently drift, so the property that they agree is pinned here rather than
/// assumed. If it ever fails, `tm run owner/repo` and `tm register owner/repo`
/// would resolve to different directories.
/// Test: itself.
#[test]
fn shorthand_identity_agrees_with_url_identity() {
    for spec in [
        "bobmatnyc/trusty-tools",
        "Acme/Cool_App",
        "123/repo",
        "a-b/c.d",
    ] {
        let url = crate::commands::register_args::resolved_url(spec).expect("resolves");
        let from_shorthand = parse_owner_repo(spec).expect("shorthand parses");
        let from_url = parse_github_path(&url).expect("url parses");
        assert_eq!(
            from_shorthand, from_url,
            "identity for '{spec}' must not depend on which parser saw it"
        );
    }
}

/// INVALID INPUT IS REJECTED BEFORE ANY NETWORK CALL.
///
/// Why: `classify_run_target` is pure — it runs no git, opens no socket. These
/// strings therefore fail with a message while a user is still watching, which
/// is the whole reason the rejection lives at the CLI boundary and not at
/// clone time. The specific shapes are inherited from `tm register`: a browser
/// paste into a repo's web UI, a relative path, and a host with no repo.
/// Test: itself.
#[test]
fn classify_rejects_browser_pastes_and_paths() {
    for (bad, expected) in [
        (
            "https://github.com/bobmatnyc/trusty-tools/pull/4990",
            "points inside a repository",
        ),
        (
            "https://github.com/bobmatnyc/trusty-tools/tree/main",
            "points inside a repository",
        ),
        // A relative path is neither a repo nor an alias. Routing it to the
        // registry would report "alias './some/dir' not found", which names
        // the wrong problem — the regression this case pins.
        ("./some/dir", "is a relative path"),
        ("../sibling", "is a relative path"),
        ("https://github.com", "names a host"),
    ] {
        let err = match classify_run_target(bad) {
            Err(e) => e.to_string(),
            Ok(t) => panic!("'{bad}' must be refused before any network call, got {t:?}"),
        };
        assert!(
            err.contains(expected),
            "rejection of '{bad}' must say '{expected}', got: {err}"
        );
    }
}

/// #6441: every repo shape `tm run` accepts is also a bare-`tm` target.
///
/// Why: `tm <url>` and `tm run <url>` must not disagree about what a string
/// means, so `classify_bare` delegates to `classify_run_target` rather than
/// re-deriving. This pins the delegation on the shapes both see.
/// Test: itself.
#[test]
fn classify_bare_accepts_repo_shapes() {
    for spec in [
        "bobmatnyc/mcp-a-protocol",
        "https://github.com/bobmatnyc/mcp-a-protocol",
        "https://github.com/bobmatnyc/mcp-a-protocol.git",
        "git@github.com:bobmatnyc/mcp-a-protocol.git",
    ] {
        let bare = classify_bare(spec)
            .unwrap_or_else(|| panic!("'{spec}' is a repo shape, not a typo"))
            .unwrap_or_else(|e| panic!("'{spec}' must resolve: {e}"));
        assert_eq!(
            bare,
            classify_run_target(spec).expect("tm run accepts it too"),
            "'{spec}' must mean the same thing bare as it does after `tm run`"
        );
        assert!(matches!(bare, RunTarget::Repo { .. }));
    }
}

/// THE GATE: anything that is not repo-shaped declines, and declines QUIETLY.
///
/// Why: `Command::External` catches subcommand typos as well as URLs. `None`
/// is what hands a typo back to clap's usage error; a `Some(Err(..))` here
/// would replace "did you mean status?" with a repository complaint, and a
/// `Some(Ok(Alias(..)))` would turn it into a registry lookup for an alias
/// nobody registered. A relative path declines for the same reason — clap's
/// usage error names the real problem better than a clone attempt would.
/// Test: itself.
#[test]
fn classify_bare_declines_subcommand_typos() {
    for not_a_repo in [
        "statuss",
        "sessionz",
        "instal",
        "notacommand",
        "",
        "   ",
        "./some/dir",
        "../sibling",
    ] {
        assert!(
            classify_bare(not_a_repo).is_none(),
            "'{not_a_repo}' must fall through to the usage-error path, not become a target"
        );
    }
}

/// A repo-shaped token that names no repo keeps `resolved_url`'s message.
///
/// Why: `https://example.com/` is a URL, so it is not a typo and the usage
/// error would be the wrong answer. The remedy is the one `tm register`
/// already writes, inherited rather than restated.
/// Test: itself.
#[test]
fn classify_bare_surfaces_resolved_url_errors() {
    for (bad, expected) in [
        ("https://example.com/", "names a host"),
        (
            "https://github.com/bobmatnyc/trusty-tools/issues",
            "points inside a repository",
        ),
    ] {
        let err = classify_bare(bad)
            .unwrap_or_else(|| panic!("'{bad}' is repo-SHAPED, so it is never the typo path"))
            .expect_err("but it does not name a repository");
        assert!(
            err.to_string().contains(expected),
            "rejection of '{bad}' must say '{expected}', got: {err}"
        );
    }
}

/// An empty or whitespace-only target names nothing and must say so.
///
/// Test: itself.
#[test]
fn classify_rejects_empty_target() {
    for empty in ["", "   ", "\t"] {
        let err = classify_run_target(empty).expect_err("empty must be refused");
        assert!(
            err.to_string().contains("needs a target"),
            "message must name the remedy: {err}"
        );
    }
}
