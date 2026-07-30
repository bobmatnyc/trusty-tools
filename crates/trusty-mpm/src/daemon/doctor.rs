//! The `tm doctor` diagnostic engine.
//!
//! Why: a misconfigured trusty-mpm stack fails in confusing ways — sessions
//! launch with no instructions, agents never deploy, memory recall silently
//! returns nothing. `tm doctor` collapses every "is this wired correctly?"
//! question into one command so the operator gets a single, actionable verdict.
//! What: [`run_doctor`] runs independent probes — the instruction pipeline,
//! agent deployment, skill deployment, the DOC-28 output-style
//! configuration check (see `doctor_output_style`), and the trusty-memory /
//! trusty-search sidecars — and folds their outcomes into a
//! [`DoctorReport`]. Each network probe is bounded by [`PROBE_TIMEOUT`] so an
//! unreachable service cannot hang the report.
//! Test: `cargo test -p trusty-mpm-daemon doctor` exercises the filesystem
//! probes against temp directories and the HTTP probes against an in-process
//! test server.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::core::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::core::paths::FrameworkPaths;

use super::discover::{TRUSTY_MEMORY_DEFAULT_ADDR, TRUSTY_SEARCH_DEFAULT_ADDR, discover_addr};

// Split out to keep this file under the 500-SLOC production cap (DOC-28 R4(a)).
#[path = "doctor_output_style.rs"]
mod doctor_output_style;
use doctor_output_style::{
    check_output_style, check_output_style_legacy_ids, check_output_style_staleness,
};

// Split out to keep this file under the 500-SLOC production cap (A2,
// tm-skills-portfolio epic — adding check_skill_source pushed it over).
#[path = "doctor_fs_checks.rs"]
mod doctor_fs_checks;
use doctor_fs_checks::{check_agents, check_instructions, check_skill_source, check_skills};

// Split out to keep this file under the 500-SLOC production cap (issue #2158
// — the full manifest-completeness diff, layered on top of the narrower
// check_agents/check_skills/check_output_style probes above).
#[path = "doctor_deploy_validate.rs"]
mod doctor_deploy_validate;
use doctor_deploy_validate::check_deployment_completeness;

// Split out to keep this file under the 500-SLOC production cap (issue #2876 —
// the skill-staleness and legacy-instruction-source probes).
#[path = "doctor_staleness.rs"]
mod doctor_staleness;
use doctor_staleness::{check_legacy_instruction_sources, check_skill_staleness};

// Split out to keep this file under the 500-SLOC production cap (DOC-42,
// issue #2889 — the agent-bundled-skills dangling-reference / prose-mention
// probe).
#[path = "doctor_agent_skills.rs"]
mod doctor_agent_skills;
use doctor_agent_skills::check_agent_skills;

// Split out to keep this file under the 500-SLOC production cap (issue #2940
// — the tm hook contamination / foreign claude-mpm hook conflict probe).
#[path = "doctor_hooks_hygiene.rs"]
mod doctor_hooks_hygiene;
use doctor_hooks_hygiene::check_hooks_hygiene;

// Split out to keep this file under the 500-SLOC production cap (issue #2997 —
// the macOS TCC responsibility-disclaim visibility probe).
#[path = "doctor_tcc.rs"]
mod doctor_tcc;
use doctor_tcc::check_tcc_taint;

// Split out to keep this file under the 500-SLOC production cap (issue #3427 —
// the harness-scaffolding tracked-in-git-AND-regenerated-locally probe).
#[path = "doctor_scaffold_tracking.rs"]
mod doctor_scaffold_tracking;
use doctor_scaffold_tracking::check_scaffold_tracking;

// Split out to keep this file under the 500-SLOC production cap (issue #2867 —
// the cross-branch push-guard coverage probe, which is what makes an
// unprotected already-provisioned base clone discoverable at all).
#[path = "doctor_push_guard.rs"]
mod doctor_push_guard;
use doctor_push_guard::check_push_guard;

/// Per-probe network timeout.
///
/// Why: a sidecar that is down or wedged must not stall the whole diagnostic;
/// a bounded probe turns "hung" into a clean verdict.
///
/// Raised from 2 s to 10 s for issue #4005. The old bound produced
/// "trusty-memory unreachable at 127.0.0.1:7070" against a daemon whose MCP
/// surface was verifiably serving in the same minutes: trusty-memory's
/// `/health` samples process RSS/CPU behind a mutex and enumerates open file
/// descriptors, none of which the MCP request path touches, so under load
/// `/health` can exceed a budget that real traffic never approaches. The probe
/// was measuring its own impatience. Note that the timeout is only half the
/// fix — a timeout now resolves to [`CheckStatus::Unknown`] rather than a
/// false `Fail` (see [`probe_health`]).
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Number of attempts a health probe makes before giving up.
///
/// Why (issue #4005): the probe was single-shot, so one unlucky sample — a GC
/// pause, a burst of concurrent MCP traffic, the warm-up window right after a
/// `tm` restart that the issue explicitly calls out — became a hard failure
/// verdict with no second opinion. Two retries cost nothing on the healthy
/// path (the first attempt succeeds and returns immediately) and remove the
/// single-sample fragility on the unhealthy one.
const PROBE_ATTEMPTS: usize = 3;

