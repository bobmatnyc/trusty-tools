//! The `gh` subprocess runner the merged-PR reclaim survey calls (#2919, #6561).
//!
//! Why: split out of `worktree_reclaim` so that file stays under the 500-SLOC
//! production cap, and because this half answers a different question. That
//! module decides what a pull-request state MEANS; this one is responsible for
//! obtaining it from `gh` and — since #6561 — for saying WHY it could not.
//!
//! #6561: every failure used to collapse into one `None`. The daemon runs with
//! neither `GH_TOKEN` nor `GH_CONFIG_DIR` in its environment, so on a host that
//! keeps its credentials in a scoped config directory `gh pr list` exits 4 with
//! `To get started with GitHub CLI, please run:  gh auth login`. That exit was
//! discarded, every branch read `Unknown`, and `--merged-prs` reported
//! `0 worktree(s) reclaimable` — indistinguishable from a healthy sweep over a
//! workspace with nothing to reclaim. The runner now returns the exit code and
//! the first line of `gh`'s own stderr so the caller can disclose it.
//!
//! Test: `run_with_timeout_captures_output`, `run_with_timeout_kills_a_hung_child`,
//! `run_with_timeout_reports_the_exit_code_and_stderr` in
//! `worktree_reclaim::worktree_reclaim_tests`.
//!
//! #6623: the daemon runs under launchd, which carries neither `GH_TOKEN` nor
//! `GH_CONFIG_DIR` — every `gh` call made from this module used to inherit
//! that bare environment and exit 4 ("gh auth login"). [`resolve_daemon_gh_env`]
//! resolves the same per-project/global `github:` binding an interactive `tm`
//! invocation would (via `core::gh_identity`), and [`gh_command`] applies it.
//!
//! #6867: killing the `gh` PID alone leaked the process TREE. `gh` reads its
//! token by running `/usr/bin/security find-generic-password`, and a wedged
//! `securityd` makes that grandchild block forever: the timeout killed `gh`,
//! the grandchild was reparented to launchd, and roughly 200 orphan pairs
//! accumulated on one host in a couple of hours. Every child this runner
//! spawns is now put in its OWN process group and the whole GROUP is signalled
//! on expiry. Deciding whether to spawn at all — the single-flight guard and
//! the consecutive-timeout backoff — belongs to
//! [`super::worktree_reclaim_gh_gate`].

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::core::gh_identity::{self, GhEnv};
use crate::core::trusty_tools_config::TrustyToolsConfig;

/// Environment variables that would point `gh`/`git` at a DIFFERENT repository.
///
/// Why: the same hazard `worktree_safety::GIT_REDIRECTING_ENV` documents —
/// `gh` resolves the repository through git, and an inherited `GIT_DIR` names
/// a repository the worktree under inspection has nothing to do with. A
/// work-destroying gate must not be steerable by ambient environment.
/// What: removed from the child environment so the repository is resolved from
/// the working directory alone.
/// Test: `gh_command_strips_repository_redirecting_env`.
const GH_STRIPPED_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GH_REPO",
    // #8510: an inherited host points `gh` at a host no binding chose;
    // a binding's own `GH_HOST` is re-applied after the strip.
    "GH_HOST",
];

/// The `--json` field set every `gh pr list` call in this module requests.
///
/// Why: named once so the bulk index and the per-branch fallback cannot drift
/// into asking for different facts. `isCrossRepository` is not optional — see
/// `PrRow::is_cross_repository`; a `gh` that does not understand the field
/// makes the whole call fail, which yields an unavailable index and blocks,
/// rather than silently returning rows with fork PRs indistinguishable from
/// local ones.
///
/// `headRefName` is what resolves a SQUASH-MERGED pull request whose head
/// branch was deleted at merge (#6561): the branch is gone from the remote, but
/// the pull request still records the name it was opened from, so
/// `gh pr list --head <branch> --state all` still answers `MERGED`.
/// Test: `pr_index_skips_fork_pull_requests`,
/// `pr_index_resolves_a_squash_merged_pr_whose_head_branch_was_deleted`.
pub(crate) const PR_JSON_FIELDS: &str = "number,headRefName,state,isCrossRepository";

/// Wall-clock ceiling for any single `gh` invocation (#2919).
///
/// Why: `gh pr list` is a NETWORK call. `std::process::Command::output()` has
/// no timeout, so a wedged or throttled request hangs the calling thread
/// forever — and because this runs inside `spawn_blocking`, which cannot be
/// cancelled, that hang propagates to runtime shutdown. This is the same
/// failure shape already fixed for the byte walk; a subprocess needs its own
/// bound for the same reason.
/// What: 10 seconds, after which the child is killed and the call reports a
/// timeout — which resolves to a blocked candidate either way.
/// Test: `run_with_timeout_kills_a_hung_child`.
pub(crate) const GH_TIMEOUT: Duration = Duration::from_secs(10);

