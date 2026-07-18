//! Per-session deployed-asset staleness detection (issue #2444).
//!
//! Why: `core::session_launch::prepare_session` deploys agents/skills into a
//! managed session's `.claude/{agents,skills}` exactly ONCE, at launch time
//! (DOC-34/#2002). A session that stays alive for days never re-syncs when the
//! bundled/catalog agent or skill source changes underneath it, so its deployed
//! copy silently drifts stale relative to what a fresh session would get — the
//! exact dogfood observation behind #2444 (a deployed skill still at v1.0.0
//! while the catalog had already moved to v1.1.0, and a missing agent added to
//! the catalog after launch). This module answers "is THIS session's deployed
//! workspace stale?" by re-targeting the SAME manifest/plan/checksum comparison
//! [`crate::core::update_check::detect_for_framework`] already performs for the
//! framework's own `~/.claude` deployment — no new staleness mechanism, just
//! pointed at a session's own workspace via
//! [`FrameworkPaths::for_managed_workspace`], mirroring how `doctor_staleness.rs`
//! (issue #2876) and `doctor_output_style.rs` (issue #2333) already reuse
//! existing manifest-checksum machinery rather than inventing new ones.
//! What: [`session_workdir`] resolves the on-disk directory a session's assets
//! deploy into (`workspace_path`, falling back to `cwd` for local-path/adopted
//! sessions); [`session_asset_staleness`] runs the full agent+skill comparison
//! and returns the [`StalenessReport`]; [`session_assets_stale`] is the boolean
//! summary `tm sessions ls`'s stale-assets marker and `tm sessions sync-assets`'s
//! pre-check consume. [`session_plan`] and [`session_asset_staleness_with_catalog`]
//! split the cheap per-session plan resolution from the expensive catalog-side
//! hash compute (issue #2444 review MEDIUM finding) so a batch caller —
//! `daemon::managed_routes::summary::checked_summaries`, computing this marker
//! for every session on every `tm sessions ls` — can group sessions by their
//! resolved `(agent_source, skill_source)` pair and share ONE
//! [`crate::core::update_check::CatalogHashes::compute`] across every session
//! using that same catalog, instead of recomposing the ~40+ catalog agents once
//! PER SESSION.
//! Test: `session_workdir_prefers_workspace_path`,
//! `session_workdir_falls_back_to_cwd`, `session_assets_stale_false_when_fresh`,
//! `session_assets_stale_true_when_catalog_moved`.

use std::path::Path;

use crate::core::manifest::HarnessPlan;
use crate::core::paths::FrameworkPaths;
use crate::core::update_check::{CatalogHashes, StalenessReport, detect_for_framework};
use crate::session_manager::SessionRecord;

/// Resolve the on-disk directory a session's deployed assets live under.
///
/// Why: [`crate::core::session_launch::prepare_session_with_repo_url`] deploys
/// into `workspace_path` for daemon-provisioned sessions, but a local-path /
/// adopted session (#1502/#1433) has no provisioned workspace at all — its
/// assets deploy into `cwd` instead. Every call site that needs "where do this
/// session's assets live" (staleness detection here, and the `sync-assets`
/// redeploy) must resolve identically, or they would silently disagree about
/// which directory is authoritative.
/// What: `workspace_path` when set, else `cwd`.
/// Test: `session_workdir_prefers_workspace_path`,
/// `session_workdir_falls_back_to_cwd`.
pub fn session_workdir(record: &SessionRecord) -> &Path {
    record
        .workspace_path
        .as_deref()
        .unwrap_or(record.cwd.as_path())
}