/// Delay between health-probe attempts.
///
/// Why: long enough to let a transient spike pass, short enough that three
/// attempts stay well inside an interactive `tm doctor` run.
const PROBE_RETRY_DELAY: Duration = Duration::from_millis(500);

/// The trusty-search index `tm doctor` expects to exist for this repo.
const EXPECTED_SEARCH_INDEX: &str = "trusty-mpm";

/// Run every diagnostic probe and assemble the report.
///
/// Why: the single entry point behind `GET /api/v1/doctor` and `tm doctor` —
/// running all probes here keeps the check set identical across every UI.
/// What: runs the instruction / agent / skill / output-style filesystem
/// probes (the output-style probe closes DOC-28 F4 — the "did the
/// trusty-mpm instructions actually load" gap), the `deployment` probe (issue
/// #2158 — a full manifest-completeness diff against the canonical bundled
/// roster, layered on top of the narrower agent/skill/output-style probes),
/// then the memory and search HTTP probes (each bounded by [`PROBE_TIMEOUT`]),
/// a worktree-orphan scan (Fix 1b, #1840), the `skill_source` probe (A2,
/// tm-skills-portfolio epic), the `gh_account` probe
/// (#gh-account-awareness — surfaces the active github.com identity and warns
/// on the multi-account ambiguity), and the `oauth_token` probe (issue #2246 —
/// warns when a managed session risks the `CLAUDE_CONFIG_DIR`-keyed Keychain
/// login loop), the `skill_staleness` and `legacy_sources` probes (issue #2876
/// — warn when deployed skills differ from the bundled assets, or when legacy
/// global instruction sources linger), and the `agent_skills` /
/// `agent_skills_prose_hints` probe pair (DOC-42, issue #2889 — the former
/// flags dangling `skills:` frontmatter references and can `Warn`; the
/// latter is always `Ok` and carries undeclared prose skill mentions as
/// informational text, per issue #2906 review — folding both severities
/// into one check caused alert fatigue), and the `hooks_contamination` /
/// `hooks_foreign_conflict` probe pair (issue #2940 — the former warns when
/// a project-level `.claude/settings*.json` still carries tm hook entries
/// from a pre-fix `tm install`'s `$HOME`-wide write and points at `tm hooks
/// clean`; the latter warns, informationally only, when a project carries
/// its own claude-mpm hook entries that would fire inside a tm session), and
/// the `output_style_staleness` probe (issue #2333 — content-diffs each
/// deployed output-style file against the bundled catalog and flags orphaned
/// files under `output-styles/`, closing the gap where `check_output_style`
/// only validates that the configured id RESOLVES to a file, not that its
/// content is current), the `output_style_legacy_ids` probe (issue #3453 part
/// 2 — Warn-only scan of every settings LAYER, not just the effective one,
/// for a legacy/unresolvable `outputStyle` id sitting dormant in a currently
/// shadowed layer such as `settings.local.json`), the `tcc_taint` probe
/// (issue #2997 — surfaces whether managed panes spawn `claude` with macOS
/// TCC responsibility disclaimed, so the "would like to access data…" prompt
/// class is diagnosable rather than silent; synchronous and instantaneous, no
/// `log show` scan), and the `scaffold_tracking` probe (issue #3427 — warns
/// when a harness-owned path under `.claude/agents/`, `.claude/skills/`, or
/// `.claude/output-styles/` is BOTH tracked in `project_dir`'s git index AND
/// regenerated locally by tm, the precondition for a `git merge --ff-only`
/// "would be overwritten" collision; reports the exact true-intersection
/// paths plus a copy-pasteable `git rm -r --cached` remediation, never runs
/// it itself), and the `push_guard` probe (issue #2867 — warns when
/// `project_dir`'s clone has no trusty-mpm cross-branch `pre-push` guard, or
/// carries an older revision of it, naming the `tm repair push-guard`
/// retrofit; this is the only way a base clone provisioned BEFORE the guard
/// shipped is discoverable as unprotected) — folding the resulting twenty-two
/// [`DoctorCheck`]s into a [`DoctorReport`] whose `overall` status is the
/// worst of them.
///
/// Note (#1905): the mpm-*→tm-* stale-skill cleanup is intentionally NOT a
/// permanent probe here — it is a one-time migration
/// ([`crate::core::stale_skills::run_stale_mpm_skills_migration_once`]) run
/// from `tm`'s startup path instead, so it does real work at most once per
/// machine rather than nagging on every `tm doctor` invocation forever.
///
/// `project_dir` scopes the instruction and output-style probes; `repos_root`
/// (when `Some`) gives the managed workspace root for the worktree scan;
/// `active_workspace_paths` is the full set of workspace paths currently
/// registered to live sessions.
///
/// Issue #2149: `check_skills` is ALSO scoped by `project_dir` when it is
/// supplied. A managed session (#1931) deploys its SKILLS under
/// `<workspace>/.claude/skills`, not the operator's `$HOME/.claude` — probing
/// only the home-tier directory previously let a managed workspace with a
/// completely empty roster report a false `Ok` (because the operator's OWN
/// `$HOME/.claude/` was populated), silently missing the exact provisioning
/// gap that issue is about.
///
/// Issue #4409 removes AGENTS from that scoping: bundled agents deploy into
/// the one tm-managed `CLAUDE_CONFIG_DIR` tier and nowhere else, so
/// `check_agents`/`check_agent_skills` probe `paths.agent_deploy_dir()`, which
/// is the same directory whether or not a `project_dir` was supplied.
/// Test: `run_doctor_produces_twenty_two_checks`,
/// `agents_check_probes_the_managed_config_tier_not_the_workspace`.
pub async fn run_doctor(
    project_dir: Option<&Path>,
    repos_root: Option<&Path>,
    active_workspace_paths: &[PathBuf],
) -> DoctorReport {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let paths = match project_dir {
        Some(dir) => FrameworkPaths::for_managed_workspace(dir),
        None => FrameworkPaths::default(),
    };
    // Skills are probed at the same root the roster deploys to: the workspace
    // itself for a managed session, else the operator's home directory.
    let skills_root: &Path = project_dir.unwrap_or(&home);

    // DOC-42 issue #2906 review (MEDIUM finding): dangling `skills:`
    // references and informational prose-mention hints carry different
    // severities, so `check_agent_skills` now returns two independent
    // checks from one scan rather than folding both into a single `Warn`.
    let (agent_skills, agent_skills_prose_hints) = check_agent_skills(&paths);

    let mut checks = vec![
        check_instructions(project_dir),
        check_agents(&paths),
        check_skills(skills_root),
        check_skill_source(&paths),
        check_output_style(project_dir, &home),
        check_output_style_staleness(project_dir, &home),
        check_output_style_legacy_ids(project_dir, &home),
        check_deployment_completeness(&paths),
        check_skill_staleness(&paths),
        check_legacy_instruction_sources(&home),
        agent_skills,
        agent_skills_prose_hints,
    ];
    checks.push(check_memory(&home).await);
    checks.push(check_search(&home).await);
    checks.push(check_worktrees(repos_root, active_workspace_paths).await);
    checks.push(check_gh_account().await);
    checks.push(check_oauth_token_config());
    let (hooks_contamination, hooks_foreign_conflict) =
        check_hooks_hygiene(project_dir, active_workspace_paths);
    checks.push(hooks_contamination);
    checks.push(hooks_foreign_conflict);
    // Issue #2997: surface whether managed panes disclaim TCC responsibility so
    // the "trusty-mpm/tmux would like to access data…" prompt class is
    // diagnosable rather than silent. Synchronous + instantaneous (no log scan).
    checks.push(check_tcc_taint());
    // Issue #3427: warn when a harness-scaffolding path is BOTH tracked in
    // git and regenerated locally by tm — the precondition for a
    // `git merge --ff-only` "would be overwritten" collision. Warn-only;
    // never auto-modifies the git index.
    checks.push(check_scaffold_tracking(project_dir));
    // Issue #2867: the push guard installs on the CLONE path only, so a base
    // clone that predates it is silently unprotected. Warn-only, naming the
    // `tm repair push-guard` retrofit; doctor never writes into a repository.
    checks.push(check_push_guard(project_dir));

    DoctorReport::from_checks(checks)
}