/// A `gh` invocation rooted at `dir` with a sanitised environment and the
/// resolved `gh` identity applied (#2919, #6623).
///
/// Why: `gh` resolves the repository from its WORKING DIRECTORY. It has no
/// global `-C` flag — that is a `git` habit, and carrying it across made every
/// call in this module fail at flag parsing with
/// `unknown shorthand flag: 'C' in -C` (verified against gh 2.96.0). The
/// failure was silent by design: a failed call made every branch read
/// `BranchPrState::Unknown`, `classify` gate 5 blocked every candidate, and
/// `--merged-prs` reclaimed zero bytes ALWAYS. Hence `current_dir`, and hence
/// `gh_command_passes_no_dash_c_flag`.
///
/// The pager and prompt suppression are not cosmetic either — an interactive
/// prompt in a daemon-run probe hangs it forever. See [`GH_STRIPPED_ENV`] for
/// the environment scrub.
///
/// #6623: `gh_env` is applied BEFORE the pager/prompt overrides so a `gh`
/// spawned under launchd — which inherits neither `GH_TOKEN` nor
/// `GH_CONFIG_DIR` — authenticates the same way an interactive `tm`
/// invocation would. It is injected rather than resolved internally so the
/// existing hermetic tests (a bare `/tmp` directory, no real config on disk)
/// stay pure; [`resolve_daemon_gh_env`] is the production resolver every real
/// call site uses.
/// What: `gh` with its working directory set to `dir`, the repository-
/// redirecting variables removed, `gh_env`'s overrides applied, and
/// pager/prompt/colour disabled.
/// Test: `gh_command_passes_no_dash_c_flag`,
/// `gh_command_runs_in_the_requested_directory`,
/// `gh_command_strips_repository_redirecting_env`,
/// `gh_command_applies_the_resolved_gh_env`.
pub(crate) fn gh_command(dir: &Path, gh_env: &GhEnv) -> Command {
    // #5475: argv/binary/env come from trusty-common's single `gh` entry
    // point; this module keeps its own kill-on-timeout runner, which is why it
    // takes the unspawned `std::process::Command` rather than a runner method.
    // #2919: `current_dir`, NEVER `-C` — `gh` has no such flag.
    let mut cmd = trusty_common::gh::GhCommand::bare().cwd(dir);
    for key in GH_STRIPPED_ENV {
        cmd = cmd.env_remove(key);
    }
    // #6668: clear the inherited identity BEFORE applying the binding — gh
    // reads an env token ahead of `GH_CONFIG_DIR`, so leaving the shell's
    // `GH_TOKEN` in place left the binding decorative.
    for key in gh_env.unset_vars() {
        cmd = cmd.env_remove(key);
    }
    for (key, value) in gh_env.vars() {
        cmd = cmd.env(key, value);
    }
    cmd.env("GH_PAGER", "")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("NO_COLOR", "1")
        .to_std_command()
}

/// `gh pr list` for `dir`, PINNED to the repository `repo` names (#7057).
///
/// Why: [`gh_command`] sets the working directory, and until #7057 that WAS the
/// repository selection — `gh` inferred the repository from whichever remote it
/// picked there, from a `remote.<name>.gh-resolved` config key, or, in a
/// directory git does not root a repository at, from an enclosing repository up
/// the filesystem. A prune run for `1m-consulting/adaptive-crm` resolved
/// against `hotstats/hotstats-product-poc` that way and reported "no pull
/// request found" for branches whose pull requests had just merged. Nothing in
/// the argv named a repository, so no output could disclose the substitution.
/// Naming it explicitly removes the inference, and puts the slug where every
/// error message and every argv assertion can read it.
/// What: [`gh_command`] plus `pr list --repo <repo>`. Callers append their own
/// filters (`--state`, `--head`, `--json`, `--limit`) after it. The slug comes
/// from [`super::worktree_repo_slug::repo_slug_for`], which fails closed — a
/// caller that cannot resolve one must refuse rather than call this — and which
/// gives `gh --repo`'s `[HOST/]OWNER/REPO` its optional host whenever the
/// remote is not on github.com, so an enterprise worktree's lookup names the
/// server as well as the repository (#7057).
/// Test: `two_worktrees_with_different_origins_produce_different_repo_flags`,
/// `an_enterprise_worktree_names_its_host_in_the_repo_flag`,
/// `gh_pr_list_command_names_the_repository_before_its_filters`.
pub(crate) fn gh_pr_list_command(dir: &Path, gh_env: &GhEnv, repo: &str) -> Command {
    let mut cmd = gh_command(dir, gh_env);
    cmd.args(["pr", "list", "--repo", repo]);
    cmd
}

