//! In-project spawn path: shared base clone + per-session worktrees (#1706, #1803, #2032).
//!
//! Why: operators who run `tm` from inside a git repository should get a managed
//! session against a git-worktree slice of that repo rather than a full clone.
//! This module implements the "in-project" path: (a) a durable PROTECTED base
//! clone under `~/trusty-mpm-projects/<owner>/<repo>/` that is always on the
//! default branch and owned by the daemon, and (b) a per-session git worktree
//! at `<base>/.worktrees/<worktree_name>` branched off that clone so each
//! session works in isolation. Since #2032 `worktree_name` is the SEMANTIC
//! tmux session name (e.g. `tm-trusty-tools-01`), not the raw session UUID —
//! resolving that name requires the async `SessionManager`, which this module
//! deliberately does not depend on, so worktree CREATION is split from
//! DETECTION (see [`try_inproject_spawn`] vs [`create_session_worktree`]).
//! Existing UUID-named worktrees are left as-is; only NEW sessions get the
//! semantic name.
//! What: [`repos_root`] resolves the configurable root; [`base_clone_path`]
//! computes the per-project clone directory; [`ensure_base_clone`] clones if
//! absent; [`try_inproject_spawn`] is the main entry point called from the
//! lifecycle layer — it detects the GitHub identity and ensures the base
//! clone, returning `(base_path, owner, repo)`; [`create_session_worktree`]
//! adds a per-session worktree at `<base>/.worktrees/<worktree_name>` given a
//! CALLER-RESOLVED name; [`worktree_name_collides`] lets the caller steer name
//! resolution away from an existing worktree dir/branch;
//! [`ensure_worktrees_gitignored`] keeps the worktree directory out of
//! `git status`; [`get_origin_url`] reads the remote.origin.url from a git
//! directory. The sibling [`untracked_sync`] module (#2196) syncs an
//! allowlist of untracked/secret files (e.g. `.env*`) from the operator's
//! live checkout into a freshly-created session worktree; it is called from
//! `lifecycle::reserve_inproject_worktree`, not from this module directly,
//! since only that call site holds both the live-checkout source path and
//! the resolved worktree destination.
//! Test: `try_inproject_spawn_returns_none_for_non_git_path` unit test covers the
//! non-git early exit; integration coverage via `tests/local_spawn.rs`.

use std::path::{Path, PathBuf};

use tracing::{info, warn};

pub mod untracked_sync;

/// Environment variable that overrides the managed repos root.
///
/// Why: operators need an escape hatch (tests, non-standard layouts) that wins
/// over config without touching the config file.
/// What: `"TRUSTY_MPM_REPOS_ROOT"`.
/// Test: indirectly by integration tests that set this env var.
pub const REPOS_ROOT_ENV: &str = "TRUSTY_MPM_REPOS_ROOT";

/// Default repos root directory name under `$HOME`.
///
/// Why: mirrors the canonical trusty-mpm-projects directory that `workspace_root()`
/// also resolves to, so both the guided-default path and `tm launch` converge on
/// the same base clone location (#1803).
/// What: `"trusty-mpm-projects"`.
/// Test: `repos_root_default_ends_with_canonical_segment`.
pub const DEFAULT_REPOS_DIR: &str = "trusty-mpm-projects";

