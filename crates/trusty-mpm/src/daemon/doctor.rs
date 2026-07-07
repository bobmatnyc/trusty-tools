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
use doctor_output_style::check_output_style;

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

/// Per-probe network timeout.
///
/// Why: a sidecar that is down or wedged must not stall the whole diagnostic;
/// a short bound turns "hung" into a clean `Fail`.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

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
/// tm-skills-portfolio epic), and the `gh_account` probe
/// (#gh-account-awareness — surfaces the active github.com identity and warns
/// on the multi-account ambiguity) — folding the resulting ten
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
/// Issue #2149: `check_agents`/`check_skills` are ALSO scoped by
/// `project_dir` when it is supplied. A managed session (#1931) deploys its
/// roster under `<workspace>/.claude/{agents,skills}`, not the operator's
/// `$HOME/.claude` — probing only the home-tier directories previously let a
/// managed workspace with a completely empty roster report a false `agents`
/// `Ok` (because the operator's OWN `$HOME/.claude/agents/` was populated),
/// silently missing the exact provisioning gap this issue is about. With no
/// `project_dir` (the pre-existing CLI/standalone usage) this is unchanged —
/// [`FrameworkPaths::default`] still probes the home tier.
/// Test: `run_doctor_produces_ten_checks`,
/// `check_agents_scoped_to_managed_workspace_fails_on_empty_roster`.
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

    let mut checks = vec![
        check_instructions(project_dir),
        check_agents(&paths),
        check_skills(skills_root),
        check_skill_source(&paths),
        check_output_style(project_dir, &home),
        check_deployment_completeness(&paths),
    ];
    checks.push(check_memory(&home).await);
    checks.push(check_search(&home).await);
    checks.push(check_worktrees(repos_root, active_workspace_paths).await);
    checks.push(check_gh_account().await);

    DoctorReport::from_checks(checks)
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
/// `tm session prune --worktrees`. The filesystem walk is delegated to
/// [`crate::session_manager::prune::find_orphaned_worktrees`] inside
/// `spawn_blocking` so the async executor is not blocked by synchronous I/O.
/// What: builds a canonicalized active-path set, then spawns a blocking task
/// to walk `<repos_root>/<owner>/<repo>/.worktrees/`; any leaf directory not
/// in the active set is counted as an orphan. Reports `Ok` (no orphans),
/// `Warn` (orphans found), or `Ok` (repos_root absent / unconfigured).
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

    // Delegate the blocking filesystem walk to spawn_blocking so the async
    // executor is not stalled on directory I/O (#1840 F).
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
/// What: resolves the service address via [`discover_addr`], issues a
/// `GET /health` bounded by [`PROBE_TIMEOUT`], and reports `Ok` on a 2xx,
/// `Fail` on any other status or a transport error.
/// Test: `memory_unreachable_is_fail`.
async fn check_memory(home: &Path) -> DoctorCheck {
    let dir = home.join(".trusty-memory");
    let default = TRUSTY_MEMORY_DEFAULT_ADDR
        .parse()
        .expect("static default is valid");
    let env = std::env::var("TRUSTY_MEMORY_ADDR").ok();
    let addr = discover_addr(&dir, default, env.as_deref()).await;
    match http_get_ok(&format!("http://{addr}/health")).await {
        Ok(true) => DoctorCheck::new(
            "memory",
            CheckStatus::Ok,
            format!("trusty-memory healthy at {addr}"),
        ),
        Ok(false) => DoctorCheck::new(
            "memory",
            CheckStatus::Fail,
            format!("trusty-memory at {addr} returned a non-2xx status"),
        ),
        Err(e) => DoctorCheck::new(
            "memory",
            CheckStatus::Fail,
            format!("trusty-memory unreachable at {addr}: {e}"),
        ),
    }
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
mod tests {
    use super::*;

    #[test]
    fn index_present_matches_each_shape() {
        // Bare string array.
        let strings = serde_json::json!(["other", "trusty-mpm"]);
        assert!(index_present(&strings, "trusty-mpm"));
        // Objects with an `id` field, nested under `indexes`.
        let objects = serde_json::json!({"indexes": [{"id": "trusty-mpm"}]});
        assert!(index_present(&objects, "trusty-mpm"));
        // Objects with a `name` field.
        let named = serde_json::json!([{"name": "trusty-mpm"}]);
        assert!(index_present(&named, "trusty-mpm"));
        // Absent index.
        let missing = serde_json::json!(["a", "b"]);
        assert!(!index_present(&missing, "trusty-mpm"));
    }

    #[tokio::test]
    async fn memory_unreachable_is_fail() {
        // Port 0 never accepts a connection, so the probe must fail cleanly
        // rather than hang.
        unsafe {
            std::env::set_var("TRUSTY_MEMORY_ADDR", "127.0.0.1:0");
        }
        let tmp = tempfile::tempdir().unwrap();
        let check = check_memory(tmp.path()).await;
        unsafe {
            std::env::remove_var("TRUSTY_MEMORY_ADDR");
        }
        assert_eq!(check.status, CheckStatus::Fail);
    }

    #[tokio::test]
    async fn search_unreachable_is_fail() {
        unsafe {
            std::env::set_var("TRUSTY_SEARCH_ADDR", "127.0.0.1:0");
        }
        let tmp = tempfile::tempdir().unwrap();
        let check = check_search(tmp.path()).await;
        unsafe {
            std::env::remove_var("TRUSTY_SEARCH_ADDR");
        }
        assert_eq!(check.status, CheckStatus::Fail);
    }

    #[tokio::test]
    async fn agents_check_scopes_to_managed_workspace_when_project_given() {
        // Issue #2149: a managed session deploys its roster under
        // `<workspace>/.claude/agents/` (issue #1931), NOT the operator's
        // `$HOME/.claude/agents/`. Before this fix `run_doctor` always probed
        // `FrameworkPaths::default()` (the home tier) even when a `project_dir`
        // was supplied, so a managed workspace with a completely empty/broken
        // roster could report a false `agents` `Ok` purely because the
        // OPERATOR's own `$HOME/.claude/agents/` happened to be populated —
        // silently missing the exact provisioning gap this issue is about.
        // A fresh, empty `project_dir` with NO `.claude/agents/` at all must
        // now `Fail`, regardless of whatever the real test-runner's home
        // directory holds.
        let project = tempfile::tempdir().unwrap();
        let report = run_doctor(Some(project.path()), None, &[]).await;
        let agents_check = report
            .checks
            .iter()
            .find(|c| c.name == "agents")
            .expect("agents check present");
        assert_eq!(agents_check.status, CheckStatus::Fail);
        // The message must name the WORKSPACE-scoped path, not the operator's
        // home directory, proving the probe was scoped to `project_dir`.
        let expected_dir = project.path().join(".claude").join("agents");
        assert!(
            agents_check
                .message
                .contains(&expected_dir.display().to_string()),
            "message must name the workspace-scoped agents dir: {}",
            agents_check.message
        );
    }

    #[tokio::test]
    async fn agents_check_ok_when_managed_workspace_roster_populated() {
        // The positive counterpart: a project-scoped roster (with its manifest)
        // must report `Ok`, proving the probe reads the WORKSPACE tier rather
        // than always falling through to the home tier.
        let project = tempfile::tempdir().unwrap();
        let agents_dir = project.path().join(".claude").join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(agents_dir.join("engineer.md"), "agent").unwrap();
        std::fs::write(
            agents_dir.join(crate::core::agent_manifest::MANIFEST_FILE),
            "{}",
        )
        .unwrap();

        let report = run_doctor(Some(project.path()), None, &[]).await;
        let agents_check = report
            .checks
            .iter()
            .find(|c| c.name == "agents")
            .expect("agents check present");
        assert_eq!(agents_check.status, CheckStatus::Ok);
    }

    #[tokio::test]
    async fn run_doctor_produces_ten_checks() {
        // Issue #2158 adds the `deployment` probe, bringing the total from
        // nine to ten checks. #1905's stale-skill cleanup is deliberately NOT
        // a `run_doctor` probe — see the `run_doctor` doc comment.
        let report = run_doctor(None, None, &[]).await;
        assert_eq!(report.checks.len(), 10);
        let names: Vec<&str> = report.checks.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "instructions",
                "agents",
                "skills",
                "skill_source",
                "output_style",
                "deployment",
                "memory",
                "search",
                "worktrees",
                "gh_account"
            ]
        );
    }

    #[test]
    fn build_gh_account_check_single_ok() {
        // A single clear active account is healthy (Ok), naming the login.
        let check =
            build_gh_account_check(Some("bobmatnyc".to_string()), vec!["bobmatnyc".to_string()]);
        assert_eq!(check.status, CheckStatus::Ok);
        assert_eq!(check.name, "gh_account");
        assert!(check.message.contains("bobmatnyc"));
    }

    #[test]
    fn build_gh_account_check_multi_warn() {
        // Multiple logged-in accounts is the ambiguity that hid the admin-merge
        // bug: Warn, name both accounts, and point at `gh auth switch`.
        let check = build_gh_account_check(
            Some("bob-duetto".to_string()),
            vec!["bob-duetto".to_string(), "bobmatnyc".to_string()],
        );
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.message.contains("bob-duetto"));
        assert!(check.message.contains("bobmatnyc"));
        assert!(check.message.contains("gh auth switch"));
    }

    #[test]
    fn build_gh_account_check_unauthenticated_warn() {
        // No account at all: Warn (advisory), not Fail, pointing at `gh auth login`.
        let check = build_gh_account_check(None, Vec::new());
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.message.contains("not authenticated"));
        assert!(check.message.contains("gh auth login"));
    }

    #[test]
    fn gh_account_check_is_advisory_only() {
        // Every branch must be advisory — never a hard Fail.
        for check in [
            build_gh_account_check(Some("a".to_string()), vec!["a".to_string()]),
            build_gh_account_check(
                Some("a".to_string()),
                vec!["a".to_string(), "b".to_string()],
            ),
            build_gh_account_check(None, Vec::new()),
            build_gh_account_check(None, vec!["a".to_string()]),
        ] {
            assert_ne!(
                check.status,
                CheckStatus::Fail,
                "gh_account must never Fail"
            );
        }
    }

    #[tokio::test]
    async fn worktrees_no_repos_root_is_ok() {
        // Fix 7 (#1840): absence of a managed workspace root is NORMAL — not every
        // operator uses in-project worktree sessions. Return Ok, not Warn.
        let check = check_worktrees(None, &[]).await;
        assert_eq!(check.status, CheckStatus::Ok);
        assert!(check.message.contains("no managed workspace"));
    }

    #[tokio::test]
    async fn worktrees_no_orphans_is_ok() {
        // Why (#1845 item 2): on macOS /tmp is a symlink to /private/tmp. If we pass the
        // raw tempfile path as active (e.g. /tmp/…) but the walk canonicalises it to
        // /private/tmp/…, the active-set lookup misses and a live worktree is falsely
        // flagged as an orphan — causing a spurious CI failure. Canonicalize the active
        // path in the test so the set membership check in find_orphaned_worktrees matches.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Create owner/repo/.worktrees/session-abc/ and register it as active.
        let wt = root
            .join("owner")
            .join("repo")
            .join(".worktrees")
            .join("session-abc");
        std::fs::create_dir_all(&wt).unwrap();
        // Canonicalize so /tmp vs /private/tmp symlink differences are resolved.
        let canonical_wt = std::fs::canonicalize(&wt).unwrap_or(wt);
        let active = vec![canonical_wt];
        let check = check_worktrees(Some(root), &active).await;
        assert_eq!(check.status, CheckStatus::Ok);
    }

    #[tokio::test]
    async fn worktrees_with_orphan_is_warn() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Create two worktrees; only one has an active session.
        let wt1 = root
            .join("owner")
            .join("repo")
            .join(".worktrees")
            .join("session-live");
        let wt2 = root
            .join("owner")
            .join("repo")
            .join(".worktrees")
            .join("session-dead");
        std::fs::create_dir_all(&wt1).unwrap();
        std::fs::create_dir_all(&wt2).unwrap();
        // wt2 is orphaned (no active session).
        let active = vec![wt1];
        let check = check_worktrees(Some(root), &active).await;
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.message.contains("1 orphaned"));
        assert!(check.message.contains("tm session prune --worktrees"));
    }
}