/// Probe for the `CLAUDE_CONFIG_DIR`-keyed OAuth login-loop risk (issue #2246).
///
/// Why: every managed spawn relocates `CLAUDE_CONFIG_DIR` to the tm-owned
/// config home; on macOS the Keychain credential Claude Code reads is keyed by
/// a hash of that path, so a `/login` run inside a managed session can
/// diverge from the login stored under the operator's default config dir and
/// loop between "login successful" and "not logged in". Storing a
/// `CLAUDE_CODE_OAUTH_TOKEN` (`tm auth set-token`) bypasses the Keychain
/// entirely — this probe warns BEFORE an operator hits the loop rather than
/// after.
/// What: `Warn` when the managed config dir resolves (true in virtually every
/// real environment — see [`crate::core::trusty_tools_config::managed_claude_config_dir`])
/// AND no OAuth token will resolve for a managed spawn — i.e. neither a stored
/// token file NOR the `CLAUDE_CODE_OAUTH_TOKEN` env var is set (the two sources
/// [`crate::core::oauth_token::resolve_oauth_token`] consults, via
/// [`crate::core::oauth_token::compute_status`]); `Ok` otherwise. Ambient
/// `ANTHROPIC_API_KEY` deliberately does NOT count — every managed spawn strips
/// it with `env -u ANTHROPIC_API_KEY` (see [`crate::runtime::claude_code`]), so
/// relying on it still triggers the #2246 login loop while doctor would
/// otherwise falsely report `Ok`. Never `Fail` — a first `/login` still
/// frequently succeeds even without a stored token, so this is advisory.
/// Test: `oauth_token_check_warns_when_relocated_with_no_token_or_key`,
/// `oauth_token_check_ok_when_token_stored`,
/// `oauth_token_check_warns_when_only_api_key_set`.
fn check_oauth_token_config() -> DoctorCheck {
    let relocated = crate::core::trusty_tools_config::managed_claude_config_dir().is_some();
    let status = crate::core::oauth_token::compute_status();
    build_oauth_token_check(relocated, status.stored_token_present, status.env_var_set)
}

