//! [`OpShim`]: a fake `op` for the 1Password tests (#7519).
//!
//! What: a POSIX shell script in its own temp dir, run as `/bin/sh <script>`
//! with both named by absolute path — never found through `PATH`, and no
//! test changes any process-global environment. It keeps items as files
//! (`items/<id>`: title, value, category), appends each call's argv to
//! `calls.log`, its environment to `env.log`, a create's stdin to
//! `stdin.log` and an edit's template to `template.log`, and fails one
//! subcommand on demand. Every scripted failure first echoes the value it
//! holds — stdin to stderr for `create`, the template to stderr for
//! `edit`, every stored value to stdout for `read` — the worst case for A2.
//! Running through `/bin/sh` avoids `ETXTBSY` when another test thread
//! forks while the script is open for writing.
//!
//! #7519: a test that needs `op` found by path — on a `PATH` value or as a
//! machine `program` pin — uses [`OpShim::install_in`], or [`plant_op`]
//! for an `op` that must never run. Both go through [`install_op`], which
//! waits out `ETXTBSY` before returning.
//! Test: the tests that build one.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use tempfile::TempDir;

use super::{OnePasswordBackend, OnePasswordSettings};

/// The fake `op`; `@DIR@` is the shim's directory.
const SCRIPT: &str = r##"L='@DIR@'
printf '%s\n' "$*" >> "$L/calls.log"
env >> "$L/env.log"
printf '%s\n' '--' >> "$L/env.log"
while :; do
  case "$1" in
    --account|--config) shift 2 ;;
    *) break ;;
  esac
done
if [ -f "$L/headless" ] && [ "$OP_SERVICE_ACCOUNT_TOKEN" != "$(cat "$L/headless")" ]; then
  echo '[ERROR] 2026/10/07 12:00:00 You are not currently signed in. Please run `op signin --help` for instructions' >&2
  exit 1
