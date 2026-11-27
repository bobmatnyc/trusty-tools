//! `tm fleet status` after a real content update, then `sync-assets` (#11700).
//!
//! Why: #10596's unit test moved the shipped skill text in memory and
//! refreshed through `deploy_architect_skills`. Neither the content pin nor
//! the `tm fleet status` and `sync-assets` paths ran, so a `sync-assets` that
//! stopped redeploying the Architect skills would pass it.
//! What: packs the checkout's `content/` tree as two releases, the second with
//! every Architect skill changed, and pins each with the production
//! `bundle_cache::update`. The old pin is deployed the way a launch does
//! (`deploy_for_launch`), so only the redeploy is `sync-assets`'s: the
//! daemon's `--all` handler over a seeded session. `tm fleet status --json`
//! is the built binary. Scratch
//! `$HOME` and cwd outside any checkout, so the pinned bundle is the content;
//! offline, no live daemon, a private tmux server (`common`). Mounted in
//! `env_serial` because it rewrites `$HOME`, the path vars and the cwd.
//! Test: this file IS the test.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::State;
use axum::response::IntoResponse;
use trusty_common::install_paths::MPM_DIR_NAME;
use trusty_mpm::content::bundle_cache::{
    self, FetchError, Release, ReleaseSource, SIDECAR_SUFFIX, UpdateAction,
};
use trusty_mpm::core::paths::FrameworkPaths;
use trusty_mpm::core::project_skill_tier::architect_skills::{self, SHIPPED, SKILLS_DIR};
use trusty_mpm::daemon::managed_routes::sync_assets::sync_all_session_assets_route;
use trusty_mpm::daemon::state::DaemonState;
use trusty_mpm::session_manager::{
    ManagedSessionId, ManagedSessionState, SessionRecord, SessionStore,
};

use crate::common;

/// The release `tm fleet init` would have deployed; above the minimum (#11470).
const OLD: &str = "content-v900.0.0";
/// The release `tm content update` moves the pin to.
const NEW: &str = "content-v900.1.0";

/// Env vars that would move the config dir, the framework root or the
/// content off the scratch home.
const PATH_VARS: [&str; 6] = [
    "TRUSTY_MPM_CONFIG_DIR",
    "TRUSTY_MPM_ROOT",
    "TRUSTY_MPM_WORKSPACE_ROOT",
    "TRUSTY_MPM_REPOS_ROOT",
    "XDG_CONFIG_HOME",
    "CLAUDE_CONFIG_DIR",
];