/// Will `gh` have to look its token up itself, rather than read one this
/// binding handed it (#6867)?
///
/// Why: that lookup is `/usr/bin/security find-generic-password` on macOS, and
/// it is the call that hangs when `securityd` is wedged — the root cause of
/// this issue's orphan pairs. Whether a poll is exposed to it is decided
/// entirely by whether the resolved binding SETS an identity, so the check is
/// a pure function of the [`GhEnv`] and gets tested as one.
/// What: true when neither `GH_TOKEN` nor `GH_CONFIG_DIR` is set — the ambient
/// fallback [`resolve_daemon_gh_env`] takes when no `github:` binding applies.
/// Test: `ambient_gh_env_is_reported_as_keychain_bound`,
/// `a_config_dir_binding_avoids_the_keychain`.
pub(crate) fn consults_the_keychain(env: &GhEnv) -> bool {
    !env.vars()
        .iter()
        .any(|(k, _)| k == "GH_TOKEN" || k == "GH_CONFIG_DIR")
}

/// The one-time warning an unbound daemon `gh` earns (#6867).
///
/// Why: separated from the emission so the wording — which is the whole value
/// of the warning, since it has to tell the operator what to configure — is
/// assertable without capturing a `tracing` subscriber. Naming a token store
/// of our own is deliberately NOT offered: `gh_identity` already resolves a
/// token through `github.token_env`, and a second secret store would be a new
/// place for a credential to leak.
/// Test: `the_keychain_warning_names_both_bindings`.
pub(crate) fn keychain_warning() -> String {
    "worktree-reclaim: no `github:` binding resolved, so every `gh` poll will look its \
     own credentials up — on macOS via `/usr/bin/security find-generic-password`, which \
     never returns while `securityd` is wedged and used to leak one orphan process pair \
     per poll (#6867). Bind an identity in trusty-tools config under `github:` — \
     `token_env: <NAME OF AN ENV VAR THE DAEMON CARRIES>`, or `config_dir: <a gh config \
     home>` — so the daemon's polls never touch the keychain."
        .to_string()
}

/// Emit [`keychain_warning`] at most once for the life of the process.
///
/// Why: the reclaim survey resolves an identity per registry root and runs on
/// every `tm doctor` and every prune, so a per-call warning would be the
/// noisiest line in the log for a condition that is one configuration fix.
fn warn_once_about_the_keychain() {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| tracing::warn!("{}", keychain_warning()));
}

/// Resolve the [`GhEnv`] to apply to a `gh` spawn rooted at `dir` — the
/// production wiring every real call site in this module uses (#6623, #5850).
///
/// Why: the daemon's `gh` spawn sites have a WORKING DIRECTORY only. Under
/// launchd they inherit neither `GH_TOKEN` nor `GH_CONFIG_DIR` (#6623's root
/// cause), so each call must resolve its own identity the same way an
/// interactive `tm` invocation's `resolve_project_aware` does. #5850 adds the
/// tier that was missing: `tm --user <login> <url>` persists the selected
/// account onto the project's REGISTRY record, which the static
/// [`TrustyToolsConfig`] `projects:` list never sees — so a repository only
/// that account can see was probed as the machine's global account and every
/// branch under it blocked with "Could not resolve to a Repository".
/// What: takes the repository the caller already resolved (`origin`, an
/// `owner/repo` slug or a URL — #5850 dropped the second `git config` read of
/// `dir`), then asks
/// [`crate::core::gh_account_registry::pinned_gh_env_with`] FIRST, against this
/// host's registry directory. Only
/// "no pin recorded" falls through to [`gh_identity::select_config_for_origin`]
/// over the static config; every unanswerable registry outcome is returned as a
/// [`GhFailure`], which the caller renders as
/// `BranchPrState::LookupFailed { reason }` rather than probing as the wrong
/// user. A static-config `account`-only binding DOES name an account, but
/// `resolve_gh_env` refuses to honour it (#5851: `gh auth token -u` does not
/// select an account on a keyring-backed host). That refusal is logged at `warn`
/// and `gh` spawns with the ambient environment, so the probe runs as the
/// machine's globally active account, which may not be the one configured. This
/// static-tier fallback is unchanged by #5850.
/// Test: `registry_pin_resolves_the_projects_scoped_config_dir`,
/// `an_account_only_pin_fails_closed_naming_the_account` and the other
/// `gh_account_registry_tests` arms cover the registry tier;
/// `daemon_gh_env_refuses_when_the_registry_cannot_answer` covers the wiring;
/// the static tier is unit-tested via `select_config_for_origin_*` in
/// `core::gh_identity`.
pub(crate) fn resolve_daemon_gh_env(dir: &Path, origin: &str) -> Result<GhEnv, GhFailure> {
    resolve_daemon_gh_env_in(dir, origin, &crate::project::registry_data_dir())
}