fi
if [ "$1" = read ]; then name=read; else name=$2; fi
if [ -f "$L/fail.$name" ]; then
  case "$name" in
    create) cat >&2 ;;
    edit) for a in "$@"; do if [ -f "$a" ]; then cat "$a" >&2; fi; done ;;
    read) for f in "$L"/items/*; do if [ -f "$f" ]; then sed -n 2p "$f"; fi; done ;;
  esac
  cat "$L/fail.$name" >&2
  exit 1
fi
case "$name" in
  list)
    printf '['
    sep=''
    for f in "$L"/items/*; do
      [ -f "$f" ] || continue
      printf '%s{"id":"%s","title":"%s","version":1,"vault":{"id":"vault7519","name":"trusty/acme/web"},"category":"%s","additional_information":"ainfo"}' \
        "$sep" "${f##*/}" "$(sed -n 1p "$f")" "$(sed -n 3p "$f")"
      sep=','
    done
    printf ']\n' ;;
  create)
    tpl=$(cat)
    printf '%s\n' "$tpl" >> "$L/stdin.log"
    title=$(printf '%s' "$tpl" | sed -n 's/.*"title":"\([^"]*\)".*/\1/p')
    value=$(printf '%s' "$tpl" | sed -n 's/.*"value":"\([^"]*\)".*/\1/p')
    id="new$$"
    printf '%s\n%s\nPASSWORD\n' "$title" "$value" > "$L/items/$id"
    printf '{"id":"%s"}\n' "$id" ;;
  edit)
    id=$3
    tpl=''
    prev=''
    for a in "$@"; do
      if [ "$prev" = --template ]; then tpl=$a; fi
      prev=$a
    done
    if [ ! -f "$L/items/$id" ]; then
      echo "[ERROR] \"$id\" isn't an item in the \"trusty/acme/web\" vault" >&2
      exit 1
    fi
    cat "$tpl" >> "$L/template.log"
    printf '\n' >> "$L/template.log"
    value=$(sed -n 's/.*"value":"\([^"]*\)".*/\1/p' "$tpl")
    title=$(sed -n 1p "$L/items/$id")
    printf '%s\n%s\nPASSWORD\n' "$title" "$value" > "$L/items/$id" ;;
  delete)
    id=$3
    if [ ! -f "$L/items/$id" ]; then
      echo "[ERROR] \"$id\" isn't an item" >&2
      exit 1
    fi
    rm "$L/items/$id" ;;
  read)
    ref=$3
    rest=${ref#op://*/}
    id=${rest%%/*}
    if [ ! -f "$L/items/$id" ]; then
      echo "[ERROR] could not read secret '$ref': \"$id\" isn't an item" >&2
      exit 1
    fi
    printf '%s' "$(sed -n 2p "$L/items/$id")" ;;
  *)
    echo "shim: unknown command" >&2
    exit 2 ;;
esac
"##;

/// A fake `op` in its own temp dir.
pub(crate) struct OpShim {
    dir: TempDir,
    script: PathBuf,
}

impl OpShim {
    pub(crate) fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let script = dir.path().join("op.sh");
        let body = SCRIPT.replace("@DIR@", &dir.path().display().to_string());
        std::fs::write(&script, body).unwrap();
        std::fs::create_dir(dir.path().join("items")).unwrap();
        Self { dir, script }
    }

    /// Settings that run this shim, with templates under `template_root`.
    pub(crate) fn settings(&self, template_root: &Path) -> OnePasswordSettings {
        let mut settings = OnePasswordSettings::new(template_root.to_path_buf());
        settings.program = "/bin/sh".into();
        settings.leading_args = vec![self.script.clone().into_os_string()];
        settings.timeout = Duration::from_secs(20);
        settings
    }

    /// This shim as an executable `dir/op`, for a test that finds `op` by path.
    pub(crate) fn install_in(&self, dir: &Path) -> PathBuf {
        install_op(
            dir,
            &format!("exec /bin/sh '{}' \"$@\"", self.script.display()),
        )
    }

    /// A backend over this shim, with templates under `template_root`.
    pub(crate) fn backend(&self, template_root: &Path) -> OnePasswordBackend {
        OnePasswordBackend::new(self.settings(template_root))
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.path(name)).unwrap_or_default()
    }

    /// Store an item directly, as if made outside this backend.
    pub(crate) fn seed(&self, id: &str, title: &str, value: &str, category: &str) {
        let body = format!("{title}\n{value}\n{category}\n");
        std::fs::write(self.path("items").join(id), body).unwrap();
    }

    /// Make subcommand `op` (`list`, `create`, `edit`, `delete`, `read`)
    /// echo what it holds, print `stderr`, and exit 1.
    pub(crate) fn fail(&self, op: &str, stderr: &str) {
        std::fs::write(self.path(&format!("fail.{op}")), stderr).unwrap();
    }

    /// Answer "not currently signed in" unless the child's
    /// `OP_SERVICE_ACCOUNT_TOKEN` is `token`.
    pub(crate) fn headless(&self, token: &str) {
        std::fs::write(self.path("headless"), token).unwrap();
    }

    /// Each call's argv, one line per call, script path excluded.
    pub(crate) fn calls(&self) -> String {
        self.read("calls.log")
    }

    /// Each call's environment, `--` after each.
    pub(crate) fn env_log(&self) -> String {
        self.read("env.log")
    }

    /// Every create's stdin.
    pub(crate) fn stdin_log(&self) -> String {
        self.read("stdin.log")
    }

    /// Every edit's template file content.
    pub(crate) fn template_log(&self) -> String {
        self.read("template.log")
    }

    /// Whether the shim ever ran.
    pub(crate) fn spawned(&self) -> bool {
        self.path("calls.log").exists()
    }

    /// Every stored item as `(id, title, value)`, sorted by id.
    pub(crate) fn items(&self) -> Vec<(String, String, String)> {
        let mut items: Vec<_> = std::fs::read_dir(self.path("items"))
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let body = std::fs::read_to_string(entry.path()).unwrap();
                let mut lines = body.lines().map(str::to_string);
                let id = entry.file_name().to_string_lossy().into_owned();
                let title = lines.next().unwrap_or_default();
                (id, title, lines.next().unwrap_or_default())
            })
            .collect();
        items.sort();
        items
    }
}

/// The argument an [`install_op`] script exits 0 on before its body runs.
const PROBE: &str = "__probe7519__";

/// Write `dir/op`, a 0755 `/bin/sh` script running `body`, and return its
/// path.
///
/// Why: a thread that forks while the file is open for writing keeps that
/// descriptor until it execs, and an `exec` of the file meanwhile fails with
/// `ETXTBSY`.
/// What: after the write, runs the script with [`PROBE`] until `exec` no
/// longer fails busy. The descriptor is closed by then, so no later fork
/// can inherit it, and no later spawn of the script fails busy.
pub(crate) fn install_op(dir: &Path, body: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join("op");
    let script = format!("#!/bin/sh\n[ \"$1\" = {PROBE} ] && exit 0\n{body}\n");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    for _ in 0..500 {
        match Command::new(&path).arg(PROBE).status() {
            Ok(status) => {
                assert!(status.success(), "{}: {status}", path.display());
                return path;
            }
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("{}: {e}", path.display()),
        }
    }
    panic!("{} stayed busy", path.display());
}

/// A planted `dir/op` that creates `marker` if it ever runs.
pub(crate) fn plant_op(dir: &Path, marker: &Path) -> PathBuf {
    install_op(dir, &format!(": > '{}'\nexit 0", marker.display()))
}

/// `dir`, absolute, as a path relative to this process's working directory.
///
/// What: one `..` per component of the canonical working directory, then
/// `dir` without its leading `/`. Tests read the working directory and
/// never change it. Asserts that the result reaches `dir`.
pub(crate) fn relative_to_cwd(dir: &Path) -> PathBuf {
    let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
    let mut relative = PathBuf::new();
    for _ in cwd.components().skip(1) {
        relative.push("..");
    }
    relative.push(dir.strip_prefix("/").unwrap());
    assert!(
        relative.is_relative() && cwd.join(&relative).is_dir(),
        "{relative:?}"
    );
    relative
}