/// Pure verdict for [`check_oauth_token_config`], separated for hermetic
/// testing without mutating real process env / home state.
///
/// Why: keeping the branch logic pure makes both the warn and ok paths
/// unit-testable without redirecting `HOME` or the process env.
/// What: `token_available = stored_token_present || env_var_set` — exactly the
/// two sources a managed spawn's [`crate::core::oauth_token::resolve_oauth_token`]
/// consults. `Warn` when `relocated && !token_available`, else `Ok`. Ambient
/// `ANTHROPIC_API_KEY` is intentionally NOT an input: managed spawns scrub it,
/// so it can never satisfy the managed-session auth requirement (#2246 doctor
/// false-negative fix).
/// Test: `oauth_token_check_warns_when_relocated_with_no_token_or_key`,
/// `oauth_token_check_ok_when_token_stored`,
/// `oauth_token_check_ok_when_env_var_set`,
/// `oauth_token_check_warns_when_only_api_key_set`,
/// `oauth_token_check_ok_when_not_relocated`.
fn build_oauth_token_check(
    relocated: bool,
    stored_token_present: bool,
    env_var_set: bool,
) -> DoctorCheck {
    let token_available = stored_token_present || env_var_set;
    if relocated && !token_available {
        DoctorCheck::new(
            "oauth_token",
            CheckStatus::Warn,
            "managed sessions relocate CLAUDE_CONFIG_DIR but no CLAUDE_CODE_OAUTH_TOKEN is \
             configured — a `/login` inside a managed session may loop between \"successful\" \
             and \"not logged in\" (issue #2246). Note an ambient ANTHROPIC_API_KEY does NOT \
             help: managed spawns strip it. Run `claude setup-token` then `tm auth set-token`",
        )
    } else {
        DoctorCheck::new(
            "oauth_token",
            CheckStatus::Ok,
            "managed-session auth looks configured",
        )
    }
}

/// Probe the active `gh` github.com account and warn on ambiguity.
///
/// Why (#gh-account-awareness): `tm` shells to `gh` for PR merges, issue edits,
/// and managed spawn, but had no awareness of WHICH github.com identity was
/// active. When several accounts are logged in, `gh` uses whichever is active —
/// and a non-admin active account silently broke `gh pr merge --admin` with
/// "1 approving review required". This probe makes the active account visible in
/// `tm doctor` and warns when the dangerous multi-account ambiguity exists.
/// What: off-loads the two bounded `gh`/`hosts.yml` reads to `spawn_blocking`
/// (so the async executor is not stalled), then folds the result via
/// [`build_gh_account_check`]. Advisory only: `Warn` when multiple accounts are
/// logged in or `gh` is unauthenticated, `Ok` for a single clear account — never
/// a hard `Fail`, since a missing/other `gh` identity is not a broken stack.
/// Test: `build_gh_account_check` covers every branch;
/// `gh_account_check_is_advisory_only` asserts it never returns `Fail`.
async fn check_gh_account() -> DoctorCheck {
    let (active, accounts) = tokio::task::spawn_blocking(|| {
        let active = crate::core::gh_account::active_gh_account();
        let accounts = crate::core::gh_account::logged_in_gh_accounts();
        (active, accounts)
    })
    .await
    .unwrap_or((None, Vec::new()));
    build_gh_account_check(active, accounts)
}