/// Resolve the absolute root for base clones, with an optional direct override.
///
/// Why: tests that control the repos root must not mutate process-wide env vars
/// (which races other threads). Exposing the override as a parameter lets callers
/// supply the value directly without `set_var` / `remove_var`.
/// What: `env_override` wins over the `TRUSTY_MPM_REPOS_ROOT` env var, which
/// wins over `workspace_root()`. Falls back to `/tmp/trusty-mpm-projects` only
/// when the home directory is unresolvable AND nothing absolute was supplied.
/// Test: `repos_root_default_ends_with_canonical_segment`; `base_clone_path_respects_repos_root_override` (integration).
pub fn repos_root_from(env_override: Option<&str>) -> PathBuf {
    let home = dirs::home_dir();

    // Direct in-process override (highest priority — used by tests to avoid env mutation).
    if let Some(raw) = env_override {
        let raw = raw.trim();
        if !raw.is_empty() {
            return match &home {
                Some(h) => expand_tilde_repos(raw, h),
                None => PathBuf::from(raw),
            };
        }
    }

    // TRUSTY_MPM_REPOS_ROOT env override (backward compat, wins over workspace_root).
    if let Ok(raw) = std::env::var(REPOS_ROOT_ENV) {
        let raw = raw.trim();
        if !raw.is_empty() {
            return match &home {
                Some(h) => expand_tilde_repos(raw, h),
                None => PathBuf::from(raw),
            };
        }
    }

    // Delegate to the canonical workspace root resolver (TRUSTY_MPM_WORKSPACE_ROOT
    // > config template > ~/trusty-mpm-projects).
    let config = crate::core::trusty_tools_config::TrustyToolsConfig::load();
    crate::core::trusty_tools_config::workspace_root(&config)
}

/// Resolve the absolute root for base clones.
///
/// Why: the base-clone path needs ONE answer for "where do managed base clones
/// live?", with precedence TRUSTY_MPM_REPOS_ROOT env > TRUSTY_MPM_WORKSPACE_ROOT
/// env > config template > built-in `~/trusty-mpm-projects` (#1803).
/// What: delegates to [`repos_root_from`] with no in-process override, reading
/// `TRUSTY_MPM_REPOS_ROOT` from the environment as usual.
/// Falls back to `/tmp/trusty-mpm-projects` only when the home directory is
/// unresolvable AND nothing absolute was supplied.
/// Test: `repos_root_default_ends_with_canonical_segment`.
pub fn repos_root() -> PathBuf {
    repos_root_from(None)
}

/// Expand a leading `~` in a path string to `home`.
///
/// Why: env values are often home-relative; normalizing them once here keeps
/// caller code free of `~` handling.
/// What: replaces a leading `~/` or bare `~` with `home`; other paths pass through.
/// Test: covered indirectly by `repos_root` env-override tests.
fn expand_tilde_repos(template: &str, home: &Path) -> PathBuf {
    if let Some(rest) = template.strip_prefix("~/") {
        home.join(rest)
    } else if template == "~" {
        home.to_path_buf()
    } else {
        PathBuf::from(template)
    }
}

/// Compute the base clone path for a GitHub `owner/repo`.
///
/// Why: the base clone directory must be stable and deterministic so multiple
/// sessions against the same repo all share one protected clone.
/// What: returns `<repos_root>/<owner>/<repo>`.
/// Test: `base_clone_path_resolves_to_canonical_root` (unit); wiring via integration tests.
pub fn base_clone_path(owner: &str, repo: &str) -> PathBuf {
    repos_root().join(owner).join(repo)
}

/// Filename-suffix stem used when migrating a pre-#1803 old-layout dir aside.
///
/// Why: naming the backup suffix once keeps [`migrate_old_layout_aside`] and any
/// operator-facing docs/tests in agreement, and makes the moved directory
/// self-describing (`<repo>.old-layout-backup-<unix_ts>`).
/// What: `".old-layout-backup-"` — the timestamp is appended by the migrator.
/// Test: `ensure_base_clone_migrates_old_layout_dir_aside` asserts a sibling
/// directory whose name contains this stem is created.
pub const OLD_LAYOUT_BACKUP_SUFFIX: &str = ".old-layout-backup-";

