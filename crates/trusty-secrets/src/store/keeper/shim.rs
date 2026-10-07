//! [`KeeperShim`]: a fake `keeper` for the Keeper tests (#7519 P3).
//!
//! What: a POSIX shell script in its own temp dir, run as `/bin/sh <script>`
//! with both named by absolute path — never found through `PATH`, and no
//! test changes any process-global environment. Folders are lines of
//! `folders` (full paths; `trusty`, `trusty/acme`, `trusty/acme/web` to
//! start); records are files `records/<uid>` (folder, title, type, value);
//! `rm` moves a record to `trash/`. Each call's argv goes to `calls.log`,
//! its environment to `env.log`, and a batch's stdin to `stdin.log`. The
//! batch reader decodes `password=$BASE64:` with `base64 -d`.
//!
//! Scripted failures, by subcommand `ls`, `get`, `rm` or `batch`:
//! [`KeeperShim::fail`] echoes what the call holds (stdin for a batch,
//! every stored value for `get`), prints a stderr text and exits 1 — the
//! worst case for A2; [`KeeperShim::stdout_only`] prints a text on stdout,
//! exits 0, and does nothing; [`KeeperShim::locked`] fails every call;
//! [`KeeperShim::fail_path`] fails `ls` of one path only.
//! Test: the tests that build one.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tempfile::TempDir;

use super::{KeeperBackend, KeeperSettings};
use crate::store::cli::test_shim::install_script;

/// The fake `keeper`; `@DIR@` is the shim's directory.
const SCRIPT: &str = r##"L='@DIR@'
printf '%s\n' "$*" >> "$L/calls.log"
env >> "$L/env.log"
printf '%s\n' '--' >> "$L/env.log"
while :; do
  case "$1" in
    --config) shift 2 ;;
    --batch-mode) shift ;;
    *) break ;;
  esac
done
cmd=$1
[ "$cmd" = - ] && cmd=batch
last=''
for a in "$@"; do last=$a; done
if [ -f "$L/locked" ]; then cat "$L/locked" >&2; exit 1; fi
if [ -f "$L/fail.$cmd" ]; then
  [ "$cmd" = batch ] && cat >&2
  if [ "$cmd" = get ]; then
    for f in "$L"/records/*; do [ -f "$f" ] && sed -n 4p "$f"; done
  fi
  cat "$L/fail.$cmd" >&2
  exit 1
fi
if [ -f "$L/stdout.$cmd" ]; then
  [ "$cmd" = batch ] && cat >> "$L/stdin.log"
  cat "$L/stdout.$cmd"
  exit 0
fi
case "$cmd" in
  ls)
    if [ -f "$L/failpath" ] && [ "$last" = "$(cat "$L/failpath")" ]; then
      echo "ls: Invalid folder path: $last" >&2
      exit 1
    fi
    if [ "$last" != / ] && ! grep -qx "$last" "$L/folders"; then
      echo "ls: Invalid folder path: $last" >&2
      exit 1
    fi
    printf '['
    sep=''
    while IFS= read -r f; do
      [ -n "$f" ] || continue
      case "$f" in */*) parent=${f%/*} ;; *) parent=/ ;; esac
      if [ "$parent" = "$last" ]; then
        printf '%s{"type":"folder","uid":"fld","name":"%s"}' "$sep" "${f##*/}"
        sep=','
      fi
    done < "$L/folders"
    for r in "$L"/records/*; do
      [ -f "$r" ] || continue
      if [ "$(sed -n 1p "$r")" = "$last" ]; then
        printf '%s{"type":"record","uid":"%s","title":"%s","record_type":"%s"}' \
          "$sep" "${r##*/}" "$(sed -n 2p "$r")" "$(sed -n 3p "$r")"
        sep=','
      fi
    done
    printf ']\n' ;;
  get)
    r="$L/records/$last"
    if [ ! -f "$r" ]; then echo "get: record $last not found" >&2; exit 1; fi
    printf '{"record_uid":"%s","title":"%s","type":"%s","fields":[{"type":"login","value":["svc"]},{"type":"password","value":["%s"]}],"custom":[]}\n' \
      "$last" "$(sed -n 2p "$r")" "$(sed -n 3p "$r")" "$(sed -n 4p "$r")" ;;
  rm)
    if [ ! -f "$L/records/$last" ]; then echo "rm: record $last not found" >&2; exit 1; fi
    mv "$L/records/$last" "$L/trash/$last" ;;
  batch)
    while IFS= read -r line; do
      printf '%s\n' "$line" >> "$L/stdin.log"
      set -f
      set -- $line
      set +f
      verb=$1
      shift
      folder=''; title=''; rtype=''; record=''; b64=''
      for a in "$@"; do
        case "$a" in
          --folder=*) folder=${a#--folder=} ;;
          --title=*) title=${a#--title=} ;;
          --record-type=*) rtype=${a#--record-type=} ;;
          --record=*) record=${a#--record=} ;;
          'password=$BASE64:'*) b64=${a#'password=$BASE64:'} ;;
        esac
      done
      value=$(printf '%s' "$b64" | base64 -d)
      case "$verb" in
        record-add)
          uid="new$$n$(ls "$L/records" | wc -l | tr -d ' ')"
          printf '%s\n%s\n%s\n%s\n' "$folder" "$title" "$rtype" "$value" > "$L/records/$uid"
          echo "$uid" ;;
        record-update)
          r="$L/records/$record"
          if [ ! -f "$r" ]; then echo "record-update: no such record" >&2; exit 1; fi
          { sed -n 1,3p "$r"; printf '%s\n' "$value"; } > "$L/tmp.rec"
          mv "$L/tmp.rec" "$r" ;;
        *) echo "shim: unknown batch command" >&2; exit 2 ;;
      esac
    done ;;
  *)
    echo "shim: unknown command" >&2
    exit 2 ;;