/// Fold the resolved gh-account state into a [`DoctorCheck`] (pure).
///
/// Why: keeping the verdict logic pure makes every branch — single account,
/// multi-account ambiguity, active-unknown, and unauthenticated — unit-testable
/// without a live `gh`.
/// What: returns `Warn` when `accounts` holds more than one login (naming them
/// and pointing at `gh auth switch`), `Warn` when no account is detected, `Warn`
/// when accounts exist but none is marked active, and `Ok` for a single clear
/// active account. Never `Fail` — this is advisory.
/// Test: `build_gh_account_check_single_ok`, `build_gh_account_check_multi_warn`,
/// `build_gh_account_check_unauthenticated_warn`,
/// `gh_account_check_is_advisory_only`.
fn build_gh_account_check(active: Option<String>, accounts: Vec<String>) -> DoctorCheck {
    if accounts.len() > 1 {
        let list = accounts.join(", ");
        let active_str = active.as_deref().unwrap_or("unknown");
        return DoctorCheck::new(
            "gh_account",
            CheckStatus::Warn,
            format!(
                "{} github.com accounts logged in ({list}); active is `{active_str}`. \
                 `gh` (and `gh pr merge --admin`) uses the ACTIVE account — if merges \
                 fail on permissions, run `gh auth switch` to the repo owner.",
                accounts.len()
            ),
        );
    }
    match active {
        Some(login) => DoctorCheck::new(
            "gh_account",
            CheckStatus::Ok,
            format!("active gh account: `{login}`"),
        ),
        None if accounts.is_empty() => DoctorCheck::new(
            "gh_account",
            CheckStatus::Warn,
            "gh is not authenticated (no github.com account) — `gh` calls will fail; \
             run `gh auth login`"
                .to_string(),
        ),
        None => DoctorCheck::new(
            "gh_account",
            CheckStatus::Warn,
            format!(
                "gh authenticated ({}) but no active account is set — run `gh auth switch`",
                accounts.join(", ")
            ),
        ),
    }
}

/// Probe for orphaned per-session git worktrees under the managed workspace root.
///
/// Why: `decommission` now calls `git worktree remove` (#1840), but sessions
/// that were decommissioned before this fix—or where `git worktree remove`
/// failed—may leave stale `.worktrees/<session-id>/` directories on disk. This
/// probe surfaces orphaned dirs so operators know to run
/// `tm session prune --worktrees`. Discovery is delegated to
/// [`crate::session_manager::prune::find_orphaned_worktrees`] inside
/// `spawn_blocking` so the async executor is not blocked by the synchronous
/// git subprocesses it spawns.
/// What: builds a canonicalized active-path set, then spawns a blocking task
/// that asks GIT for the worktrees of each managed project (#4207 — this is no
/// longer a filesystem walk of `.worktrees/`, and a candidate is no longer "any
/// leaf directory"): a git-registered worktree counts as an orphan when it is
/// not main/bare/prunable/locked, IS a strict descendant of the project whose
/// registry named it, and is absent from the active set. Reports `Ok` (no
/// orphans), `Warn` (orphans found), or `Ok` (repos_root absent /
/// unconfigured).
///
/// Note this probe is REPORT-ONLY — it counts, it never removes — so it
/// deliberately reports candidates the reclaim path would refuse to delete
/// (owner-unknown, dirty). Conversely a directory git does not register is not
/// counted at all, so an unregistered husk is invisible here; reclaiming those
/// is a separate open concern, deliberately out of scope for #4207.
/// Test: `worktrees_no_orphans_is_ok`, `worktrees_with_orphan_is_warn`.
async fn check_worktrees(
    repos_root: Option<&Path>,
    active_workspace_paths: &[PathBuf],
) -> DoctorCheck {
    let Some(root) = repos_root else {
        // No managed workspace root configured — this is a normal state for
        // operators who don't use in-project worktree sessions (#1840).
        return DoctorCheck::new(
            "worktrees",
            CheckStatus::Ok,
            "no managed workspace root configured — worktree scan skipped",
        );
    };
    if !root.is_dir() {
        return DoctorCheck::new(
            "worktrees",
            CheckStatus::Ok,
            format!("{} does not exist — no worktrees to check", root.display()),
        );
    }

    // Build a canonicalized HashSet for O(1) lookup and symlink safety (Fix 1b, #1840).
    let root = root.to_path_buf();
    let active_set: std::collections::HashSet<PathBuf> = active_workspace_paths
        .iter()
        .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
        .collect();

    // Delegate the blocking scan to spawn_blocking so the async executor is not
    // stalled (#1840 F). Since #4207 what blocks is not directory I/O but the
    // git subprocesses `find_orphaned_worktrees` spawns per managed project.
    let orphans = tokio::task::spawn_blocking(move || {
        crate::session_manager::prune::find_orphaned_worktrees(&root, &active_set)
    })
    .await
    .unwrap_or_else(|e| {
        tracing::error!("doctor: worktree scan panicked: {e}");
        Vec::new()
    });

    if orphans.is_empty() {
        DoctorCheck::new("worktrees", CheckStatus::Ok, "no orphaned worktrees found")
    } else {
        DoctorCheck::new(
            "worktrees",
            CheckStatus::Warn,
            format!(
                "{} orphaned worktree dir(s) found — run `tm session prune --worktrees` to remove",
                orphans.len()
            ),
        )
    }
}