/// Points `HOME` and the cwd at scratch dirs, clears [`PATH_VARS`], sets the
/// content-offline flag; restores all on drop.
struct ProcessGuard {
    saved: Vec<(&'static str, Option<OsString>)>,
    cwd: PathBuf,
}

impl ProcessGuard {
    fn install(home: &Path, cwd: &Path) -> Self {
        let offline = trusty_mpm::content::first_use::OFFLINE_ENV;
        let saved = ["HOME", offline]
            .into_iter()
            .chain(PATH_VARS)
            .map(|k| (k, std::env::var_os(k)))
            .collect();
        let prev = std::env::current_dir().expect("cwd");
        // SAFETY: `env_serial` runs one test at a time; Drop restores all.
        unsafe {
            std::env::set_var("HOME", home);
            std::env::set_var(offline, "1");
            for k in PATH_VARS {
                std::env::remove_var(k);
            }
        }
        std::env::set_current_dir(cwd).expect("cwd into the scratch dir");
        Self { saved, cwd: prev }
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.cwd);
        // SAFETY: as in `install`.
        unsafe {
            for (k, v) in self.saved.drain(..) {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
}

/// An in-memory release host: each published tag carries its bundle and the
/// sha256 sidecar, as `content-release.yml` publishes them.
#[derive(Default)]
struct LocalReleases {
    assets: HashMap<String, Vec<u8>>,
}

impl LocalReleases {
    fn publish(&mut self, tag: &str, bundle: Vec<u8>) {
        let sidecar = format!(
            "{}  {tag}.tar.gz\n",
            trusty_common::integrity::Sha256Digest::of_bytes(&bundle)
        );
        self.assets.insert(
            format!("{tag}/{tag}.tar.gz{SIDECAR_SUFFIX}"),
            sidecar.into(),
        );
        self.assets.insert(format!("{tag}/{tag}.tar.gz"), bundle);
    }
}

impl ReleaseSource for LocalReleases {
    fn asset_url(&self, tag: &str, file: &str) -> String {
        format!("local://{tag}/{file}")
    }

    fn asset(&self, tag: &str, file: &str, _max: u64) -> Result<Option<Vec<u8>>, FetchError> {
        Ok(self.assets.get(&format!("{tag}/{file}")).cloned())
    }

    fn content_tags(&self) -> Result<Vec<String>, FetchError> {
        let mut tags: Vec<String> = self
            .assets
            .keys()
            .filter_map(|k| k.split('/').next().map(str::to_owned))
            .collect();
        tags.sort();
        tags.dedup();
        Ok(tags)
    }

    fn release(&self, tag: &str) -> Result<Release, FetchError> {
        Ok(Release {
            tag: tag.to_owned(),
            draft: false,
            prerelease: false,
        })
    }
}

/// The scratch home, the Architect project and the daemon root.
struct Fixture {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
    daemon_root: PathBuf,
    cwd: PathBuf,
}

impl Fixture {
    /// An allow-listed project that requests the supervisor profile.
    fn new() -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("tm-test-11700-")
            .tempdir_in("/tmp")
            .expect("scratch dir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonical scratch dir");
        let [home, project, daemon_root, cwd] =
            ["home", "architect", "daemon", "cwd"].map(|d| root.join(d));
        for dir in [&home, &project, &daemon_root, &cwd] {
            std::fs::create_dir_all(dir).expect("scratch subdir");
        }
        std::fs::create_dir_all(home.join(MPM_DIR_NAME)).expect("framework root");
        std::fs::write(
            home.join(MPM_DIR_NAME).join("config.toml"),
            format!(
                "[supervisor]\nprojects = [{:?}]\n",
                project.display().to_string()
            ),
        )
        .expect("allow-list the project");
        std::fs::write(
            project.join(".trusty-mpm.toml"),
            "profile = \"supervisor\"\n",
        )
        .expect("request the supervisor profile");
        Self {
            _tmp: tmp,
            home,
            project,
            daemon_root,
            cwd,
        }
    }

    fn cache(&self) -> PathBuf {
        self.home.join(MPM_DIR_NAME).join("content")
    }

    /// Seed one `Active` managed session whose workspace is the project.
    async fn seed_session(&self) {
        let record: SessionRecord = serde_json::from_value(serde_json::json!({
            "id": ManagedSessionId::new(),
            "tmux_name": "tm-architect",
            "cwd": self.project,
            "workspace_path": self.project,
            "task": "the Architect",
            "state": ManagedSessionState::Active,
            "created_at": "2026-11-27T00:00:00Z",
        }))
        .expect("a managed session record");
        let mut store = SessionStore::load(&self.daemon_root.join("session-manager"))
            .await
            .expect("load the scratch session store");
        store.upsert(record).await.expect("seed the session");
    }

    /// `tm sessions sync-assets --all`, the daemon side: the route's handler.
    async fn sync_assets_all(&self) -> serde_json::Value {
        let state =
            Arc::new(DaemonState::with_root_isolated_managed(self.daemon_root.clone()).await);
        let resp = sync_all_session_assets_route(State(state))
            .await
            .into_response();
        assert!(resp.status().is_success(), "{}", resp.status());
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("sync-assets body");
        let report: serde_json::Value = serde_json::from_slice(&body).expect("sync-assets JSON");
        assert_eq!(report["errors"], serde_json::json!([]), "{report}");
        assert_eq!(
            report["synced"].as_array().map(Vec::len),
            Some(1),
            "{report}"
        );
        report
    }

    /// The `skills` row of `tm fleet status --json --dir <project>`.
    fn status_skills(&self) -> (bool, String) {
        let out = common::tm_command_in(&self.home)
            .args(["fleet", "status", "--json", "--dir"])
            .arg(&self.project)
            .current_dir(&self.cwd)
            .env_remove("TMUX_SOCKET")
            .output()
            .expect("run tm fleet status");
        let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "{e}: {}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        let skills = &report["skills"];
        (
            skills["ok"].as_bool().expect("skills.ok"),
            skills["detail"].as_str().expect("skills.detail").to_owned(),
        )
    }
}

/// Whether `path` is one of the Architect's shipped skill sources.
fn is_architect_skill(path: &str) -> bool {
    path.strip_prefix("instructions/")
        .and_then(|rel| rel.strip_prefix(SKILLS_DIR))
        .is_some_and(|rest| rest.ends_with(".md"))
}

/// #11700: a real content update leaves the Architect skills stale in `tm
/// fleet status` until `sync-assets` redeploys them; then they read current.
#[tokio::test(flavor = "multi_thread")]
async fn fleet_status_reads_a_content_update_stale_until_sync_assets_redeploys_11700() {
    let fx = Fixture::new();
    // Packed before the cwd moves: the checkout is found from it.
    let old = common::repo_content_bundle(OLD, |_, _| {});
    let new = common::repo_content_bundle(NEW, |path, bytes| {
        if is_architect_skill(path) {
            bytes.extend_from_slice(format!("\n<!-- {NEW} -->\n").as_bytes());
        }
    });
    assert_ne!(old.len(), new.len(), "the new release moves the skill text");
    let _guard = ProcessGuard::install(&fx.home, &fx.cwd);
    assert!(
        trusty_common::content::find_dev_checkout(&fx.cwd).is_none()
            && trusty_common::content::find_dev_checkout(&fx.project).is_none(),
        "precondition: no checkout encloses the cwd or the project"
    );
    let mut releases = LocalReleases::default();
    releases.publish(OLD, old);
    let pinned = bundle_cache::update(&fx.cache(), &releases, None).expect("pin the old release");
    assert_eq!(
        (pinned.tag.as_str(), pinned.action),
        (OLD, UpdateAction::Installed)
    );
    fx.seed_session().await;

    // The deploy the earlier launch made, so only the redeploy below is
    // `sync-assets`'s: every skill current at the old pin.
    let launch = architect_skills::deploy_for_launch(&FrameworkPaths::default().root, &fx.project)
        .expect("an Architect launch deploys its skills");
    assert_eq!(
        (launch.written.len(), launch.errors.len()),
        (SHIPPED.len(), 0),
        "{launch:?}"
    );
    let current = format!("{} Architect skills current", SHIPPED.len());
    assert_eq!(fx.status_skills(), (true, current.clone()));

    // `tm content update`: the pin moves to the new release.
    releases.publish(NEW, new);
    let pinned = bundle_cache::update(&fx.cache(), &releases, None).expect("pin the new release");
    assert_eq!(
        (pinned.tag.as_str(), pinned.action),
        (NEW, UpdateAction::Installed)
    );
    let (ok, detail) = fx.status_skills();
    assert!(!ok, "a moved pin must read stale: {detail}");
    for skill in SHIPPED {
        let stale = format!(
            "{}: stale (the next Architect launch refreshes it)",
            skill.stem
        );
        assert!(detail.contains(&stale), "{stale} not in: {detail}");
    }

    // `tm sessions sync-assets --all` redeploys them without a relaunch.
    fx.sync_assets_all().await;
    assert_eq!(fx.status_skills(), (true, current));
}
