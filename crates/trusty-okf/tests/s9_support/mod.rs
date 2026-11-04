//! Shared fixtures for the slice S9 adapter criteria (#10746): a fake
//! adapter process and connector specs.
//!
//! The fake adapter is a POSIX shell script that speaks the `acquire`
//! transport (4-byte big-endian length, JSON) from fixture files: it logs
//! every request, answers `hello` from `hello.json`, a `list` from
//! `list_<cursor|start>.json`, a `fetch` from `fetch_<item>.json` (an
//! `error` frame when the file is missing), substitutes the request ID for
//! `@RID@`, and writes its PID to `pid`. A `hang_<op>` file makes it sleep
//! on that op; a `die_after` file holding N makes it exit after N fetches;
//! a `spawn_grandchild` file makes it start a background `sleep` that holds
//! stdout open and writes its PID to `grandchild`.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use trusty_okf::build::registry::{ConnectorParams, Locator, SourceSpec};
use trusty_okf::import::ImportSpec;
use trusty_okf::import::adapter::AdapterCommand;

pub const SCRIPT: &str = r#"#!/bin/sh
d="$1"
echo $$ > "$d/pid"
if [ -f "$d/spawn_grandchild" ]; then sleep 600 & echo $! > "$d/grandchild"; fi
fetches=0
emit() {
  n=$(printf %s "$1" | wc -c | tr -d ' ')
  printf "$(printf '\\%03o\\%03o\\%03o\\%03o' $((n>>24&255)) $((n>>16&255)) $((n>>8&255)) $((n&255)))"
  printf %s "$1"
}
field() { printf %s "$req" | sed -n "s/.*\"$1\":\"\\([^\"]*\\)\".*/\\1/p"; }
while :; do
  h=$(dd bs=1 count=4 2>/dev/null | od -An -tu1)
  [ -z "$h" ] && exit 0
  set -- $h
  n=$(( ($1<<24) + ($2<<16) + ($3<<8) + $4 ))
  req=$(dd bs=1 count=$n 2>/dev/null)
  printf '%s\n' "$req" >> "$d/requests.log"
  rid=$(printf %s "$req" | sed -n 's/.*"request_id":\([0-9]*\).*/\1/p')
  type=$(field type)
  op=$(field op)
  case "$type" in
    hello) f="$d/hello.json" ;;
    shutdown) exit 0 ;;
    acquire)
      [ -f "$d/hang_$op" ] && exec sleep 600
      if [ "$op" = list ]; then
        c=$(field cursor); f="$d/list_${c:-start}.json"
      else
        fetches=$((fetches+1))
        if [ -f "$d/die_after" ] && [ "$fetches" -gt "$(cat "$d/die_after")" ]; then exit 3; fi
        f="$d/fetch_$(field item_id).json"
      fi ;;
    *) exit 4 ;;
  esac
  if [ -f "$f" ]; then
    resp=$(sed "s/@RID@/$rid/g" "$f")
  else
    resp="{\"type\":\"error\",\"request_id\":$rid,\"code\":\"fetch_error\",\"message\":\"no fixture\"}"
  fi
  emit "$resp"
done
"#;

/// A fake adapter: its script, its fixture directory, and a bundle root.
pub struct FakeAdapter {
    pub tmp: tempfile::TempDir,
    pub dir: PathBuf,
    pub root: PathBuf,
}

