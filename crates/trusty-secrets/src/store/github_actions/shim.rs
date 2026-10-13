//! [`GhShim`]: a fake `gh` for the GitHub Actions sync-target tests (#9069).
//!
//! What: a POSIX shell script in its own temp dir, installed by absolute
//! path — never found through `PATH`, and no test changes the process
//! environment. A secret is a file `secrets/<NAME>` holding its
//! `updated_at`; each push advances a clock file by one second. Each call's
//! argv is a line in `calls.log`, its environment goes to `env.log`, and a
//! push's stdin to `stdin.<NAME>`. `gh api` of a missing secret prints
//! `gh: Not Found (HTTP 404)` on stderr and exits 1, as `gh` does.
//!
//! Scripted behaviour, set by a file in the shim's dir:
//! [`GhShim::locked`] fails every call with the signed-out text;
//! [`GhShim::noop`] makes a push exit 0 and store nothing;
//! [`GhShim::fail_push`] fails one name's push after echoing its stdin to
//! stderr; [`GhShim::echo`] echoes a push's stdin to stdout and stderr;
//! [`GhShim::fail_api_call`] fails the n-th `api` call; [`GhShim::slow`]
//! makes a push sleep after reading stdin.
//! Test: the tests that build one.

use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::{GitHubActionsBackend, GitHubActionsSettings};
use crate::api::{OwnerName, RepoName};
use crate::store::RemoteIdentity;
use crate::store::cli::test_shim::install_script;

/// The fake `gh`; `@DIR@` is the shim's directory.
const SCRIPT: &str = r##"L='@DIR@'
printf '%s\n' "$*" >> "$L/calls.log"
env >> "$L/env.log"
printf '%s\n' '--' >> "$L/env.log"
if [ -f "$L/locked" ]; then
  echo 'To get started with GitHub CLI, please run:  gh auth login' >&2
  exit 4
fi
case "$1" in
  api)
    n=$(grep -c '^api ' "$L/calls.log")
    if [ -f "$L/fail.api.$n" ]; then echo 'gh: Server Error (HTTP 502)' >&2; exit 1; fi
    name=${4##*/}
    if [ -f "$L/secrets/$name" ]; then
      printf '{"name":"%s","created_at":"2026-01-01T00:00:00Z","updated_at":"%s","visibility":"private"}' \
        "$name" "$(cat "$L/secrets/$name")"
      exit 0
    fi
    echo 'gh: Not Found (HTTP 404)' >&2
    exit 1
    ;;
  secret)
    name=$3
    cat > "$L/stdin.$name"
    if [ -f "$L/echo" ]; then cat "$L/stdin.$name"; cat "$L/stdin.$name" >&2; fi
    if [ -f "$L/slow" ]; then sleep 30; fi
    if [ -f "$L/fail.push.$name" ]; then
      cat "$L/stdin.$name" >&2
      echo 'failed to set secret: HTTP 422' >&2
      exit 1
    fi
    [ -f "$L/noop" ] && exit 0
    t=$(( $(cat "$L/clock") + 1 ))
    echo "$t" > "$L/clock"
    printf '2026-10-12T00:00:%02dZ' "$t" > "$L/secrets/$name"
    exit 0
    ;;
esac
echo "unexpected gh call: $1" >&2
exit 2
"##;

/// A fake `gh` in its own temp dir.
pub(crate) struct GhShim {
    dir: TempDir,
    program: PathBuf,
}

impl GhShim {
    /// A shim holding no secret, its clock at 10 s.
    pub(crate) fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let script = SCRIPT.replace("@DIR@", &dir.path().display().to_string());
        let program = install_script(&dir.path().join("bin"), "gh", &script);
        std::fs::create_dir(dir.path().join("secrets")).unwrap();
        std::fs::write(dir.path().join("clock"), "10\n").unwrap();
        Self { dir, program }
    }

    /// The executable `gh` this shim runs as.
    pub(crate) fn program(&self) -> &Path {
        &self.program
    }

    /// Settings running this shim with the default timeout.
    pub(crate) fn settings(&self) -> GitHubActionsSettings {
        GitHubActionsSettings::new(self.program.clone())
    }

    /// A backend over this shim pushing into `github.com/acme/web`.
    pub(crate) fn backend(&self) -> GitHubActionsBackend {
        self.backend_with(self.settings())
    }

    /// A backend over `settings` pushing into `github.com/acme/web`.
    pub(crate) fn backend_with(&self, settings: GitHubActionsSettings) -> GitHubActionsBackend {
        GitHubActionsBackend::new(settings, identity("acme", "web"))
    }

    fn touch(&self, name: &str) {
        std::fs::write(self.dir.path().join(name), "").unwrap();
    }

    /// Store `name` with `updated_at` as if pushed earlier.
    pub(crate) fn holds(&self, name: &str, updated_at: &str) {
        std::fs::write(self.dir.path().join("secrets").join(name), updated_at).unwrap();
    }

    /// Every call fails with the signed-out text.
    pub(crate) fn locked(&self) {
        self.touch("locked");
    }

    /// A push exits 0 and stores nothing.
    pub(crate) fn noop(&self) {
        self.touch("noop");
    }

    /// `name`'s push echoes its stdin to stderr and exits 1.
    pub(crate) fn fail_push(&self, name: &str) {
        self.touch(&format!("fail.push.{name}"));
    }

    /// A push echoes its stdin to stdout and stderr.
    pub(crate) fn echo(&self) {
        self.touch("echo");
    }

    /// The `n`-th `api` call (1-based) fails with a server error.
    pub(crate) fn fail_api_call(&self, n: usize) {
        self.touch(&format!("fail.api.{n}"));
    }

    /// A push sleeps 30 s after reading its stdin.
    pub(crate) fn slow(&self) {
        self.touch("slow");
    }

    fn read(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.dir.path().join(name)).ok()
    }

    /// Every call's argv, one line each; empty when nothing ran.
    pub(crate) fn calls(&self) -> String {
        self.read("calls.log").unwrap_or_default()
    }

    /// Every call's environment, `--` after each.
    pub(crate) fn env_log(&self) -> String {
        self.read("env.log").unwrap_or_default()
    }

    /// The stdin `name`'s push received, if it ran.
    pub(crate) fn stdin(&self, name: &str) -> Option<String> {
        self.read(&format!("stdin.{name}"))
    }

    /// `name`'s stored `updated_at`, if the shim holds it.
    pub(crate) fn updated_at(&self, name: &str) -> Option<String> {
        self.read(&format!("secrets/{name}"))
    }

    /// The shim's temp dir.
    pub(crate) fn dir(&self) -> &Path {
        self.dir.path()
    }
}

/// The identity `<owner>/<repo>`.
pub(crate) fn identity(owner: &str, repo: &str) -> RemoteIdentity {
    RemoteIdentity::new(OwnerName::new(owner).unwrap(), RepoName::new(repo).unwrap())
}