/// Compute the full staleness report for a session's deployed workspace.
///
/// Why: exposed separately from [`session_assets_stale`] so a future detail
/// surface (a `tm sessions info` staleness breakdown, or `tm doctor`) can show
/// WHICH agents/skills drifted rather than only whether any did.
/// What: builds a workspace-scoped [`FrameworkPaths`] via
/// [`FrameworkPaths::for_managed_workspace`] — deploy DESTINATIONS move to
/// `<workdir>/.claude/{agents,skills}`, while the bundled/catalog SOURCE paths
/// stay at the shared framework install, exactly as a real session deploy uses
/// them — and delegates to [`detect_for_framework`] with `project_dir = workdir`
/// so any project-level manifest override the session's own workspace carries
/// (e.g. `.trusty-mpm/manifest.toml`) is honoured exactly as launch would.
/// Test: `session_asset_staleness_flags_catalog_drift`,
/// `session_asset_staleness_ok_when_fresh`.
pub fn session_asset_staleness(record: &SessionRecord) -> StalenessReport {
    let workdir = session_workdir(record);
    let fw = FrameworkPaths::for_managed_workspace(workdir);
    detect_for_framework(&fw, workdir)
}

/// Resolve the workspace-scoped [`FrameworkPaths`] and [`HarnessPlan`] for a
/// session, WITHOUT doing any expensive catalog compose/hash work.
///
/// Why (issue #2444 review): a batch caller comparing many sessions needs to
/// know each session's resolved `(plan.agent_source, plan.skill_source)`
/// pair BEFORE deciding whether it can share an already-computed
/// [`CatalogHashes`] or must compute a new one — that decision has to happen
/// without paying the compose cost first. This is exactly the manifest-
/// resolve step [`session_asset_staleness`] and
/// `core::session_launch::sync_session_assets` already perform internally;
/// splitting it out here means a batch caller does not have to re-implement
/// (and risk diverging from) that resolution.
/// What: builds `FrameworkPaths::for_managed_workspace(session_workdir(record))`
/// and resolves its [`HarnessPlan`] exactly as [`session_asset_staleness`]
/// does — only file reads of small manifest/config files (project-level
/// `.trusty-mpm/manifest.toml` if present, the framework's own `config.toml`),
/// never a catalog directory scan or agent compose.
/// Test: `session_plan_resolves_default_bundled_source`.
pub fn session_plan(record: &SessionRecord) -> (FrameworkPaths, HarnessPlan) {
    let workdir = session_workdir(record);
    let fw = FrameworkPaths::for_managed_workspace(workdir);
    let catalog_root = crate::content::catalog_root_for(&fw.root);
    let sources = crate::core::manifest::ManifestSources::resolve(workdir, &fw.root, &catalog_root);
    let manifest = crate::core::manifest::resolve_manifest(&sources);
    let plan = HarnessPlan::from_manifest(&manifest, &fw, &catalog_root);
    (fw, plan)
}

/// Compute a session's staleness report reusing a precomputed [`CatalogHashes`]
/// for its resolved catalog source pair.
///
/// Why: the cheap, per-session half of the comparison a batch caller runs
/// AFTER grouping sessions by [`session_plan`]'s resolved `(agent_source,
/// skill_source)` pair and computing (or reusing) the matching
/// [`CatalogHashes`] — see the module doc for the full #2444 review
/// rationale. Reads only this session's OWN deployed manifest + on-disk
/// files; performs zero catalog I/O.
/// What: loads this session's deployed [`AgentManifest`]/[`SkillManifest`]
/// from `fw.claude_agents_dir()`/`fw.claude_skills_dir()` and delegates to
/// [`CatalogHashes::detect`] with `plan`'s selection predicates.
/// Test: `session_asset_staleness_with_catalog_matches_uncached_result`.
pub fn session_asset_staleness_with_catalog(
    fw: &FrameworkPaths,
    plan: &HarnessPlan,
    catalog: &CatalogHashes,
) -> StalenessReport {
    let agents_dir = fw.claude_agents_dir();
    let skills_dir = fw.claude_skills_dir();
    let deployed_agents = crate::core::agent_manifest::AgentManifest::load(&agents_dir);
    let deployed_skills = crate::core::skill_manifest::SkillManifest::load(&skills_dir);
    catalog.detect(
        &deployed_agents,
        &deployed_skills,
        &agents_dir,
        &skills_dir,
        |name| plan.agent_selected(name),
        |name| plan.skill_selected(name),
        |name| plan.agent_staleness_ignored(name),
    )
}