/// Detect and migrate a pre-#1803 OLD-LAYOUT base directory out of the way (#1805).
///
/// Why: before #1803 the managed base directory `<repos_root>/<owner>/<repo>/` was
/// a plain container of per-session UUID full-clone subdirectories with NO
/// top-level `.git`. The new shared-base design wants to `git clone` INTO that same
/// path, but git refuses to clone into a non-empty directory — so the clone failed
/// and the daemon silently fell back to the legacy full-clone-per-session behaviour,
/// meaning migrated repos never got the `.worktrees/` layout. Moving the stale dir
/// aside (rather than erroring) upholds the project's "operator does nothing"
/// north-star: the fresh clone proceeds automatically and the old data is preserved
/// under a clearly-named backup the operator can inspect or delete at leisure.
/// What: acts ONLY when `base_path` exists, is a directory, is non-empty, and has
/// NO top-level `.git` (the precise old-layout signature). In that case it renames
/// `base_path` to a sibling `<repo><OLD_LAYOUT_BACKUP_SUFFIX><unix_nanos>` and
/// returns `Ok(Some(backup))`. Empty dirs, already-`.git` dirs, and absent paths
/// are left untouched (`Ok(None)`) — git clone handles an empty/absent dir fine and
/// the caller handles the `.git` case before ever calling this. Loudly `warn!`s so
/// the migration is never silent.
/// Test: `ensure_base_clone_migrates_old_layout_dir_aside`,
/// `migrate_old_layout_aside_ignores_empty_and_git_dirs`.
fn migrate_old_layout_aside(base_path: &Path) -> Result<Option<PathBuf>, String> {
    // Absent path or an existing `.git` dir are both non-old-layout: nothing to do.
    if !base_path.exists() || base_path.join(".git").exists() {
        return Ok(None);
    }

    // A non-directory sitting at the base path is not an old-layout container;
    // leave it for `git clone` to fail loudly rather than silently deleting a file.
    if !base_path.is_dir() {
        return Ok(None);
    }

    // Empty directory → `git clone` succeeds into it; no migration needed.
    let is_empty = std::fs::read_dir(base_path)
        .map_err(|e| {
            format!(
                "inproject: could not inspect base dir {} for old-layout migration: {e}",
                base_path.display()
            )
        })?
        .next()
        .is_none();
    if is_empty {
        return Ok(None);
    }

    // Non-empty, no top-level `.git` → this is the pre-#1803 old layout. Move it
    // aside so the fresh shared-base clone can be established at `base_path`.
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let leaf = base_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_string());
    let backup = base_path.with_file_name(format!("{leaf}{OLD_LAYOUT_BACKUP_SUFFIX}{ts}"));

    warn!(
        base = %base_path.display(),
        backup = %backup.display(),
        "inproject: detected pre-#1803 old-layout base dir (non-empty, no top-level \
         .git); migrating it aside so the new shared-base clone + .worktrees/ layout \
         can engage. The old per-session clones are preserved at the backup path — \
         inspect and delete them once you have confirmed nothing is needed."
    );

    std::fs::rename(base_path, &backup).map_err(|e| {
        format!(
            "inproject: failed to migrate old-layout dir {} aside to {}: {e}",
            base_path.display(),
            backup.display()
        )
    })?;

    warn!(
        backup = %backup.display(),
        "inproject: old-layout base dir migrated aside successfully"
    );
    Ok(Some(backup))
}

