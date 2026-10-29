//! [`VercelShim`]: a fake `vercel` for the Vercel sync-target tests (#9069
//! PR 2).
//!
//! What: a POSIX shell script in its own temp dir, installed by absolute
//! path — never found through `PATH`. A variable is a file
//! `vars/<target>/<NAME>` holding its value. Each call's argv is a line in
//! `calls.log`, its environment goes to `env.log` (`--` after each), and an
//! `env add`'s stdin to `stdin.<NAME>.<target>`. Like the real CLI, a call
//! links to the project named by `VERCEL_ORG_ID`/`VERCEL_PROJECT_ID` when
//! both are set, else by `.vercel/project.json` in its `--cwd` directory
//! (else its working directory); unlinked, it fails. `link.log` records the
//! link per call, `cwd.log` the `--cwd` directory's mode, entry count and
//! path, and `pwd.log` whether the real working directory held a
//! `.vercel/project.json`.
//!
//! Scripted behaviour, set by a file in the shim's dir:
//! [`VercelShim::locked`] fails every call with the signed-out text;
//! [`VercelShim::noop`] makes an `env add` exit 0 and store nothing;
//! [`VercelShim::fail_add`] fails one name's add on one target after
//! echoing its stdin to stderr; [`VercelShim::echo`] echoes an add's stdin
//! to stdout and stderr; [`VercelShim::slow`] makes an add on one target
//! sleep; [`VercelShim::short`] makes an add read one byte, close stdin,
//! store and exit 0; [`VercelShim::fail_ls_call`] fails the n-th `env ls`;
//! [`VercelShim::garbage_ls_call`] makes the n-th `env ls` exit 0 with text
//! that is not a listing.
//! Test: the tests that build one.

use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::{VercelBackend, VercelLink, VercelSettings};
use crate::store::cli::test_shim::install_script;

/// The mapping's team id; appears nowhere in a path or argv.
pub(crate) const ORG: &str = "team_acme9069";
/// The mapping's project id; appears nowhere in a path or argv.
pub(crate) const PROJECT: &str = "prj_web9069";

/// The fake `vercel`; `@DIR@` is the shim's directory.
const SCRIPT: &str = r##"L='@DIR@'
printf '%s\n' "$*" >> "$L/calls.log"
env >> "$L/env.log"
printf '%s\n' '--' >> "$L/env.log"
cwd=; prev=
for a in "$@"; do
  [ "$prev" = --cwd ] && cwd=$a
  prev=$a
done
if [ -n "$cwd" ]; then
  printf '%s %s %s\n' "$(ls -ld "$cwd" | cut -c1-10)" "$(ls -A "$cwd" | wc -l | tr -d ' ')" "$cwd" >> "$L/cwd.log"
  dir=$cwd
else
  echo none >> "$L/cwd.log"
  dir=$(pwd)
fi
if [ -f "$(pwd)/.vercel/project.json" ]; then echo has-link >> "$L/pwd.log"; else echo no-link >> "$L/pwd.log"; fi
if [ -n "$VERCEL_ORG_ID" ] && [ -n "$VERCEL_PROJECT_ID" ]; then
  link="env:$VERCEL_ORG_ID/$VERCEL_PROJECT_ID"
elif [ -f "$dir/.vercel/project.json" ]; then
  link="file:$(cat "$dir/.vercel/project.json")"
else
  link=none
fi
echo "$link" >> "$L/link.log"
if [ -f "$L/locked" ]; then
  echo 'Error: No existing credentials found. Please run `vercel login` or pass "--token"' >&2
  exit 1
fi
if [ "$link" = none ]; then
  echo "Error: Your codebase isn't linked to a project on Vercel." >&2
  exit 1
fi
case "$1 $2" in
  'env ls')
    n=$(grep -c '^env ls ' "$L/calls.log")
    if [ -f "$L/fail.ls.$n" ]; then echo 'Error: Internal Server Error' >&2; exit 1; fi
    if [ -f "$L/garbage.ls.$n" ]; then echo 'Upgrade available: run npm i -g vercel'; exit 0; fi
    target=$3
    echo 'Vercel CLI 99.0.0' >&2
    if [ -z "$(ls -A "$L/vars/$target")" ]; then
      echo '> No Environment Variables found for acme/web [12ms]'
      exit 0
    fi
    echo '> Environment Variables found for acme/web [12ms]' >&2
    echo ''
    echo ' name                value               environments        created'
    for f in "$L/vars/$target"/*; do
      printf ' %s  Encrypted  %s  1s ago\n' "${f##*/}" "$target"
    done
    exit 0
    ;;
  'env add')
    name=$3; target=$4
    if [ -f "$L/short" ]; then
      head -c 1 > /dev/null
      exec 0<&-
      echo x > "$L/vars/$target/$name"
      exit 0
    fi
    cat > "$L/stdin.$name.$target"
    if [ -f "$L/echo" ]; then cat "$L/stdin.$name.$target"; cat "$L/stdin.$name.$target" >&2; fi
    if [ -f "$L/slow.$target" ]; then sleep 30; fi
    if [ -f "$L/fail.add.$name.$target" ]; then
      cat "$L/stdin.$name.$target" >&2
      echo 'Error: Request failed with status 500' >&2
      exit 1
    fi
    [ -f "$L/noop" ] && exit 0
    if [ -f "$L/vars/$target/$name" ]; then
      case " $* " in
        *' --force '*) ;;
        *) echo "Error: The variable $name has already been added to all Environments" >&2; exit 1 ;;
      esac
    fi
    cp "$L/stdin.$name.$target" "$L/vars/$target/$name"
    exit 0
    ;;