/// Whether a session's deployed agents/skills have drifted from the catalog.
///
/// Why: the boolean summary `tm sessions ls`'s stale-assets marker and the
/// `sync-assets` pre-check need. `unknown` (neither a bundled nor catalog
/// source tree could be found to compare against) is deliberately treated as
/// "not stale", matching [`StalenessReport`]'s own documented semantics —
/// nothing authoritative to compare against is not evidence of drift.
/// What: `session_asset_staleness(record).stale`.
/// Test: `session_assets_stale_false_when_fresh`,
/// `session_assets_stale_true_when_catalog_moved`.
pub fn session_assets_stale(record: &SessionRecord) -> bool {
    session_asset_staleness(record).stale
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent_deployer::deploy_agents_filtered;
    use crate::session_manager::{ManagedSessionId, ManagedSessionState};
    use chrono::Utc;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Build a minimal `SessionRecord` for a workspace-rooted session.
    fn make_record(workspace: Option<PathBuf>, cwd: PathBuf) -> SessionRecord {
        SessionRecord {
            id: ManagedSessionId::new(),
            tmux_name: "tm-test".to_string(),
            cwd,
            task: "test".to_string(),
            state: ManagedSessionState::Active,
            created_at: Utc::now(),
            last_activity_at: None,
            workspace_path: workspace,
            repo_url: None,
            branch: None,
            pending_decision: None,
            proposed_default: None,
            correlation: Default::default(),
            runtime: Default::default(),
            ephemeral: false,
            workspace_owned: false,
            source_id: None,
            claude_session_id: None,
            scrollback_path: None,
            last_cwd: None,
            deliverable_id: None,
            pane_id: None,
            injection_status: Default::default(),
        }
    }

    #[test]
    fn session_workdir_prefers_workspace_path() {
        let ws = PathBuf::from("/workspace");
        let record = make_record(Some(ws.clone()), PathBuf::from("/cwd"));
        assert_eq!(session_workdir(&record), ws.as_path());
    }

    #[test]
    fn session_workdir_falls_back_to_cwd() {
        let cwd = PathBuf::from("/cwd");
        let record = make_record(None, cwd.clone());
        assert_eq!(session_workdir(&record), cwd.as_path());
    }

    /// RAII guard restoring `$HOME` on drop (including panic) — mirrors the
    /// identical pattern in `core::standalone::load::tests::HomeGuard`.
    ///
    /// Why: [`session_asset_staleness`] resolves its bundled-source half via
    /// `FrameworkPaths::for_managed_workspace`, which always anchors the
    /// framework SOURCE tree at `FrameworkPaths::default().root` (the real
    /// `$HOME/.trusty-mpm` in production, by design — the daemon supports no
    /// `--root` override). A test exercising real staleness drift must
    /// therefore point `$HOME` at a throwaway tempdir so it reads/writes a
    /// fake framework tree, never the developer's real one.
    struct HomeGuard(Option<String>);
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            // SAFETY: paired with `#[serial_test::serial]` — no other thread
            // reads/writes the environment concurrently.
            match self.0 {
                Some(ref p) => unsafe { std::env::set_var("HOME", p) },
                None => unsafe { std::env::remove_var("HOME") },
            }
        }
    }

    /// Point `$HOME` at a fresh tempdir for the duration of the guard.
    fn fake_home() -> (TempDir, HomeGuard) {
        let home = TempDir::new().unwrap();
        let prior = std::env::var("HOME").ok();
        // SAFETY: serialized via `#[serial_test::serial]` on every caller.
        unsafe { std::env::set_var("HOME", home.path()) };
        (home, HomeGuard(prior))
    }

    #[test]
    #[serial_test::serial]
    fn session_asset_staleness_ok_when_fresh() {
        // Deploy a session's agents from the (fake-home) bundled source; a
        // freshly deployed workspace must not be reported stale.
        let (home, _guard) = fake_home();
        let fw = FrameworkPaths::default();
        let bundled = fw.agent_source_dir();
        std::fs::create_dir_all(&bundled).unwrap();
        std::fs::write(bundled.join("rust-engineer.md"), "v1 body").unwrap();

        let workspace = home.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let session_fw = FrameworkPaths::for_managed_workspace(&workspace);
        deploy_agents_filtered(&bundled, &session_fw.claude_agents_dir(), |_| true).unwrap();

        let record = make_record(Some(workspace.clone()), workspace);
        assert!(
            !session_assets_stale(&record),
            "freshly deployed session must not be stale"
        );
    }

    #[test]
    #[serial_test::serial]
    fn session_asset_staleness_flags_catalog_drift() {
        // The #2444 scenario: the bundled/catalog source changes AFTER the
        // session already deployed from it (a framework upgrade landing
        // mid-session) — the session's workspace must now report stale.
        let (home, _guard) = fake_home();
        let fw = FrameworkPaths::default();
        let bundled = fw.agent_source_dir();
        std::fs::create_dir_all(&bundled).unwrap();
        std::fs::write(bundled.join("rust-engineer.md"), "v1 body").unwrap();

        let workspace = home.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let session_fw = FrameworkPaths::for_managed_workspace(&workspace);
        deploy_agents_filtered(&bundled, &session_fw.claude_agents_dir(), |_| true).unwrap();

        let record = make_record(Some(workspace.clone()), workspace);
        assert!(!session_assets_stale(&record));

        // Framework upgrade changes the bundled source out from under it.
        std::fs::write(bundled.join("rust-engineer.md"), "v2 body — catalog moved").unwrap();
        assert!(
            session_assets_stale(&record),
            "session must be reported stale once the bundled source changed"
        );
    }

    #[test]
    #[serial_test::serial]
    fn session_plan_resolves_default_bundled_source() {
        // With no project-level manifest override, `session_plan`'s resolved
        // agent/skill source must equal the shared framework bundled dirs —
        // the property `checked_summaries`'s batch grouping (issue #2444
        // review) depends on to collapse multiple sessions onto ONE
        // `CatalogHashes::compute` call.
        let (home, _guard) = fake_home();
        let workspace = home.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let record = make_record(Some(workspace.clone()), workspace);

        let (fw, plan) = session_plan(&record);
        assert_eq!(plan.agent_source, fw.agent_source_dir());
        assert_eq!(plan.skill_source, fw.skill_source_dir());
    }

    #[test]
    #[serial_test::serial]
    fn session_asset_staleness_with_catalog_matches_uncached_result() {
        // The batched (`session_plan` + `CatalogHashes` + `session_asset_staleness_with_catalog`)
        // path must agree EXACTLY with the uncached `session_asset_staleness`
        // path — sharing the catalog compute must never change the answer.
        let (home, _guard) = fake_home();
        let fw = FrameworkPaths::default();
        let bundled = fw.agent_source_dir();
        std::fs::create_dir_all(&bundled).unwrap();
        std::fs::write(bundled.join("rust-engineer.md"), "v1 body").unwrap();

        let workspace = home.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let session_fw = FrameworkPaths::for_managed_workspace(&workspace);
        deploy_agents_filtered(&bundled, &session_fw.claude_agents_dir(), |_| true).unwrap();
        // Catalog drifts after deploy, so both paths must agree it is stale.
        std::fs::write(bundled.join("rust-engineer.md"), "v2 body").unwrap();

        let record = make_record(Some(workspace.clone()), workspace);
        let uncached = session_asset_staleness(&record);

        let (session_fw, plan) = session_plan(&record);
        let catalog = crate::core::update_check::CatalogHashes::compute(
            &plan.agent_source,
            &plan.skill_source,
        );
        let cached = session_asset_staleness_with_catalog(&session_fw, &plan, &catalog);

        assert_eq!(uncached.stale, cached.stale);
        assert!(
            cached.stale,
            "the catalog drift must be detected either way"
        );
    }
}