/// Probe the trusty-memory sidecar's `/health` endpoint.
///
/// Why: memory recall and store route through trusty-memory; if it is down the
/// PM silently loses its long-term memory, so the operator must know.
/// What: resolves the service address via [`discover_addr`], then delegates to
/// [`probe_health`], which retries, distinguishes a refusal from a timeout, and
/// reads the daemon's own worker-pool observation out of the response body.
/// Test: `memory_unreachable_is_fail`, `memory_timeout_is_unknown_not_fail`,
/// `memory_wedged_worker_pool_is_not_ok`, `memory_warming_is_warn_not_fail`.
async fn check_memory(home: &Path) -> DoctorCheck {
    let dir = home.join(".trusty-memory");
    let default = TRUSTY_MEMORY_DEFAULT_ADDR
        .parse()
        .expect("static default is valid");
    let env = std::env::var("TRUSTY_MEMORY_ADDR").ok();
    let addr = discover_addr(&dir, default, env.as_deref()).await;
    probe_health(
        "memory",
        "trusty-memory",
        &format!("http://{addr}/health"),
        &addr.to_string(),
    )
    .await
}

/// Outcome of one `/health` request attempt.
///
/// Why (issue #4005): the old code collapsed every non-success into a single
/// `Err`, which is what made "timed out" and "connection refused" produce the
/// same "unreachable" verdict despite meaning opposite things operationally.
/// Naming the cases is what lets the caller be honest about which it saw.
/// What: a 2xx with its parsed body, a non-2xx, a timeout, or a refusal.
enum ProbeOutcome {
    /// 2xx. The body is `None` when it could not be read or parsed as JSON.
    Success(Option<serde_json::Value>),
    /// The service answered, but not with a 2xx.
    NonSuccess(u16),
    /// No answer within [`PROBE_TIMEOUT`] — we learned nothing.
    TimedOut,
    /// Connection refused / DNS / other transport failure — nothing is there.
    Unreachable(String),
}

/// Issue one `/health` request and classify the outcome.
async fn probe_health_once(url: &str) -> ProbeOutcome {
    let client = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => return ProbeOutcome::Unreachable(e.to_string()),
    };
    match client.get(url).send().await {
        Ok(resp) if resp.status().is_success() => {
            ProbeOutcome::Success(resp.json::<serde_json::Value>().await.ok())
        }
        Ok(resp) => ProbeOutcome::NonSuccess(resp.status().as_u16()),
        Err(e) if e.is_timeout() => ProbeOutcome::TimedOut,
        Err(e) => ProbeOutcome::Unreachable(e.to_string()),
    }
}

