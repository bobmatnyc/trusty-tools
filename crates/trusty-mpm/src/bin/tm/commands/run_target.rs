//! `tm run <target>` routing: DOC-24 alias vs. daemon-managed cold start (#4990).
//!
//! Why: `tm run` used to accept exactly one thing — a DOC-24 registry alias —
//! and launch a blocking foreground `claude` for it. Starting work on a repo
//! that is not already registered and not already on disk had no entry point
//! at all: the daemon-managed system (ADR-0030's tm checkout at
//! `~/trusty-mpm-projects/<owner>/<repo>`, a real `SessionRecord`, a tmux pane)
//! can only be entered from a checkout that already exists, because
//! `try_inproject_spawn` gates on `.git` and derives the GitHub identity from
//! `remote.origin.url`. `tm run <owner>/<repo>` is the cold start: clone or
//! verify the managed checkout, then hand off to the daemon-managed launch.
//!
//! What: [`classify_run_target`] sorts the positional using the SAME predicate
//! `tm register` uses ([`super::register_args::looks_like_repo`]) so the two
//! commands cannot disagree about what a string means; [`run`] dispatches an
//! alias to the unchanged standalone driver and a repo to [`run_managed`],
//! which drives [`trusty_mpm::daemon::managed_routes::inproject_cold_start`]
//! and then the existing [`super::launch::launch`] path.
//! Test: `run_target_tests.rs`.

use trusty_common::github_path::{parse_github_path, parse_owner_repo};

/// What a `tm run` positional names.
///
/// Why: the positional now carries two disjoint meanings, and the disjointness
/// is real rather than assumed — a DOC-24 alias matches
/// `^[a-z0-9][a-z0-9._-]*$` (`core::standalone::registry::validate_alias`), so
/// it can never contain the `/` or `:` every repo form requires.
/// What: `Alias` is the pre-existing standalone target; `Repo` carries the
/// resolved GitHub identity and the URL to clone.
/// Test: `classify_sorts_alias_from_repo`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RunTarget {
    /// A DOC-24 registry alias — the unchanged standalone `tm run <alias>`.
    Alias(String),
    /// A GitHub repo to cold-start under daemon management.
    Repo {
        /// Slugified repository owner.
        owner: String,
        /// Slugified repository name.
        repo: String,
        /// Fully-resolved clone URL.
        clone_url: String,
    },
}

/// Sort a `tm run` positional into the case that decides its outcome.
///
/// Why: routing has to happen before any I/O, so an unparseable repo string is
/// rejected with a usable message rather than becoming a `git clone` that
/// fails minutes later — or, worse, a registry lookup for an alias that was
/// never meant to be one.
/// What: delegates the repo-or-not question to
/// [`super::register_args::looks_like_repo`] and the URL resolution to
/// [`super::register_args::resolved_url`], so `tm run` inherits every rejection
/// `tm register` makes (browser pastes into a repo's web UI, relative paths,
/// host-only URLs). Identity comes from
/// [`trusty_common::github_path::parse_owner_repo`] for the `owner/repo`
/// shorthand — the shared, tested primitive for a canonical identity path —
/// falling back to [`trusty_common::github_path::parse_github_path`] for a full
/// URL, which `parse_owner_repo` refuses by design.
/// Test: `classify_sorts_alias_from_repo`, `classify_resolves_shorthand`,
/// `classify_resolves_full_urls`, `classify_rejects_browser_pastes_and_paths`,
/// `shorthand_identity_agrees_with_url_identity`.
pub(crate) fn classify_run_target(spec: &str) -> anyhow::Result<RunTarget> {
    let spec = spec.trim();
    if spec.is_empty() {
        anyhow::bail!(
            "tm run needs a target. Pass <owner>/<repo> to start a managed session for a \
             GitHub repo, or a registered alias (see `tm ls --projects`)."
        );
    }

    // A relative path is neither a repo nor an alias, and `looks_like_repo`
    // answers `false` for it — so without this arm `tm run ./some/dir` would
    // become a registry lookup and report "alias not found" instead of the
    // path advice. An alias matches `^[a-z0-9][a-z0-9._-]*$` and can never
    // start with a dot.
    if super::register_args::is_relative_path(spec) {
        return Err(super::register_args::rejection(spec));
    }

    if !super::register_args::looks_like_repo(spec) {
        return Ok(RunTarget::Alias(spec.to_string()));
    }

    let clone_url = super::register_args::resolved_url(spec)?;
    let gh = parse_owner_repo(spec)
        .or_else(|| parse_github_path(&clone_url))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "cannot derive an owner/repo identity from '{spec}'. \
                 Pass <owner>/<repo>, or a full repository URL."
            )
        })?;

    Ok(RunTarget::Repo {
        owner: gh.owner,
        repo: gh.repo,
        clone_url,
    })
}