/// Ensure a base clone exists at `base_path`, cloning from `origin_url` if not.
///
/// Why: the first session against a repo triggers a one-time clone; subsequent
/// sessions reuse the same base directory and only add worktrees.
/// What: if `base_path/.git` exists, calls `ensure_worktrees_gitignored` and
/// returns `Ok(())` (idempotent). Otherwise it first migrates any pre-#1803
/// old-layout dir aside via [`migrate_old_layout_aside`] (#1805 — so a non-empty,
/// non-git directory no longer makes the clone fail and silently fall back to the
/// legacy full-clone-per-session path), then runs
/// `git clone --no-local <origin_url> <base_path>` and calls
/// `ensure_worktrees_gitignored`. A clone failure returns `Err` with the
/// command's stderr.
/// Test: idempotent path covered by unit tests; migration path by
/// `ensure_base_clone_migrates_old_layout_dir_aside`; clone path by integration tests.
pub fn ensure_base_clone(origin_url: &str, base_path: &Path) -> Result<(), String> {
    if base_path.join(".git").exists() {
        info!(
            path = %base_path.display(),
            "inproject: base clone already present, reusing"
        );
        ensure_worktrees_gitignored(base_path)?;
        return Ok(());
    }

    // #1805: a pre-existing old-layout dir (non-empty, no top-level `.git`) would
    // otherwise make `git clone` fail; migrate it aside first so the fresh clone
    // succeeds instead of silently falling back to full-clone-per-session.
    migrate_old_layout_aside(base_path)?;

    // Announce the clone stage (issue #1904, #1919). Placed here — after the
    // idempotent `.git`-exists reuse check above returns early — so the event
    // fires ONLY on an actual first-run clone, mirroring
    // `WorkspaceProvisioner::provision_in`'s placement (which always clones,
    // so it emits unconditionally). This is the exact "tm: first run for X —
    // cloning into ..., this may take a few minutes…" scenario #1904 set out
    // to make observable, and #1919 found it was never wired up here. No-op
    // unless the daemon's `spawn_managed` wrapped this call tree in
    // `provisioning_stage::scoped` (see that module's doc).
    crate::core::provisioning_stage::emit(
        crate::core::provisioning_stage::ProvisioningStage::CloningRepo,
    );

    info!(
        url = %origin_url,
        dest = %base_path.display(),
        "inproject: cloning base repo"
    );

    if let Some(parent) = base_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            format!(
                "inproject: failed to create parent dir {}: {e}",
                parent.display()
            )
        })?;
    }

    // Use the parent of base_path (just created above) as cwd so a deleted
    // inherited cwd cannot cause git to fail at startup with "fatal: Unable to
    // read current working directory" (exit 128) → HTTP 500 on managed-spawn.
    let cwd = base_path.parent().unwrap_or(std::path::Path::new("/"));
    let out = std::process::Command::new("git")
        .args(["clone", "--no-local", origin_url])
        .arg(base_path)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("inproject: git clone failed to spawn: {e}"))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "inproject: git clone failed ({}): {stderr}",
            out.status
        ));
    }
    info!(dest = %base_path.display(), "inproject: base clone complete");
    ensure_worktrees_gitignored(base_path)?;
    Ok(())
}

/// Compute the per-session worktree directory for `worktree_name` under `base_path`.
///
/// Why: both [`create_session_worktree`] and the caller-side collision check
/// ([`worktree_name_collides`], used by the in-project spawn routing layer to
/// steer `SessionManager::resolve_session_name` away from a name whose
/// worktree already exists on disk, #2032) must agree on exactly where a
/// worktree for a given name would live.
/// What: `<base_path>/.worktrees/<worktree_name>` (dot-prefixed so
/// `ensure_worktrees_gitignored` hides it from `git status`).
/// Test: `session_worktree_path_uses_dot_prefix`.
pub fn worktree_path_for(base_path: &Path, worktree_name: &str) -> PathBuf {
    base_path.join(".worktrees").join(worktree_name)
}

/// Compute the per-session branch name for `worktree_name`.
///
/// Why: re-exported here (rather than defined here) so existing callers in
/// this module keep the short `worktree_branch_for(...)` spelling. The single
/// source of truth lives in [`crate::core::worktree_naming`] — an
/// unconditionally-compiled module — specifically so
/// `session_manager::decommission` (which is NOT gated behind the `daemon`
/// feature) can depend on the SAME convention without pulling in this
/// feature-gated `daemon` module (#2032; see that module's doc for why the
/// dependency direction matters).
/// What: `format!("session/{worktree_name}")`.
/// Test: `crate::core::worktree_naming::tests::worktree_branch_for_adds_session_prefix`.
pub use crate::core::worktree_naming::worktree_branch_for;