/// Probe a sidecar's `/health` and turn what was actually observed into a
/// [`DoctorCheck`] (issues #4005, #4001).
///
/// Why: this function encodes the principle both issues share. Doctor used to
/// infer health from a cheap proxy — "the socket answered" — instead of
/// observing the thing it claims to report. That produced a false NEGATIVE
/// when the proxy was merely slow (#4005) and a false POSITIVE when the
/// listener was fine but every worker was parked (#4001). So: retry before
/// concluding anything, separate "nothing is listening" from "nothing
/// answered in time", and prefer the daemon's own observation of its workers
/// over our inference from the status code.
/// What: up to [`PROBE_ATTEMPTS`] attempts. Returns `Ok` only when the daemon
/// positively reports a healthy worker pool; `Fail` on a refusal, a non-2xx,
/// or a reported wedge; `Warn` while warming or degraded; and `Unknown` when
/// every attempt timed out or the body carried no worker observation.
/// Test: `memory_unreachable_is_fail`, `memory_timeout_is_unknown_not_fail`,
/// `memory_wedged_worker_pool_is_not_ok`, `memory_warming_is_warn_not_fail`,
/// `memory_slow_but_serving_daemon_is_ok`.
async fn probe_health(check: &str, service: &str, url: &str, addr: &str) -> DoctorCheck {
    let mut last_timeout = false;
    let mut last_err: Option<String> = None;
    let mut last_status: Option<u16> = None;

    for attempt in 0..PROBE_ATTEMPTS {
        match probe_health_once(url).await {
            ProbeOutcome::Success(body) => {
                return interpret_health(check, service, addr, body.as_ref());
            }
            ProbeOutcome::NonSuccess(code) => {
                last_status = Some(code);
                last_timeout = false;
            }
            ProbeOutcome::TimedOut => {
                last_timeout = true;
            }
            ProbeOutcome::Unreachable(e) => {
                last_err = Some(e);
                last_timeout = false;
            }
        }
        if attempt + 1 < PROBE_ATTEMPTS {
            tokio::time::sleep(PROBE_RETRY_DELAY).await;
        }
    }

    if last_timeout {
        // Issue #4005: THE false negative. Do not claim the daemon is down —
        // we never established that. A slow /health is exactly what a healthy
        // daemon under load looks like from here.
        return DoctorCheck::new(
            check,
            CheckStatus::Unknown,
            format!(
                "{service} at {addr} did not answer /health within {}s across {PROBE_ATTEMPTS} \
                 attempts, but the connection was NOT refused — the daemon may be alive and \
                 merely slow. Health could not be determined. Check the MCP surface before \
                 restarting anything.",
                PROBE_TIMEOUT.as_secs()
            ),
        );
    }

    if let Some(code) = last_status {
        return DoctorCheck::new(
            check,
            CheckStatus::Fail,
            format!("{service} at {addr} returned HTTP {code}"),
        );
    }

    DoctorCheck::new(
        check,
        CheckStatus::Fail,
        format!(
            "{service} unreachable at {addr}: {}",
            last_err.unwrap_or_else(|| "connection refused".to_string())
        ),
    )
}