/// `tm run` handler.
///
/// Why: one command, two subsystems. Keeping the branch here rather than in
/// `main.rs` keeps the dispatch table flat and gives the routing decision a
/// testable home.
/// What: warns about flags that do not apply, classifies the target, and
/// dispatches — alias to `core::standalone`'s driver (unchanged), repo to
/// [`run_managed`].
///
/// Two flags reach an arm that cannot use them, and BOTH warn rather than being
/// dropped silently. `--task` is unimplemented on either arm. `--root` selects
/// the DOC-24 managed root, which the daemon-managed arm never consults: its
/// checkout location comes from `repos_root()` (`TRUSTY_MPM_REPOS_ROOT` >
/// `TRUSTY_MPM_WORKSPACE_ROOT` > config > `~/trusty-mpm-projects`), a different
/// setting entirely. A user passing `--root` there is trying to relocate the
/// checkout and would otherwise get no hint that it had no effect.
/// Test: `classify_sorts_alias_from_repo` covers the routing decision; the
/// managed arm's checkout half is covered by
/// `daemon::managed_routes::inproject_cold_start` tests.
pub(crate) async fn run(
    client: &reqwest::Client,
    url: &str,
    target: &str,
    task: Option<String>,
    root: Option<String>,
) -> anyhow::Result<()> {
    // `--task` is not implemented on either arm. Per DOC-24 autonomous/task
    // dispatch is the session-manager layer's concern; warn rather than drop
    // the flag silently.
    if let Some(ref t) = task {
        eprintln!(
            "warning: --task '{t}' is not yet implemented and will be ignored. \
             Task dispatch is handled by the session-manager layer."
        );
    }

    match classify_run_target(target)? {
        RunTarget::Alias(alias) => {
            let paths = super::managed_root::resolve_managed_paths(root.as_deref())?;
            super::standalone::run_cmd(&paths, &alias)
        }
        RunTarget::Repo {
            owner,
            repo,
            clone_url,
        } => {
            if let Some(r) = &root {
                eprintln!(
                    "warning: --root '{r}' does not apply to a <owner>/<repo> target and will \
                     be ignored. It selects the standalone managed root; the managed checkout \
                     location comes from TRUSTY_MPM_REPOS_ROOT / TRUSTY_MPM_WORKSPACE_ROOT."
                );
            }
            run_managed(client, url, &owner, &repo, &clone_url).await
        }
    }
}

/// Decide whether a token clap could not match names a repo to run (#6441).
///
/// Why: `tm <github-url>` has to reach the same register→load→run chain
/// `tm run <url>` already drives, and clap surfaces such a token through
/// [`crate::cli::Command::External`] — the same arm EVERY token clap cannot
/// resolve lands in: a typo (`statuss`), an ambiguous prefix (`sta`, which
/// matches `start`/`status`/`statusline`), and a retired subcommand
/// (`coordinator-tui`). So the gate is the whole design: each of those must
/// keep getting clap's usage error and the "did you mean?" hint, never a
/// managed run and never an alias lookup reporting "alias 'statuss' not found".
/// Only a token [`super::register_args::looks_like_repo`] accepts — the SAME
/// predicate `tm register` and [`classify_run_target`] use — is a repo.
/// What: `None` means "not a repo, hand it back to the usage-error path".
/// `Some(Ok(..))` is always [`RunTarget::Repo`]; `Some(Err(..))` is a
/// repo-shaped token that [`super::register_args::resolved_url`] refuses (a
/// host with no path, a browser paste into a repo's web UI), and that
/// descriptive error is what the user should see rather than a usage dump.
///
/// A relative path is `None` on purpose: it is neither a repo nor a
/// subcommand, and clap's usage error names the real problem more usefully
/// than a clone attempt would.
/// Test: `classify_bare_accepts_repo_shapes`,
/// `classify_bare_declines_subcommand_typos`,
/// `classify_bare_surfaces_resolved_url_errors`.
pub(crate) fn classify_bare(token: &str) -> Option<anyhow::Result<RunTarget>> {
    let token = token.trim();
    if !super::register_args::looks_like_repo(token) {
        return None;
    }
    Some(classify_run_target(token))
}

