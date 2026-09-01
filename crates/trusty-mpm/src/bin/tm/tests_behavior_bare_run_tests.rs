//! Parse-layer coverage for the bare `tm <github-url>` form (#6441).
//!
//! Why: the feature is one clap variant — `Command::External` — plus one gate,
//! and BOTH halves fail silently if they are wrong. A missing variant means
//! `tm https://github.com/o/r` exits 2 with a "did you mean?" hint; a gate that
//! is too wide means `tm statuss` becomes a registry-alias lookup reporting
//! "alias 'statuss' not found" instead of the usage error it used to get. The
//! interaction between `infer_subcommands` and `external_subcommand` is
//! untested in clap's own docs, so the precedence is pinned here rather than
//! assumed.
//! What: the two accepted repo shapes, the two regressions (prefix inference
//! and subcommand typos), and the descriptive error a repo-shaped non-repo URL
//! must surface.
//! Test: this file IS the test module.

use clap::{CommandFactory as _, Parser as _};

use crate::cli::{Cli, Command};
use crate::commands::run_target::{RunTarget, classify_bare};

/// Pull the `External` token list out of a parsed `Cli`, or fail loudly.
///
/// Why: every test here asserts on the same two-step shape — the parse reached
/// `External`, and its first token classified a particular way — so the
/// unwrapping is factored out rather than repeated five times.
fn external_token(argv: &[&str]) -> String {
    let cli = Cli::try_parse_from(argv).expect("bare token must parse");
    match cli.command.expect("a token is not the no-subcommand case") {
        Command::External(tokens) => tokens.first().cloned().expect("one token"),
        other => panic!("{argv:?} must reach the External arm, got {other:?}"),
    }
}

/// ACCEPTANCE: `tm https://github.com/<owner>/<repo>` classifies to the repo.
///
/// Why: the owner's acceptance-test invocation, and the headline of #6441.
/// Without the `External` variant this parse fails outright with clap's
/// unknown-subcommand error.
/// Test: itself.
#[test]
fn cli_parses_bare_github_url() {
    let token = external_token(&["tm", "https://github.com/bobmatnyc/mcp-a-protocol"]);
    let target = classify_bare(&token)
        .expect("a github URL is a repo, not a typo")
        .expect("it resolves");
    assert_eq!(
        target,
        RunTarget::Repo {
            owner: "bobmatnyc".into(),
            repo: "mcp-a-protocol".into(),
            clone_url: "https://github.com/bobmatnyc/mcp-a-protocol".into(),
        }
    );
}

/// ACCEPTANCE: the `owner/repo` shorthand takes the same path.
///
/// Why: #4912 made `owner/repo` the primary way to name a repo; the bare form
/// must not be URL-only, and both spellings must resolve to the SAME clone URL
/// or `tm <url>` and `tm <owner>/<repo>` would provision two checkouts.
/// Test: itself.
#[test]
fn cli_parses_bare_owner_repo() {
    let token = external_token(&["tm", "bobmatnyc/mcp-a-protocol"]);
    let target = classify_bare(&token)
        .expect("shorthand is a repo")
        .expect("it resolves");
    assert_eq!(
        target,
        RunTarget::Repo {
            owner: "bobmatnyc".into(),
            repo: "mcp-a-protocol".into(),
            clone_url: "https://github.com/bobmatnyc/mcp-a-protocol".into(),
        }
    );
}

/// REGRESSION: an UNAMBIGUOUS prefix still infers; it does NOT go external.
///
/// Why: `infer_subcommands` (#4398) and `external_subcommand` both want an
/// unrecognized leading token, and clap documents no precedence between them.
/// Measured here: inference wins whenever it resolves to exactly one
/// subcommand, so every abbreviation users type today keeps working. Only a
/// token inference CANNOT resolve reaches `External`.
/// Test: itself.
#[test]
fn cli_bare_infers_abbreviated_subcommand_over_external() {
    for (argv, label) in [
        (["tm", "doc"], "doctor"),
        (["tm", "heal"], "health"),
        (["tm", "rest"], "restart"),
        (["tm", "instal"], "install"),
    ] {
        let cli = Cli::try_parse_from(argv).expect("abbreviation must parse");
        let command = cli.command.expect("an abbreviation names a subcommand");
        assert!(
            !matches!(command, Command::External(_)),
            "'{}' must infer {label}, not fall through to External — got {command:?}",
            argv[1]
        );
    }

    // The exact resolution, not merely "not External".
    assert!(matches!(
        Cli::try_parse_from(["tm", "heal"])
            .expect("parses")
            .command
            .expect("has a command"),
        Command::Health
    ));
}

/// REGRESSION: an AMBIGUOUS prefix is not a repo, so it keeps its usage error.
///
/// Why: `sta` prefixes `start`, `status`, AND `statusline`, so inference
/// cannot resolve it and it lands in `External` — where, before this gate, it
/// would have become a registry-alias lookup. On `origin/main` the same
/// invocation exits 2 with clap's "tip: some similar subcommands exist: …
/// 'status', 'start'" line, and `classify_bare` returning `None` is what
/// routes it back to exactly that error.
/// Test: itself.
#[test]
fn cli_bare_ambiguous_prefix_is_not_a_repo() {
    let token = external_token(&["tm", "sta"]);
    assert_eq!(token, "sta");
    assert!(
        classify_bare(&token).is_none(),
        "an ambiguous prefix must reach the usage-error path, not a managed run"
    );
}