/// Map a 2xx `/health` body to a status (issues #4001, #4005).
///
/// Why: a 2xx proves a listener accepted a socket, not that the daemon is
/// doing work — which is precisely how #3992 kept `tm doctor` green for the
/// duration of an incident in which six threads were parked and a
/// `memory_remember` had been hung for ~1800 s. When the daemon reports its
/// own worker occupancy, that observation wins over our inference.
/// What: `Fail` on a reported wedge, `Warn` while warming or degraded,
/// `Unknown` when no worker block is present (an older daemon, or an
/// unreadable body — we cannot claim health we did not observe), `Ok`
/// otherwise.
/// Test: `memory_wedged_worker_pool_is_not_ok`, `memory_warming_is_warn_not_fail`,
/// `health_body_without_worker_block_is_unknown`.
fn interpret_health(
    check: &str,
    service: &str,
    addr: &str,
    body: Option<&serde_json::Value>,
) -> DoctorCheck {
    let Some(body) = body else {
        return DoctorCheck::new(
            check,
            CheckStatus::Unknown,
            format!(
                "{service} at {addr} answered /health but the body was unreadable — the \
                 listener is up; whether its workers are progressing is UNKNOWN"
            ),
        );
    };

    let worker = body.get("worker");
    let wedged = worker
        .and_then(|w| w.get("wedged"))
        .and_then(serde_json::Value::as_bool);

    match wedged {
        Some(true) => {
            let oldest = worker
                .and_then(|w| w.get("oldest_age_secs"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default();
            let in_flight = worker
                .and_then(|w| w.get("in_flight"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default();
            return DoctorCheck::new(
                check,
                CheckStatus::Fail,
                format!(
                    "{service} at {addr} is answering /health BUT reports a WEDGED worker \
                     pool: oldest in-flight operation {oldest}s, {in_flight} in flight. The \
                     listener responding does not mean writes are progressing (issue #3992)."
                ),
            );
        }
        None => {
            return DoctorCheck::new(
                check,
                CheckStatus::Unknown,
                format!(
                    "{service} at {addr} is reachable but does not report worker-pool \
                     occupancy (pre-#4001 build) — liveness confirmed, progress UNKNOWN"
                ),
            );
        }
        Some(false) => {}
    }

    // Issue #4005 explicitly calls out post-restart warm-up: it is a normal
    // transient state, not a failure, and must not read as fully healthy either.
    if body.get("daemon_state").and_then(|v| v.as_str()) == Some("warming") {
        return DoctorCheck::new(
            check,
            CheckStatus::Warn,
            format!(
                "{service} at {addr} is WARMING UP (embedder initialising) — normal shortly after a restart"
            ),
        );
    }

    if body.get("status").and_then(|v| v.as_str()) == Some("degraded") {
        let detail = body
            .get("detail")
            .and_then(|v| v.as_str())
            .unwrap_or("no detail reported");
        return DoctorCheck::new(
            check,
            CheckStatus::Warn,
            format!("{service} at {addr} reports DEGRADED: {detail}"),
        );
    }

    DoctorCheck::new(
        check,
        CheckStatus::Ok,
        format!("{service} healthy at {addr}, workers progressing"),
    )
}

/// Probe the trusty-search sidecar's health and the `trusty-mpm` index.
///
/// Why: code search backs the PM's "search before grep" rule; both the service
/// being up *and* the `trusty-mpm` index existing are required for it to work.
/// What: resolves the service address, checks `GET /health` (a non-2xx or
/// transport error is `Fail`), then checks `GET /indexes` for an index named
/// [`EXPECTED_SEARCH_INDEX`] — a healthy service missing that index is `Warn`.
/// Test: `search_unreachable_is_fail`.
async fn check_search(home: &Path) -> DoctorCheck {
    let dir = home.join(".trusty-search");
    let default = TRUSTY_SEARCH_DEFAULT_ADDR
        .parse()
        .expect("static default is valid");
    let env = std::env::var("TRUSTY_SEARCH_ADDR").ok();
    let addr = discover_addr(&dir, default, env.as_deref()).await;

    match http_get_ok(&format!("http://{addr}/health")).await {
        Ok(true) => {}
        Ok(false) => {
            return DoctorCheck::new(
                "search",
                CheckStatus::Fail,
                format!("trusty-search at {addr} returned a non-2xx status"),
            );
        }
        Err(e) => {
            return DoctorCheck::new(
                "search",
                CheckStatus::Fail,
                format!("trusty-search unreachable at {addr}: {e}"),
            );
        }
    }

    // Service is up — confirm the expected index exists.
    match http_get_json(&format!("http://{addr}/indexes")).await {
        Ok(body) if index_present(&body, EXPECTED_SEARCH_INDEX) => DoctorCheck::new(
            "search",
            CheckStatus::Ok,
            format!("trusty-search healthy at {addr}, `{EXPECTED_SEARCH_INDEX}` index present"),
        ),
        Ok(_) => DoctorCheck::new(
            "search",
            CheckStatus::Warn,
            format!(
                "trusty-search healthy at {addr} but the `{EXPECTED_SEARCH_INDEX}` index is missing"
            ),
        ),
        Err(e) => DoctorCheck::new(
            "search",
            CheckStatus::Warn,
            format!("trusty-search healthy at {addr} but listing indexes failed: {e}"),
        ),
    }
}

/// True when `body` mentions an index named `name`.
///
/// Why: the `/indexes` payload shape varies (a bare string array, or objects
/// with an `id`/`name` field); a tolerant scan avoids coupling the probe to one
/// exact wire form.
/// What: returns true when any array element equals `name` directly or carries
/// an `id`/`name`/`index_id` field equal to `name`.
/// Test: `index_present_matches_each_shape`.
fn index_present(body: &serde_json::Value, name: &str) -> bool {
    // The array may be the top-level value or nested under `indexes`.
    let array = body
        .as_array()
        .or_else(|| body.get("indexes").and_then(|v| v.as_array()));
    let Some(array) = array else {
        return false;
    };
    array.iter().any(|entry| {
        if entry.as_str() == Some(name) {
            return true;
        }
        ["id", "name", "index_id"]
            .iter()
            .any(|key| entry.get(key).and_then(|v| v.as_str()) == Some(name))
    })
}

/// Issue a timeout-bounded `GET` and report whether the status is 2xx.
///
/// Why: the memory and search health probes only need a yes/no liveness answer.
/// What: `GET url` with a [`PROBE_TIMEOUT`] client timeout; `Ok(true)` on a 2xx
/// response, `Ok(false)` on any other status, `Err` on a transport failure.
/// Test: covered by `memory_unreachable_is_fail`.
async fn http_get_ok(url: &str) -> anyhow::Result<bool> {
    let client = reqwest::Client::builder().timeout(PROBE_TIMEOUT).build()?;
    let resp = client.get(url).send().await?;
    Ok(resp.status().is_success())
}

/// Issue a timeout-bounded `GET` and parse the response body as JSON.
///
/// Why: the search index check needs the `/indexes` payload, not just its
/// status code.
/// What: `GET url` with a [`PROBE_TIMEOUT`] client timeout; returns the parsed
/// [`serde_json::Value`], `Err` on a non-2xx status or a transport / parse
/// failure.
/// Test: covered by `search_unreachable_is_fail`.
async fn http_get_json(url: &str) -> anyhow::Result<serde_json::Value> {
    let client = reqwest::Client::builder().timeout(PROBE_TIMEOUT).build()?;
    let body = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(body)
}

#[cfg(test)]
#[path = "doctor_tests.rs"]
mod tests;