impl FakeAdapter {
    /// A fake for store kind `kind`, with an empty bundle.
    pub fn new(kind: &str) -> FakeAdapter {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let (dir, root) = (base.join("adapter"), base.join("bundle"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(base.join("adapter.sh"), SCRIPT).unwrap();
        let fake = FakeAdapter { tmp, dir, root };
        fake.put(
            "hello.json",
            &json!({"type": "hello", "protocol": 1, "adapter_version": "fake", "kind": kind}),
        );
        fake
    }

    pub fn script(&self) -> PathBuf {
        self.dir.parent().unwrap().join("adapter.sh")
    }

    fn put(&self, name: &str, v: &Value) {
        let text = v.to_string().replace("\"@RID@\"", "@RID@");
        std::fs::write(self.dir.join(name), text).unwrap();
    }

    /// The page served for `cursor` (`None`: the first page).
    pub fn page(&self, cursor: Option<&str>, entries: Value, next: Option<&str>, complete: bool) {
        let name = format!("list_{}.json", cursor.unwrap_or("start"));
        self.put(
            &name,
            &json!({"type": "listing", "request_id": "@RID@", "entries": entries,
                    "cursor": next, "complete": complete}),
        );
    }

    /// A raw listing reply for `cursor`, any shape.
    pub fn page_raw(&self, cursor: Option<&str>, reply: Value) {
        let name = format!("list_{}.json", cursor.unwrap_or("start"));
        self.put(&name, &reply);
    }

    /// The PID the fake's background grandchild wrote, when it started one.
    pub fn grandchild(&self) -> Option<u32> {
        std::fs::read_to_string(self.dir.join("grandchild"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }

    /// The record served for its `item_id`.
    pub fn item(&self, record: Value) {
        let id = record["item_id"].as_str().unwrap().to_string();
        self.put(
            &format!("fetch_{id}.json"),
            &json!({"type": "item", "request_id": "@RID@", "record": record}),
        );
    }

    pub fn flag(&self, name: &str, text: &str) {
        std::fs::write(self.dir.join(name), text).unwrap();
    }

    pub fn unflag(&self, name: &str) {
        let _ = std::fs::remove_file(self.dir.join(name));
    }

    /// The command that runs the fake: `/bin/sh <script> <dir>` with a
    /// minimal environment list.
    pub fn cmd(&self) -> AdapterCommand {
        let mut c = AdapterCommand::new("/bin/sh");
        c.args = vec![self.script().into(), self.dir.clone().into()];
        c.environment = vec![
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("LC_ALL".into(), "C".into()),
        ];
        c
    }

    /// Every request the fake received, in order.
    pub fn requests(&self) -> Vec<Value> {
        std::fs::read_to_string(self.dir.join("requests.log"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// The item IDs of every `fetch` request.
    pub fn fetched(&self) -> Vec<String> {
        self.requests()
            .iter()
            .filter(|r| r["op"] == "fetch")
            .map(|r| r["item_id"].as_str().unwrap().to_string())
            .collect()
    }

    pub fn clear_requests(&self) {
        let _ = std::fs::remove_file(self.dir.join("requests.log"));
    }

    pub fn pid(&self) -> Option<u32> {
        std::fs::read_to_string(self.dir.join("pid"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }
}

/// Whether process `pid` exists (a zombie counts as existing).
pub fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A connector spec of `kind` with the given `--select` style params.
pub fn spec(store: &str, kind: &str, params: &[(&str, toml::Value)]) -> ImportSpec {
    let mut p = ConnectorParams::new();
    for (k, v) in params {
        p.insert(k, v.clone());
    }
    let locator = Locator::Connector {
        kind: kind.into(),
        params: p,
    };
    ImportSpec::new(SourceSpec::new(
        store,
        None,
        locator,
        "2026-11-04T00:00:00Z",
    ))
}

pub fn s(v: &str) -> toml::Value {
    toml::Value::String(v.into())
}

/// A `gmail` spec with an account and a query.
pub fn mail_spec() -> ImportSpec {
    spec(
        "mail-src",
        "gmail",
        &[("account", s("work")), ("query", s("in:sent"))],
    )
}

/// A `gdocs` spec over a folder, recursive or not, tombstoning on.
pub fn docs_spec(recursive: bool) -> ImportSpec {
    let mut sp = spec(
        "docs-src",
        "gdocs",
        &[
            ("account", s("work")),
            ("folder_id", s("F1")),
            ("recursive", toml::Value::Boolean(recursive)),
        ],
    );
    sp.store.tombstone_deleted = true;
    sp
}

/// A mail list entry.
pub fn msg_entry(id: &str, thread: &str) -> Value {
    json!({"item_id": id, "group": thread, "revision": id})
}

/// A mail record with a text body.
pub fn msg(id: &str, thread: &str, ms: u64, subject: &str, text: &str) -> Value {
    json!({
        "item_id": id, "group": thread, "revision": id, "title": subject,
        "dates": {"internal_date": ms.to_string(), "date_header": "Thu, 1 Jan 2026 00:00:00 +0000"},
        "parts": [{"mime": "text/plain", "data": text}],
    })
}

pub fn read(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// What a SIGINT to the CLI left behind ([`interrupt_cli`]).
#[derive(Debug)]
pub struct Interrupted {
    /// The CLI's exit code; `None` when a signal ended it or it never
    /// exited within 10 s and the test killed it.
    pub code: Option<i32>,
    /// The JSON report the CLI printed, when it printed one.
    pub report: Option<Value>,
    /// Whether the adapter, its grandchild or anything in its process group
    /// was still running 3 s after the CLI ended.
    pub adapter_left: bool,
    pub grandchild_left: bool,
    pub group_left: bool,
}

/// Whether anything is left in process group `pgid`.
fn group_alive(pgid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-s", "0", "--", &format!("-{pgid}")])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// #10746 S9+S10 merge (Architect ruling 2c9381bdbdc9): run `import run`
/// through the fake with `extra` arguments, make the adapter hang on `op`
/// after it starts a grandchild, send the CLI one SIGINT, and record what
/// is left. Every process the fake left running is killed before return,
/// so a failing run leaks nothing.
pub fn interrupt_cli(fake: &FakeAdapter, op: &str, extra: &[&str]) -> Interrupted {
    use std::time::{Duration, Instant};
    for f in ["pid", "grandchild", "requests.log"] {
        let _ = std::fs::remove_file(fake.dir.join(f));
    }
    fake.flag(&format!("hang_{op}"), "");
    fake.flag("spawn_grandchild", "");
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_trusty-okf"))
        .args(["import", "run"])
        .arg(&fake.root)
        .args(["--from", "adapter:gmail", "--adapter", "/bin/sh"])
        .arg("--adapter-arg")
        .arg(fake.script())
        .arg("--adapter-arg")
        .arg(&fake.dir)
        .args([
            "--adapter-var",
            "PATH=/usr/bin:/bin",
            "--adapter-var",
            "LC_ALL=C",
        ])
        .args(["--select", "account=work", "--select", "query=in:sent"])
        .args(["--json", "--now", "2026-11-04T12:00:00Z"])
        .args(extra)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    // The fake logs a request before it hangs on it.
    let wanted = format!("\"op\":\"{op}\"");
    let until = Instant::now() + Duration::from_secs(10);
    while !std::fs::read_to_string(fake.dir.join("requests.log"))
        .unwrap_or_default()
        .contains(&wanted)
    {
        assert!(Instant::now() < until, "the adapter never received {op}");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(300));
    let sent = std::process::Command::new("kill")
        .args(["-s", "INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(sent.success(), "kill -INT failed");
    let until = Instant::now() + Duration::from_secs(10);
    let exited = loop {
        if child.try_wait().unwrap().is_some() {
            break true;
        }
        if Instant::now() >= until {
            let _ = child.kill();
            break false;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let out = child.wait_with_output().unwrap();
    let (adapter, grandchild) = (fake.pid(), fake.grandchild());
    let left = |p: Option<u32>| p.is_some_and(alive);
    let until = Instant::now() + Duration::from_secs(3);
    while (left(adapter) || left(grandchild) || adapter.is_some_and(group_alive))
        && Instant::now() < until
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    let got = Interrupted {
        code: if exited { out.status.code() } else { None },
        report: serde_json::from_slice(&out.stdout).ok(),
        adapter_left: left(adapter),
        grandchild_left: left(grandchild),
        group_left: adapter.is_some_and(group_alive),
    };
    for pid in [adapter, grandchild].into_iter().flatten() {
        let _ = std::process::Command::new("kill")
            .args(["-s", "KILL", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status();
    }
    fake.unflag(&format!("hang_{op}"));
    fake.unflag("spawn_grandchild");
    got
}

/// The `(read, write, reason)` of `item_id` in a JSON report.
pub fn outcome_of(report: &Value, item_id: &str) -> (String, String, String) {
    let it = report["items"]
        .as_array()
        .and_then(|a| a.iter().find(|i| i["item_id"] == item_id))
        .unwrap_or_else(|| panic!("{item_id} not in {report}"));
    let s = |k: &str| it[k].as_str().unwrap_or("").to_string();
    (s("read"), s("write"), s("reason"))
}