/// REGRESSION: a subcommand typo is NOT a repo, so it keeps the usage error.
///
/// Why: this is the gate `classify_bare` exists for. `statuss` reaches the
/// `External` arm (no subcommand starts with it), and if `classify_bare`
/// accepted it the dispatcher would try a registry-alias lookup and report
/// "alias 'statuss' not found" — naming the wrong problem for what is plainly
/// a typo. `None` is what routes it back to clap's usage error plus the
/// workspace "did you mean?" hint.
/// Test: itself.
#[test]
fn cli_bare_unknown_subcommand_is_not_a_repo() {
    for typo in ["statuss", "sessionz", "notacommand"] {
        let token = external_token(&["tm", typo]);
        assert_eq!(token, typo);
        assert!(
            classify_bare(&token).is_none(),
            "'{typo}' has no '/' or ':', so it must fall back to the usage error"
        );
    }
}

/// REGRESSION: the retired `tm coordinator-tui` stays refused (#1392).
///
/// Why: `external_subcommand` makes clap ACCEPT this token at the parse layer,
/// so the rejection `cli_rejects_removed_coordinator_tui` used to assert there
/// moves here. The guarantee is unchanged — the invocation still exits with a
/// usage error — but it is now enforced by the `classify_bare` gate rather than
/// by the parse failing.
/// Test: itself.
#[test]
fn cli_bare_retired_subcommand_is_not_a_repo() {
    let token = external_token(&["tm", "coordinator-tui"]);
    assert_eq!(token, "coordinator-tui");
    assert!(
        classify_bare(&token).is_none(),
        "a retired subcommand must never become a managed run"
    );
}

/// A repo-SHAPED token that names no repo surfaces `resolved_url`'s message.
///
/// Why: `https://example.com/` passes `looks_like_repo` (it is a URL) but has
/// no owner/repo path, so it is not a typo and a usage error would be useless.
/// The remedy belongs in the message `tm register` already writes, inherited
/// here rather than restated.
/// Test: itself.
#[test]
fn cli_bare_non_repo_url_surfaces_the_descriptive_error() {
    for (bad, expected) in [
        ("https://example.com/", "names a host"),
        (
            "https://github.com/bobmatnyc/trusty-tools/pull/6441",
            "points inside a repository",
        ),
    ] {
        let token = external_token(&["tm", bad]);
        let err = classify_bare(&token)
            .expect("a URL is repo-shaped, so it is never the typo path")
            .expect_err("but it does not name a repository");
        assert!(
            err.to_string().contains(expected),
            "rejection of '{bad}' must say '{expected}', got: {err}"
        );
    }
}

/// The rebuilt command reproduces the usage error `origin/main` printed.
///
/// Why: this is the load-bearing half of the fallback, and the obvious
/// alternative silently does nothing. Calling
/// `Cli::command().allow_external_subcommands(false)` reads the flag back as
/// `false` and STILL matches `sta` as the external subcommand `sta` — the
/// derive wires the match path during augmentation, so the flag arrives too
/// late. Measured, not assumed. Only a fresh `clap::Command` refuses it, which
/// is why `run_target::without_external_catch_all` rebuilds rather than flips a
/// flag; this test fails if anyone simplifies it back.
///
/// The expected text is `origin/main`'s own output for `tm sta`, captured
/// before this change:
///
/// ```text
/// error: unrecognized subcommand 'sta'
///
///   tip: some similar subcommands exist: 'stop', 'meta', 'statusline', …
///
/// Usage: tm [OPTIONS] [COMMAND]
/// ```
/// Test: itself.
#[test]
fn reject_unknown_subcommand_rebuild_reproduces_claps_error() {
    let argv = ["tm".to_string(), "sta".to_string()];

    // The mutation that looks equivalent and is not.
    let flag_flip = Cli::command().allow_external_subcommands(false);
    assert!(
        flag_flip.try_get_matches_from(argv.clone()).is_ok(),
        "if this now ERRORS, clap has changed and the rebuild may be simplifiable"
    );

    let err = crate::commands::run_target::without_external_catch_all()
        .try_get_matches_from(argv)
        .expect_err("an ambiguous prefix must not parse");
    assert_eq!(err.kind(), clap::error::ErrorKind::InvalidSubcommand);

    let rendered = err.to_string();
    for fragment in [
        "unrecognized subcommand 'sta'",
        "tip: some similar subcommands exist",
        "'status'",
        "'start'",
        "Usage: tm [OPTIONS] [COMMAND]",
    ] {
        assert!(
            rendered.contains(fragment),
            "the rebuilt error must still say '{fragment}':\n{rendered}"
        );
    }
}

/// The hardcoded rebuild name still matches the derive's own.
///
/// Why: `clap::builder::Str` accepts only a `&'static str`, so
/// `without_external_catch_all` cannot read the name off the source command at
/// runtime and spells it out instead. This is the guard against that copy
/// drifting from `#[command(name = ...)]`.
/// Test: itself.
#[test]
fn command_name_matches_the_rebuild() {
    assert_eq!(
        Cli::command().get_name(),
        crate::commands::run_target::CLI_NAME
    );
}