/// `tm <token>` where `<token>` matched no subcommand (#6441).
///
/// Why: keeping the fallback here rather than in `main.rs` keeps that file's
/// dispatch arm one line wide — it sits against the 500-SLOC production cap —
/// and gives the two-outcome decision a testable home next to the predicate it
/// depends on.
/// What: a repo-shaped token runs through [`run_managed`], the same cold start
/// `tm run <owner>/<repo>` uses, so an already-registered repo refreshes and
/// runs rather than erroring. Anything else goes to
/// [`reject_unknown_subcommand`], which reproduces the exact usage error the
/// invocation produced before [`crate::cli::Command::External`] existed.
/// Trailing tokens are refused rather than silently dropped: `tm <url> extra`
/// means something the CLI cannot honour.
/// Test: the repo half is [`classify_bare`]'s coverage plus `tm run`'s existing
/// managed-checkout tests; the usage-error half exits the process and is
/// covered at the parse layer by `cli_bare_unknown_subcommand_is_not_a_repo`,
/// `cli_bare_ambiguous_prefix_is_not_a_repo`, and
/// `cli_bare_retired_subcommand_is_not_a_repo`.
pub(crate) async fn run_external(
    client: &reqwest::Client,
    url: &str,
    tokens: &[String],
    argv: &[String],
    help: &trusty_common::help::HelpConfig,
) -> anyhow::Result<()> {
    let token = tokens.first().map(String::as_str).unwrap_or_default();
    let Some(classified) = classify_bare(token) else {
        reject_unknown_subcommand(argv, help);
    };

    if tokens.len() > 1 {
        anyhow::bail!(
            "tm {token} takes no further arguments (got {extra:?}). \
             Use `tm run {token}` for the flag-bearing form.",
            extra = &tokens[1..]
        );
    }

    match classified? {
        RunTarget::Repo {
            owner,
            repo,
            clone_url,
        } => run_managed(client, url, &owner, &repo, &clone_url).await,
        // `classify_bare` returns `None` rather than an alias, so this is
        // unreachable; routing it to the same cold start keeps the arm total.
        RunTarget::Alias(alias) => Err(super::register_args::rejection(&alias)),
    }
}

/// Print the usage error a token would have produced before #6441, and exit.
///
/// Why: `external_subcommand` makes clap ACCEPT any leading token it does not
/// recognize, so three previously-failing invocations now parse — a typo
/// (`tm statuss`), an AMBIGUOUS prefix (`tm sta`, which matches `start`,
/// `status`, and `statusline`), and a retired subcommand (`tm coordinator-tui`,
/// #1392). All three must still be refused, and with clap's own wording: its
/// "tip: some similar subcommands exist" line names the real candidates, which
/// a hand-written message cannot reproduce.
/// What: re-parses the ORIGINAL argv against a command carrying the same
/// arguments and subcommands but WITHOUT the external catch-all — which is
/// exactly the pre-#6441 definition — then prints that error, appends the
/// workspace "did you mean?" hint the way `main` does, and exits with clap's
/// own code.
///
/// 🔴 The rebuild is required. Calling `allow_external_subcommands(false)` on
/// the command `clap::CommandFactory` hands back does NOT disable the
/// catch-all: the flag reads back as `false` and `sta` still matches as the
/// external subcommand `sta`, because the derive already wired the match path
/// during augmentation. Only a command built fresh from
/// [`clap::Command::new`] honours the absence of the flag. Do not "simplify"
/// this back to a flag flip.
///
/// The re-parse cannot succeed — the token reached this function precisely
/// because nothing matched it — but an `Ok` is handled rather than unwrapped.
/// Test: `reject_unknown_subcommand_rebuild_reproduces_claps_error`,
/// `cli_bare_ambiguous_prefix_is_not_a_repo`,
/// `cli_bare_unknown_subcommand_is_not_a_repo`,
/// `cli_bare_retired_subcommand_is_not_a_repo`.
fn reject_unknown_subcommand(argv: &[String], help: &trusty_common::help::HelpConfig) -> ! {
    let code = match without_external_catch_all().try_get_matches_from(argv) {
        Err(e) => {
            e.print().ok();
            trusty_common::help::print_suggestion_hint(argv, help);
            e.exit_code()
        }
        // Unreachable: this token matched no subcommand a moment ago.
        Ok(_) => {
            eprintln!("error: unrecognized subcommand");
            2
        }
    };
    std::process::exit(code);
}

