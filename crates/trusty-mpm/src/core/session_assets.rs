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
//! pre-check consume.
//! Test: `session_workdir_prefers_workspace_path`,
//! `session_workdir_falls_back_to_cwd`, `session_assets_stale_false_when_fresh`,
//! `session_assets_stale_true_when_catalog_moved`.

use std::path::Path;

use crate::core::paths::FrameworkPaths;
use crate::core::update_check::{StalenessReport, detect_for_framework};
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
}