/// [`resolve_daemon_gh_env`] against an explicit registry directory (#5850).
///
/// Why: the seam that lets a test prove a registry refusal reaches the caller
/// as a [`GhFailure`] instead of falling through to the ambient account.
/// Test: `daemon_gh_env_refuses_when_the_registry_cannot_answer`,
/// `daemon_gh_env_uses_the_registry_pin`.
pub(crate) fn resolve_daemon_gh_env_in(
    dir: &Path,
    origin: &str,
    registry_dir: &Path,
) -> Result<GhEnv, GhFailure> {
    let origin = slug_as_url(origin);
    let config = TrustyToolsConfig::load();
    // #8510: an account-only registry pin may borrow the static binding's dir
    // or tm's own `gh-accounts/<login>` — only once `gh` proves it selects it.
    let sources = crate::core::gh_account_dir::AccountDirSources::for_origin(
        &config,
        &origin,
        crate::core::paths::FrameworkPaths::default().root,
    );
    let probe = crate::core::gh_account_dir::CliTokenProbe;
    // #5850: the registry is what the operator-facing pinning paths write, so
    // it is consulted before the static config — and its failures BLOCK.
    match crate::core::gh_account_registry::pinned_gh_env_with(
        registry_dir,
        &origin,
        &sources,
        &probe,
    ) {
        Ok(Some(env)) => return Ok(env),
        Ok(None) => {}
        Err(reason) => return Err(GhFailure::new(reason)),
    }
    let selected = gh_identity::select_config_for_origin(&config, Some(&origin));
    let env = match gh_identity::resolve_gh_env(selected) {
        Ok(env) => env,
        Err(e) => {
            tracing::warn!(
                dir = %dir.display(),
                "worktree-reclaim: {e} — spawning gh with the ambient environment (#6623)"
            );
            GhEnv::default()
        }
    };
    // #6867: an unbound gh is the one that consults the keychain on every poll.
    if consults_the_keychain(&env) {
        warn_once_about_the_keychain();
    }
    Ok(env)
}

/// Turn a `repo_slug_for` slug back into a URL `repo_url_matches` can compare.
///
/// Why (#5850): `parse_github_path` reads a bare `owner/repo` as `host/repo`
/// and drops the owner, so a slug never matched a registered `repo_url`.
/// What: a value carrying `://` or `@` is already a URL and passes through;
/// `owner/repo` gains `https://github.com/`; `host/owner/repo` gains `https://`.
/// Test: `daemon_gh_env_uses_the_registry_pin`.
fn slug_as_url(origin: &str) -> String {
    if origin.contains("://") || origin.contains('@') {
        origin.to_string()
    } else if origin.matches('/').count() == 1 {
        format!("https://github.com/{origin}")
    } else {
        format!("https://{origin}")
    }
}

/// Why one `gh` call failed, and whether it failed by HANGING (#6561, #6867).
///
/// Why: `Result<String, String>` said what to print but not what to do next.
/// The backoff in [`super::worktree_reclaim_gh_gate`] must count timeouts and
/// only timeouts — an exit-4 auth failure returns instantly and no amount of
/// waiting fixes it, whereas a wedged keychain is exactly the thing that must
/// stop being retried every few seconds. String-matching the reason text to
/// tell them apart would make a wording change silently disable the backoff.
/// What: the operator-readable one-liner, the resolved `gh` identity when one
/// was resolved before the failure, and `timed_out`, set ONLY where the
/// wall-clock budget expired. [`std::fmt::Display`] renders the line the CLI
/// summary and the doctor message carry.
/// Test: `run_with_timeout_reports_the_exit_code_and_stderr`,
/// `run_with_timeout_marks_a_timeout_as_timed_out`,
/// `gh_failure_displays_the_resolved_identity`.
#[derive(Debug, Clone)]
pub(crate) struct GhFailure {
    reason: String,
    identity: Option<String>,
    timed_out: bool,
}

