//! Temp git repos and host files for the S2b loader tests (#8454).
//!
//! Why: the gate reads real git state, so each test builds its own repos.
//! They never depend on the checkout the suite runs in: EVO runs from a
//! detached checkout with no local branches, and forces TMPDIR.
//! What: [`git`] runs with every `GIT_*` variable cleared and a fixed
//! identity; [`Repo`] is a repo made with `git init -b <branch>`; [`Home`]
//! is a temp home holding `.trusty-tools/trusty-mpm/config.yaml`.
//! Test: this module is test support.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::policy::{Channel, LoadRequest};

/// Run git in `dir`; panic on failure; return trimmed stdout.
pub(super) fn git(dir: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir).args([
        "-c",
        "user.name=test",
        "-c",
        "user.email=test@example.com",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "core.hooksPath=/dev/null",
    ]);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(key);
        }
    }
    let out = cmd.args(args).output().expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A canonical temp directory (macOS puts tempdirs under a symlink).
pub(super) fn tempdir() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let real = std::fs::canonicalize(tmp.path()).expect("canonical tempdir");
    (tmp, real)
}

/// A project repo in its own temp directory.
pub(super) struct Repo {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
}

impl Repo {
    /// `git init -b <branch>` with one empty root commit.
    pub(super) fn init(branch: &str) -> Self {
        let (tmp, root) = tempdir();
        let dir = root.join("proj");
        std::fs::create_dir(&dir).expect("mkdir");
        git(&dir, &["init", "-q", "-b", branch]);
        git(&dir, &["commit", "-q", "--allow-empty", "-m", "root"]);
        Self { _tmp: tmp, dir }
    }

    /// The repo's top level.
    pub(super) fn dir(&self) -> &Path {
        &self.dir
    }

    /// `.trusty-channels/routes.toml`.
    pub(super) fn routes_file(&self) -> PathBuf {
        self.dir.join(".trusty-channels/routes.toml")
    }

    /// Write the routes file, untracked or modified.
    pub(super) fn write_routes(&self, text: &str) {
        std::fs::create_dir_all(self.dir.join(".trusty-channels")).expect("mkdir");
        std::fs::write(self.routes_file(), text).expect("write routes");
    }

    /// Write, stage and commit the routes file on the current branch.
    pub(super) fn commit_routes(&self, text: &str) {
        self.write_routes(text);
        self.git(&["add", ".trusty-channels/routes.toml"]);
        self.git(&["commit", "-q", "-m", "routes"]);
    }

    /// Run git in this repo.
    pub(super) fn git(&self, args: &[&str]) -> String {
        git(&self.dir, args)
    }
}

/// A temp home with trusty-mpm's config.yaml.
pub(super) struct Home {
    _tmp: tempfile::TempDir,
    pub(super) home: PathBuf,
}

impl Home {
    /// A home whose config.yaml enables Slack and Telegram for `dirs`.
    pub(super) fn listing(dirs: &[&Path]) -> Self {
        let (tmp, home) = tempdir();
        let me = Self { _tmp: tmp, home };
        me.write_host(&host_yaml(dirs));
        me
    }

    /// `~/.trusty-tools/trusty-mpm/config.yaml`.
    pub(super) fn host_path(&self) -> PathBuf {
        self.home.join(".trusty-tools/trusty-mpm/config.yaml")
    }

    /// Replace config.yaml.
    pub(super) fn write_host(&self, yaml: &str) {
        let path = self.host_path();
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, yaml).expect("write host");
    }

    /// The daemon's request: Slack and Telegram, every listed project.
    pub(super) fn request(&self) -> LoadRequest {
        LoadRequest {
            host_path: self.host_path(),
            home: Some(self.home.clone()),
            project: None,
            channels: vec![Channel::Slack, Channel::Telegram],
        }
    }
}

/// A ceiling enabling every channel for `dirs`.
pub(super) fn host_yaml(dirs: &[&Path]) -> String {
    let list = dirs
        .iter()
        .map(|d| format!("\"{}\"", d.display()))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "channels:\n  version: 1\n  gchat:\n    enabled: true\n    projects: [{list}]\n  \
         slack:\n    enabled: true\n    projects: [{list}]\n  \
         telegram:\n    enabled: true\n    projects: [{list}]\n"
    )
}

/// A v2 file with one Slack route.
pub(super) fn slack_routes(name: &str, recipient: &str) -> String {
    format!(
        "version = 2\n\n[[slack.routes]]\nname = \"{name}\"\nrecipient = \"{recipient}\"\nkinds = [\"question\"]\n"
    )
}