esac
"##;

/// A fake `keeper` in its own temp dir.
pub(crate) struct KeeperShim {
    dir: TempDir,
    script: PathBuf,
}

impl KeeperShim {
    pub(crate) fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let script = dir.path().join("keeper.sh");
        let body = SCRIPT.replace("@DIR@", &dir.path().display().to_string());
        std::fs::write(&script, body).unwrap();
        for sub in ["records", "trash"] {
            std::fs::create_dir(dir.path().join(sub)).unwrap();
        }
        let shim = Self { dir, script };
        shim.set_folders(&["trusty", "trusty/acme", "trusty/acme/web"]);
        shim
    }

    /// Settings that run this shim with `config_path` as `--config`.
    pub(crate) fn settings(&self, config_path: &Path) -> KeeperSettings {
        let mut settings = KeeperSettings::new("/bin/sh", config_path.to_path_buf());
        settings.leading_args = vec![self.script.clone().into_os_string()];
        settings.timeout = Duration::from_secs(20);
        settings
    }

    /// A backend over this shim.
    pub(crate) fn backend(&self) -> KeeperBackend {
        KeeperBackend::new(self.settings(&self.path("config.json")))
    }

    /// This shim as an executable `dir/keeper`, for a machine `program` pin.
    pub(crate) fn install_in(&self, dir: &Path) -> PathBuf {
        install_script(
            dir,
            "keeper",
            &format!("exec /bin/sh '{}' \"$@\"", self.script.display()),
        )
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.path(name)).unwrap_or_default()
    }

    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.path(name), text).unwrap();
    }

    /// Replace the folder paths the account holds.
    pub(crate) fn set_folders(&self, paths: &[&str]) {
        let mut body = paths.join("\n");
        body.push('\n');
        self.write("folders", &body);
    }

    /// Store a record directly, as if made outside this backend.
    pub(crate) fn seed(&self, uid: &str, folder: &str, title: &str, kind: &str, value: &str) {
        let body = format!("{folder}\n{title}\n{kind}\n{value}\n");
        std::fs::write(self.path("records").join(uid), body).unwrap();
    }

    /// Make subcommand `cmd` (`ls`, `get`, `rm`, `batch`) echo what it
    /// holds, print `stderr`, and exit 1.
    pub(crate) fn fail(&self, cmd: &str, stderr: &str) {
        self.write(&format!("fail.{cmd}"), stderr);
    }

    /// Make subcommand `cmd` print `stdout`, exit 0, and change nothing.
    pub(crate) fn stdout_only(&self, cmd: &str, stdout: &str) {
        self.write(&format!("stdout.{cmd}"), stdout);
    }

    /// Fail every call with `stderr`, as a logged-out `keeper` would.
    pub(crate) fn locked(&self, stderr: &str) {
        self.write("locked", stderr);
    }

    /// Fail `ls` of exactly `path`, though the folder exists.
    pub(crate) fn fail_path(&self, path: &str) {
        self.write("failpath", path);
    }

    /// Each call's argv, one line per call, script path excluded.
    pub(crate) fn calls(&self) -> String {
        self.read("calls.log")
    }

    /// Each call's environment, `--` after each.
    pub(crate) fn env_log(&self) -> String {
        self.read("env.log")
    }

    /// Every batch's stdin.
    pub(crate) fn stdin_log(&self) -> String {
        self.read("stdin.log")
    }

    /// Whether the shim ever ran.
    pub(crate) fn spawned(&self) -> bool {
        self.path("calls.log").exists()
    }

    /// Every live record as `(uid, title, value)`, sorted by uid.
    pub(crate) fn records(&self) -> Vec<(String, String, String)> {
        let mut records: Vec<_> = std::fs::read_dir(self.path("records"))
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let body = std::fs::read_to_string(entry.path()).unwrap();
                let lines: Vec<&str> = body.lines().collect();
                let uid = entry.file_name().to_string_lossy().into_owned();
                (uid, lines[1].to_string(), lines[3].to_string())
            })
            .collect();
        records.sort();
        records
    }

    /// The uids in Keeper's trash, sorted.
    pub(crate) fn trash(&self) -> Vec<String> {
        let mut uids: Vec<String> = std::fs::read_dir(self.path("trash"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        uids.sort();
        uids
    }
}