/// Check whether a worktree directory or branch already exists for `worktree_name`.
///
/// Why (#2032): `tmux_name` collisions were already guarded by
/// `SessionManager::resolve_session_name`'s live-tmux-session check, but that
/// check knows nothing about git worktrees or branches — a DIFFERENT
/// collision surface. Without this, two names could theoretically differ in
/// tmux-liveness (one dead, one live) yet share the same worktree directory
/// once #2032 makes the worktree leaf equal to the tmux name, silently
/// clobbering (or failing to clobber, but confusingly erroring on) a stale
/// worktree left behind by a hand-deleted-but-not-decommissioned session.
/// This predicate is threaded into `resolve_session_name` as its
/// `extra_collision` callback so a colliding candidate is retried with the
/// next free serial BEFORE `create_session_worktree` ever runs, rather than
/// discovered as a raw `git worktree add`/`git branch` failure.
/// What: `true` if `<base_path>/.worktrees/<worktree_name>` exists on disk OR
/// `refs/heads/session/<worktree_name>` already exists in `base_path`'s repo.
/// Test: `worktree_name_collides_detects_existing_dir_and_branch`.
pub fn worktree_name_collides(base_path: &Path, worktree_name: &str) -> bool {
    if worktree_path_for(base_path, worktree_name).exists() {
        return true;
    }
    let branch_ref = format!("refs/heads/{}", worktree_branch_for(worktree_name));
    std::process::Command::new("git")
        .arg("-C")
        .arg(base_path)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(&branch_ref)
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Create a per-session git worktree branched off the base clone.
///
/// Why: each managed session must work in an isolated branch; git worktrees
/// achieve this without duplicating the object store of the base clone. Since
/// #2032 the caller supplies the ALREADY-RESOLVED semantic tmux name (e.g.
/// `tm-trusty-tools-01`) rather than the raw session UUID, so the worktree
/// directory and branch are human-readable and match the tmux session the
/// operator actually sees in `tm session ls`.
///
/// NOTE for #2033 (trusty-search index-id derivation): the worktree LEAF NAME
/// computed here (`worktree_name`) is significant beyond this function — the
/// trusty-search index id for a managed session is derived from the worktree
/// path, so #2033 must read the ACTUAL path this function returns (or the
/// record's `workspace_path`, which `spawn_managed_inproject` sets to this
/// same path) rather than re-deriving a path from the session id.
/// What: runs `git -C <base_path> worktree add -b session/<worktree_name>
/// <base_path>/.worktrees/<worktree_name>`. Returns the worktree path on
/// success. Fails loudly (rather than silently clobbering) if the target
/// worktree directory already exists — callers are expected to have already
/// steered `SessionManager::resolve_session_name` away from a colliding name
/// via [`worktree_name_collides`], so hitting this guard indicates a TOCTOU
/// race or a caller that skipped the collision check.
/// `owner_session_id` (#3649, Option B): the managed session this worktree is
/// being created FOR — written into the ownership sentinel (as a JSON
/// payload, replacing the pre-#3649 zero-byte convention) so the orphan-GC
/// sweep and the `decommission` owner gate can later resolve who is entitled
/// to reclaim this worktree.
/// Test: covered by integration tests against a real temp repo;
/// `create_session_worktree_rejects_existing_worktree_dir`,
/// `create_session_worktree_writes_owner_sentinel` (#3649).
pub fn create_session_worktree(
    base_path: &Path,
    worktree_name: &str,
    owner_session_id: &crate::session_manager::ManagedSessionId,
) -> Result<PathBuf, String> {
    let worktree_path = worktree_path_for(base_path, worktree_name);
    let branch = worktree_branch_for(worktree_name);

    if worktree_path.exists() {
        return Err(format!(
            "inproject: worktree path already exists: {} — refusing to clobber \
             (this should have been caught by the pre-reservation collision check)",
            worktree_path.display()
        ));
    }

    info!(
        base = %base_path.display(),
        worktree = %worktree_path.display(),
        branch = %branch,
        "inproject: creating per-session worktree"
    );

    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(base_path)
        .args(["worktree", "add", "-b"])
        .arg(&branch)
        .arg(&worktree_path)
        .output()
        .map_err(|e| format!("inproject: git worktree add failed to spawn: {e}"))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "inproject: git worktree add failed ({}): {stderr}",
            out.status
        ));
    }

    // #2189: give the new worktree a working `git pull` (upstream tracking the
    // default branch) WITHOUT letting `git push` target that branch. Best-effort
    // and non-fatal — see `configure_session_branch_tracking`.
    configure_session_branch_tracking(base_path, &worktree_path);

    // Item 5 (#1845; JSON payload added #3649): write the SM ownership sentinel
    // so remove_session_worktree can confirm this directory was TM-created and
    // not a user-owned path that happens to sit under a `.worktrees/` parent,
    // AND so the orphan-GC / `decommission` owner gate can resolve WHO owns it
    // (#3649, Option B) — the sentinel now carries a JSON payload naming
    // `owner_session_id`, rather than the pre-#3649 zero-byte convention. The
    // sentinel write is best-effort: a failure here is a non-fatal warning; the
    // worktree was created successfully and the naming-convention fallback in
    // decommission.rs provides backward-compat, while the tolerant sentinel
    // parser treats an unwritten sentinel identically to a legacy one
    // (owner-unknown), never as an error.
    let sentinel = worktree_path.join(crate::session_manager::decommission::WORKTREE_SENTINEL_FILE);
    if let Err(e) = std::fs::write(
        &sentinel,
        crate::session_manager::worktree_ownership::sentinel_payload_bytes(*owner_session_id),
    ) {
        warn!(
            worktree = %worktree_path.display(),
            sentinel = crate::session_manager::decommission::WORKTREE_SENTINEL_FILE,
            "inproject: failed to write SM ownership sentinel (non-fatal): {e}"
        );
    }

    info!(worktree = %worktree_path.display(), "inproject: per-session worktree created");
    Ok(worktree_path)
}