impl GhFailure {
    /// A failure that is NOT a hang — a spawn error, a non-zero exit, a wait
    /// error, or the gate's own refusal to spawn.
    pub(crate) fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            identity: None,
            timed_out: false,
        }
    }

    /// A failure caused by the wall-clock budget expiring — the only kind the
    /// backoff counts (#6867).
    pub(crate) fn timeout(reason: impl Into<String>) -> Self {
        Self {
            timed_out: true,
            ..Self::new(reason)
        }
    }

    /// Attach the `gh` identity the caller resolved before spawning (#6623).
    #[must_use]
    pub(crate) fn with_identity(mut self, identity: impl Into<String>) -> Self {
        self.identity = Some(identity.into());
        self
    }

    /// Did the child outlive its budget rather than answer?
    pub(crate) fn timed_out(&self) -> bool {
        self.timed_out
    }

    /// The resolved identity, or a note that the call never got that far.
    pub(crate) fn identity(&self) -> &str {
        self.identity
            .as_deref()
            .unwrap_or("no gh identity was resolved — the call never spawned")
    }
}

impl std::fmt::Display for GhFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.identity {
            Some(id) => write!(f, "{} (resolved gh identity: {id})", self.reason),
            None => f.write_str(&self.reason),
        }
    }
}

/// One operator-readable line naming why a `gh` call failed (#6561).
///
/// Why: the reason is rendered into a CLI summary and a doctor message, so it
/// has to be one line and has to carry the fact that identifies the fix — the
/// exit code plus `gh`'s own first complaint. `gh auth login` and
/// `gh: command not found` need opposite responses and used to read alike.
/// What: the first non-blank stderr line, prefixed by the exit code, or by a
/// signal note when the child was killed rather than exiting.
/// Test: `run_with_timeout_reports_the_exit_code_and_stderr`.
fn failure_reason(code: Option<i32>, stderr: &str) -> String {
    let first = stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("no error output");
    match code {
        Some(c) => format!("`gh` exited {c}: {first}"),
        None => format!("`gh` was terminated by a signal: {first}"),
    }
}

/// Run `cmd`, killing its whole process group if it outlives `budget`
/// (#2919, #6561, #6867, #7965).
///
/// Why: see [`GH_TIMEOUT`]. `Command::output()` waits indefinitely, and — as
/// #6867 found — killing only the `gh` pid leaves its keychain grandchild
/// behind. #7965 moved those mechanics to
/// [`crate::core::bounded_proc::run_bounded`], which the in-project hygiene
/// sweep needs too; this function is now the `gh`-specific ADAPTER over it and
/// owns only the failure taxonomy the reclaim gate branches on.
/// What: delegates the spawn/drain/poll/kill to `run_bounded`, then maps the
/// outcome — `Ok` carries stdout for a zero exit; a non-zero exit becomes a
/// [`GhFailure`] carrying `gh`'s own first stderr line, and a timeout becomes
/// one carrying the `timed_out` flag the backoff counts. Every `Err` blocks
/// reclamation.
/// Test: `run_with_timeout_captures_output`, `run_with_timeout_kills_a_hung_child`,
/// `run_with_timeout_kills_the_whole_process_group`,
/// `run_with_timeout_marks_a_timeout_as_timed_out`,
/// `run_with_timeout_reports_the_exit_code_and_stderr`.
pub(crate) fn run_with_timeout(cmd: Command, budget: Duration) -> Result<String, GhFailure> {
    use crate::core::bounded_proc::{BoundedError, run_bounded};

    match run_bounded(cmd, budget) {
        Ok(out) if out.status.success() => Ok(out.stdout),
        Ok(out) => Err(GhFailure::new(failure_reason(
            out.status.code(),
            &out.stderr,
        ))),
        Err(BoundedError::TimedOut) => Err(GhFailure::timeout(format!(
            "`gh` did not answer within {}s and its process group was killed",
            budget.as_secs()
        ))),
        Err(BoundedError::Spawn(e)) => Err(GhFailure::new(format!("`gh` could not be run: {e}"))),
        Err(BoundedError::NoPipe(which)) => {
            Err(GhFailure::new(format!("`gh` exposed no {which} pipe")))
        }
        Err(BoundedError::Wait(e)) => {
            Err(GhFailure::new(format!("`gh` could not be waited on: {e}")))
        }
    }
}