esac
echo "unexpected vercel call: $1 $2" >&2
exit 2
"##;

/// A fake `vercel` in its own temp dir.
pub(crate) struct VercelShim {
    dir: TempDir,
    program: PathBuf,
}

impl VercelShim {
    /// A shim whose project holds no variable.
    pub(crate) fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let script = SCRIPT.replace("@DIR@", &dir.path().display().to_string());
        let program = install_script(&dir.path().join("bin"), "vercel", &script);
        for target in ["production", "preview", "development"] {
            std::fs::create_dir_all(dir.path().join("vars").join(target)).unwrap();
        }
        Self { dir, program }
    }

    /// The executable `vercel` this shim runs as.
    pub(crate) fn program(&self) -> &Path {
        &self.program
    }

    /// The root each push's empty working directory is made under.
    pub(crate) fn work_root(&self) -> PathBuf {
        self.dir.path().join("work")
    }

    /// Settings running this shim with the default timeout.
    pub(crate) fn settings(&self) -> VercelSettings {
        VercelSettings::new(self.program.clone(), self.work_root())
    }

    /// A backend over this shim pushing into [`ORG`]/[`PROJECT`].
    pub(crate) fn backend(&self) -> VercelBackend {
        self.backend_with(self.settings())
    }

    /// A backend over `settings` pushing into [`ORG`]/[`PROJECT`].
    pub(crate) fn backend_with(&self, settings: VercelSettings) -> VercelBackend {
        VercelBackend::new(settings, VercelLink::new(ORG, PROJECT).unwrap())
    }

    fn touch(&self, name: &str) {
        std::fs::write(self.dir.path().join(name), "").unwrap();
    }

    /// Store `name` = `value` on `target`, as if pushed earlier.
    pub(crate) fn holds(&self, target: &str, name: &str, value: &str) {
        std::fs::write(self.dir.path().join("vars").join(target).join(name), value).unwrap();
    }

    /// Every call fails with the signed-out text.
    pub(crate) fn locked(&self) {
        self.touch("locked");
    }

    /// An `env add` exits 0 and stores nothing.
    pub(crate) fn noop(&self) {
        self.touch("noop");
    }

    /// `name`'s add on `target` echoes its stdin to stderr and exits 1.
    pub(crate) fn fail_add(&self, name: &str, target: &str) {
        self.touch(&format!("fail.add.{name}.{target}"));
    }

    /// An add echoes its stdin to stdout and stderr.
    pub(crate) fn echo(&self) {
        self.touch("echo");
    }

    /// An add on `target` sleeps 30 s after reading its stdin.
    pub(crate) fn slow(&self, target: &str) {
        self.touch(&format!("slow.{target}"));
    }

    /// An add reads one byte, closes stdin, stores `x` and exits 0.
    pub(crate) fn short(&self) {
        self.touch("short");
    }

    /// The `n`-th `env ls` call (1-based) fails with a server error.
    pub(crate) fn fail_ls_call(&self, n: usize) {
        self.touch(&format!("fail.ls.{n}"));
    }

    /// The `n`-th `env ls` call (1-based) exits 0 with a non-listing.
    pub(crate) fn garbage_ls_call(&self, n: usize) {
        self.touch(&format!("garbage.ls.{n}"));
    }

    fn read(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.dir.path().join(name)).ok()
    }

    /// Every call's argv, one line each; empty when nothing ran.
    pub(crate) fn calls(&self) -> String {
        self.read("calls.log").unwrap_or_default()
    }

    /// Every call's argv with its trailing `--cwd <dir>` and later
    /// arguments cut off.
    pub(crate) fn commands(&self) -> Vec<String> {
        self.calls()
            .lines()
            .map(|line| line.split(" --cwd ").next().unwrap_or(line).to_string())
            .collect()
    }

    /// Every call's environment, `--` after each.
    pub(crate) fn env_log(&self) -> String {
        self.read("env.log").unwrap_or_default()
    }

    /// Every call's `--cwd` directory as `<mode> <entries> <path>`.
    pub(crate) fn cwd_log(&self) -> String {
        self.read("cwd.log").unwrap_or_default()
    }

    /// The project each call linked to.
    pub(crate) fn link_log(&self) -> String {
        self.read("link.log").unwrap_or_default()
    }

    /// Whether each call's real working directory held a project file.
    pub(crate) fn pwd_log(&self) -> String {
        self.read("pwd.log").unwrap_or_default()
    }

    /// The stdin `name`'s add on `target` received, if it ran.
    pub(crate) fn stdin(&self, name: &str, target: &str) -> Option<String> {
        self.read(&format!("stdin.{name}.{target}"))
    }

    /// `name`'s stored value on `target`, if the project holds it.
    pub(crate) fn stored(&self, target: &str, name: &str) -> Option<String> {
        self.read(&format!("vars/{target}/{name}"))
    }

    /// The shim's temp dir.
    pub(crate) fn dir(&self) -> &Path {
        self.dir.path()
    }
}