/// Give a freshly-created session worktree a working `git pull` from the
/// default branch, WITHOUT making `git push` target that branch (#2189).
///
/// Why: [`create_session_worktree`] leaves the new `session/<name>` branch
/// with no upstream at all, so a bare `git pull` inside the worktree fails
/// with "There is no tracking information for the current branch." Setting
/// the upstream to `origin/<default>` fixes `pull`/`fetch`-with-merge, but
/// naively doing so ALSO makes a bare `git push` try to push the session
/// branch's commits onto the shared default branch — which is exactly what a
/// per-session isolated branch must never do. Scoping `push.default =
/// current` to this one worktree (via `extensions.worktreeConfig` +
/// `--worktree` config) keeps push targeting `origin/session/<name>` while
/// pull still tracks the default branch.
/// What: (1) resolves the default branch via
/// [`super::inproject_hygiene::get_default_branch`], falling back to `"main"`
/// when it cannot be determined; (2) runs `git -C <worktree_path> branch
/// --set-upstream-to=origin/<default>` so `git pull` works; (3) runs `git -C
/// <base_path> config extensions.worktreeConfig true` to enable per-worktree
/// config on the shared base repo (idempotent — safe to set on every call);
/// (4) runs `git -C <worktree_path> config --worktree push.default current`
/// so `git push` always targets the current (session) branch, never the
/// default branch. Every step is best-effort: a failure is logged via `warn!`
/// and does NOT fail worktree creation — the worktree itself was already
/// created successfully by the time this runs, and an operator can always set
/// tracking manually as a fallback.
/// Test: `create_session_worktree_sets_pull_upstream_and_worktree_scoped_push`
/// (integration, real temp git repos with a bare `origin` remote).
fn configure_session_branch_tracking(base_path: &Path, worktree_path: &Path) {
    let default_branch = super::inproject_hygiene::get_default_branch(base_path)
        .unwrap_or_else(|| "main".to_string());

    // Step 1: set upstream so `git pull` (and `git fetch` + merge) works.
    let upstream = format!("origin/{default_branch}");
    let set_upstream = std::process::Command::new("git")
        .arg("-C")
        .arg(worktree_path)
        .args(["branch", &format!("--set-upstream-to={upstream}")])
        .output();
    match set_upstream {
        Ok(out) if out.status.success() => {
            info!(
                worktree = %worktree_path.display(),
                upstream = %upstream,
                "inproject: session worktree branch upstream set (git pull now works)"
            );
        }
        Ok(out) => {
            warn!(
                worktree = %worktree_path.display(),
                upstream = %upstream,
                "inproject: failed to set branch upstream (non-fatal; `git pull` will \
                 require manual tracking setup) ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Err(e) => {
            warn!(
                worktree = %worktree_path.display(),
                "inproject: git branch --set-upstream-to failed to spawn (non-fatal): {e}"
            );
        }
    }

    // Step 2: enable per-worktree config on the shared base repo (idempotent).
    let enable_worktree_config = std::process::Command::new("git")
        .arg("-C")
        .arg(base_path)
        .args(["config", "extensions.worktreeConfig", "true"])
        .output();
    match enable_worktree_config {
        Ok(out) if out.status.success() => {
            info!(base = %base_path.display(), "inproject: extensions.worktreeConfig enabled on base clone");
        }
        Ok(out) => {
            warn!(
                base = %base_path.display(),
                "inproject: git config extensions.worktreeConfig failed (non-fatal) ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Err(e) => {
            warn!(
                base = %base_path.display(),
                "inproject: failed to spawn git config extensions.worktreeConfig (non-fatal): {e}"
            );
        }
    }

    // Step 3: scope push.default=current to THIS worktree only, so `git push`
    // always targets `origin/session/<name>` and never the default branch.
    let set_push_default = std::process::Command::new("git")
        .arg("-C")
        .arg(worktree_path)
        .args(["config", "--worktree", "push.default", "current"])
        .output();
    match set_push_default {
        Ok(out) if out.status.success() => {
            info!(
                worktree = %worktree_path.display(),
                "inproject: worktree-scoped push.default=current set (git push cannot target the default branch)"
            );
        }
        Ok(out) => {
            warn!(
                worktree = %worktree_path.display(),
                "inproject: failed to set worktree-scoped push.default (non-fatal) ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Err(e) => {
            warn!(
                worktree = %worktree_path.display(),
                "inproject: git config --worktree push.default failed to spawn (non-fatal): {e}"
            );
        }
    }
}

/// Best-effort-idempotent: add `.worktrees/` to the base clone's `.git/info/exclude`.
///
/// Why: per-session worktrees live at `<base>/.worktrees/<session-id>/`; without
/// a gitignore entry `git status` inside the base clone reports the `.worktrees/`
/// directory as untracked, polluting the output for every harness that runs there.
/// Adding to `.git/info/exclude` (not `.gitignore`) keeps the entry local to the
/// clone and does not affect any committed `.gitignore`.
/// What: creates `<base>/.git/info/` if absent, then reads the current exclude
/// file and appends `.worktrees/` only when the entry is absent. The read-then-
/// append window means concurrent callers racing at first launch may each observe
/// an absent entry and both append — resulting in a duplicate line. Git tolerates
/// duplicate patterns (same set is matched), so this is safe; single-caller
/// invocations are strictly idempotent. Returns `Ok(())` on success or when the
/// entry already exists; propagates I/O errors.
/// Test: `ensure_worktrees_gitignored_idempotent`.
pub fn ensure_worktrees_gitignored(base_path: &Path) -> Result<(), String> {
    let info_dir = base_path.join(".git").join("info");
    std::fs::create_dir_all(&info_dir).map_err(|e| {
        format!(
            "inproject: could not create .git/info/ at {}: {e}",
            info_dir.display()
        )
    })?;

    let exclude_path = info_dir.join("exclude");
    let existing = std::fs::read_to_string(&exclude_path).unwrap_or_default();

    // Idempotent: skip if the entry is already present.
    if existing.lines().any(|line| line.trim() == ".worktrees/") {
        return Ok(());
    }

    // Append with a leading newline when the file does not already end with one.
    let entry = if existing.is_empty() || existing.ends_with('\n') {
        ".worktrees/\n".to_string()
    } else {
        "\n.worktrees/\n".to_string()
    };

    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&exclude_path)
        .map_err(|e| format!("inproject: could not open .git/info/exclude: {e}"))?;
    file.write_all(entry.as_bytes())
        .map_err(|e| format!("inproject: could not write .git/info/exclude: {e}"))?;

    info!(
        path = %exclude_path.display(),
        "inproject: added .worktrees/ to .git/info/exclude"
    );
    Ok(())
}

/// Read the `remote.origin.url` from a git repository at `path`.
///
/// Why: the in-project spawn path needs the remote URL to determine the
/// `owner/repo` identity and locate the matching base clone.
/// What: runs `git -C <path> config --get remote.origin.url` and returns the
/// trimmed stdout on success, or `None` if git fails or there is no remote origin.
/// Test: `get_origin_url_returns_none_for_non_git` (unit).
pub fn get_origin_url(path: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .ok()?;

    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if url.is_empty() { None } else { Some(url) }
}

/// Main entry point for the in-project spawn path's DETECTION phase (#1706, #2032).
///
/// Why: the lifecycle layer needs a single call that encapsulates in-project
/// detection: is the given path inside a git repo with a GitHub remote? If so,
/// ensure the base clone exists. Since #2032 this function STOPS SHORT of
/// creating the per-session worktree: the worktree directory/branch must be
/// named after the semantic tmux name (resolved via
/// `SessionManager::resolve_session_name`), which requires the session
/// manager's async store/tmux state that this synchronous, decoupled module
/// deliberately has no access to. The caller
/// (`daemon::managed_routes::lifecycle::spawn_managed_routed`) resolves the
/// name and calls [`create_session_worktree`] itself.
/// What: (1) checks that `path` is an existing directory with a `.git` entry;
/// (2) reads the `remote.origin.url` — returns `Ok(None)` if absent;
/// (3) parses the URL via `trusty_common::github_path::parse_github_path` —
/// returns `Ok(None)` if unparseable; (4) calls [`ensure_base_clone`];
/// (5) returns `Ok(Some((base_clone_path, owner, repo)))` — NOT a worktree
/// path (that was the pre-#2032 contract). Step 4 errors propagate as `Err`.
/// Test: `try_inproject_spawn_returns_none_for_non_git_path` (unit).
pub fn try_inproject_spawn(path: &Path) -> Result<Option<(PathBuf, String, String)>, String> {
    if !path.is_dir() || !path.join(".git").exists() {
        return Ok(None);
    }

    let Some(origin_url) = get_origin_url(path) else {
        warn!(
            path = %path.display(),
            "inproject: no remote.origin.url found; falling through to local-path spawn"
        );
        return Ok(None);
    };

    let Some(gh) = trusty_common::github_path::parse_github_path(&origin_url) else {
        warn!(
            url = %origin_url,
            "inproject: cannot parse GitHub owner/repo from remote URL; falling through"
        );
        return Ok(None);
    };

    let base = base_clone_path(&gh.owner, &gh.repo);
    ensure_base_clone(&origin_url, &base)?;

    info!(
        owner = %gh.owner,
        repo = %gh.repo,
        base = %base.display(),
        "inproject: base clone ready for per-session worktree"
    );

    Ok(Some((base, gh.owner, gh.repo)))
}

#[cfg(test)]
mod tests;