/// The `tm` command definition as it stood before #6441.
///
/// Why: see [`reject_unknown_subcommand`] — the pre-#6441 error can only be
/// reproduced by a command that never had the external catch-all wired in.
/// What: copies [`crate::cli::Cli`]'s own arguments and subcommands onto a
/// fresh [`clap::Command`], keeping `infer_subcommands` so an unambiguous
/// prefix still resolves and an ambiguous one still reports every candidate.
/// The name is spelled out because `clap::builder::Str` only accepts a
/// `&'static str`; `command_name_matches_the_rebuild` pins it against the
/// derive so the two cannot drift.
/// Test: `command_name_matches_the_rebuild`,
/// `reject_unknown_subcommand_rebuild_reproduces_claps_error`.
pub(crate) fn without_external_catch_all() -> clap::Command {
    use clap::CommandFactory as _;

    let src = crate::cli::Cli::command();
    clap::Command::new(CLI_NAME)
        .infer_subcommands(true)
        .args(src.get_arguments().cloned())
        .subcommands(src.get_subcommands().cloned())
}

/// The `#[command(name = ...)]` on [`crate::cli::Cli`].
///
/// Test: `command_name_matches_the_rebuild`.
pub(crate) const CLI_NAME: &str = "trusty-mpm";

/// Cold-start a daemon-managed session for a GitHub `owner/repo`.
///
/// Why: this is the whole point of #4990 — a bare string ends in a real
/// `SessionRecord` with a tmux pane, visible in `tm ls` and `tm sessions`, not
/// a blocking foreground `claude`.
/// What: (1) ensures the managed base clone at
/// `<repos_root>/<owner>/<repo>` via
/// [`trusty_mpm::daemon::managed_routes::inproject_cold_start::ensure_managed_checkout`],
/// which clones when absent and fails loud on a mismatched remote — a dirty
/// tree is NOT a failure there, it reports a skipped fast-forward through
/// `ManagedCheckout::refresh_skipped`, which this function prints; (2) hands
/// that directory to [`super::launch::launch`], the existing daemon-managed
/// launch path — which registers the project alias, provisions the per-session
/// worktree, prepares the session, registers the session with the daemon,
/// creates the tmux host, and attaches.
///
/// Step 2 is a HANDOFF, not a reimplementation: everything after the checkout
/// exists already and is shared with `tm launch`, so a managed session started
/// cold is indistinguishable from one started from a local checkout.
/// Test: the checkout half is covered by the `inproject_cold_start` tests and
/// `tests/inproject_cold_start.rs`; the launch half is `tm launch`'s existing
/// coverage.
async fn run_managed(
    client: &reqwest::Client,
    url: &str,
    owner: &str,
    repo: &str,
    clone_url: &str,
) -> anyhow::Result<()> {
    let checkout =
        trusty_mpm::daemon::managed_routes::inproject_cold_start::ensure_managed_checkout(
            owner, repo, clone_url,
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if checkout.reused {
        eprintln!(
            "tm: reusing managed checkout {}",
            checkout.base_path.display()
        );
    } else {
        eprintln!("tm: cloned {clone_url} → {}", checkout.base_path.display());
    }

    // A skipped fast-forward must be VISIBLE, not just logged. `pull_ff_only`
    // (`core::standalone::load.rs`) is the shape to avoid: it warns where nobody
    // looks and returns Ok, so a stale checkout reads as a refreshed one.
    if let Some(reason) = &checkout.refresh_skipped {
        eprintln!(
            "tm: warning: did NOT fast-forward {} — {reason}",
            checkout.base_path.display()
        );
        eprintln!(
            "tm:          the session's branch is cut from a freshly-fetched \
             origin/<default-branch>, so it is unaffected (#4957). \
             The checkout itself stays where you left it."
        );
    }

    // #5274: `tm run` targets an already-resolved managed checkout, so the
    // session runs there directly; it carries no operator worktree request.
    // #5836: `ensure_managed_checkout` IS the resolution, so say so rather than
    // let `provision_for_launch` redo it against a differently-spelled path.
    super::launch::launch(
        client,
        url,
        Some(checkout.base_path.to_string_lossy().into_owned()),
        None,
        false,
        super::managed_workspace::LaunchDir::CallerResolved,
    )
    .await
}

#[cfg(test)]
#[path = "run_target_tests.rs"]
mod tests;
