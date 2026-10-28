//! T24: the console bridge against the real `trusty-secrets` server and the
//! host's real process table (#9066, A3, A9, A10).
//!
//! Why: the stub-socket tests prove what the console sends; only a real
//! server proves what it then decides and records. The server believes a
//! bridge assertion only from a peer whose executable is `trusty-console`
//! with no Claude Code above it, and judges `set`, `delete` and a deny
//! removal (`set_agents_may_use` with `allowed: true`, #10386) on the
//! console's verdict.
//! What: [`live_bridge_is_judged_by_the_real_server`] re-runs this test
//! binary in roles chosen by [`ROLE`]:
//! - `console`: a copy named `trusty-console` serving the console's own
//!   router on loopback with the accept-time pid lookup, dialing the live
//!   socket in external mode;
//! - `parent`: a copy named `claude` (or `notclaude`, the control) whose only
//!   job is to be the real parent of the client;
//! - `client`: this binary, making the HTTP calls a page would.
//!
//! The server is the `trusty-secrets` binary built beside this test binary,
//! else the one `bin_resolve` finds; its `HOME` and machine config are under
//! one temp root, with the `file` backend. The console reads a tm registry
//! under the same root that lists the checkout (#9067 PR B1). Run once with the audit on (the
//! control: records appear) and once with `secrets.audit: false` (none
//! appear). Values are never printed. Run on a host that is not itself under
//! Claude Code, or the console is refused as a bridge peer.
//! [`live_page_flow_over_real_server_honours_registry`] (#9067 PR C) reuses
//! the `console` role and makes the page's calls from the test process.
//! Test: itself (`#[ignore]`; `--include-ignored`).

#![cfg(unix)]

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;

#[path = "support/self_copy.rs"]
mod self_copy;
use self_copy::self_copy;

const TEST: &str = "live_bridge_is_judged_by_the_real_server";
const ROLE: &str = "TC_LIVE_ROLE";
const SOCKET: &str = "TC_LIVE_SOCKET";
const OUT: &str = "TC_LIVE_OUT";
const ADDR: &str = "TC_LIVE_ADDR";
const PROJECT: &str = "TC_LIVE_PROJECT";
const REGISTRY: &str = "TC_LIVE_REGISTRY";
const CLIENT: &str = "TC_LIVE_CLIENT";
const VAULT: &str = "trusty/acme/web";
const KEY: &str = "API_KEY";
// #9067 PR C: the second key the page counts, and the key an unlisted call names.
const KEY_B: &str = "DB_URL";
const UNLISTED_KEY: &str = "UNLISTED_KEY";
const VALUE: &str = "live-9066-value-Hx7Pq2";
const CSRF: &str = "x-trusty-secrets-csrf";

/// The real server binary: a workspace build beside this test, else installed.
fn server_binary() -> PathBuf {
    let sibling = Path::new(env!("CARGO_BIN_EXE_trusty-console")).with_file_name("trusty-secrets");
    let found = sibling
        .to_str()
        .and_then(trusty_common::bin_resolve::resolve_binary)
        .or_else(|| trusty_common::bin_resolve::resolve_binary("trusty-secrets"));
    found.expect(
        "no trusty-secrets binary: build it with `cargo build -p trusty-secrets --features server`",
    )
}

/// One temp root: the project checkout and the server. The named copies are
/// per process, from [`self_copy`] (#10212).
struct Live {
    root: TempDir,
    children: Vec<Child>,
}

impl Drop for Live {
    fn drop(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Live {
    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn new() -> Self {
        let base = if cfg!(target_os = "macos") {
            "/private/tmp"
        } else {
            "/tmp"
        };
        let root = tempfile::Builder::new()
            .prefix("tc")
            .tempdir_in(base)
            .expect("tempdir");
        let live = Self {
            root,
            children: Vec::new(),
        };
        let repo = live.path("repo");
        init_checkout(&repo);
        // #9067: the console dials only for a project the tm registry lists.
        let rows = json!([{ "alias": "web", "path": repo }]);
        std::fs::write(live.path("registry.json"), rows.to_string()).expect("registry");
        live
    }

    /// Start a server with the audit on or off; its socket and audit log.
    fn serve(&mut self, tag: &str, audit: bool) -> (PathBuf, PathBuf) {
        let dir = self.path(tag);
        let home = dir.join("home");
        std::fs::create_dir_all(&home).expect("home");
        let config = dir.join("machine.yaml");
        let audit_line = if audit { "" } else { "  audit: false\n" };
        std::fs::write(
            &config,
            format!("secrets:\n  default_backend: file\n{audit_line}"),
        )
        .expect("config");
        let socket = dir.join("s.sock");
        let audit_log = dir.join("audit").join("audit.jsonl");
        let server = Command::new(server_binary())
            .args(["serve", "--socket"])
            .arg(&socket)
            .arg("--index-dir")
            .arg(dir.join("index"))
            .arg("--machine-config")
            .arg(&config)
            .arg("--audit-log")
            .arg(&audit_log)
            .args(["--idle-timeout-secs", "120"])
            .env("HOME", &home)
            .env_remove("TRUSTY_SECRETS_SOCKET")
            .env_remove("TRUSTY_SECRETS_INDEX_DIR")
            .env_remove("TRUSTY_SECRETS_IDLE_TIMEOUT_SECS")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn trusty-secrets");
        self.children.push(server);
        wait_for(|| socket.exists());
        (socket, audit_log)
    }

    /// Start the `trusty-console` copy on `socket`; its bound address.
    fn console(&mut self, tag: &str, socket: &Path) -> SocketAddr {
        let out = self.path(tag).join("addr");
        let console = role(&self_copy("trusty-console"), "console")
            .env(SOCKET, socket)
            .env(REGISTRY, self.path("registry.json"))
            .env(OUT, &out)
            .spawn()
            .expect("spawn console");
        self.children.push(console);
        wait_for(|| out.exists());
        let text = std::fs::read_to_string(&out).expect("addr");
        text.trim().parse().expect("addr")
    }

    /// Run the client under a parent copy named `parent`; its report.
    fn client(&self, parent: &str, addr: SocketAddr) -> Value {
        let out = self.path(&format!("{parent}-{}.json", addr.port()));
        let status = role(&self_copy(parent), "parent")
            .env(CLIENT, std::env::current_exe().expect("exe"))
            .env(ADDR, addr.to_string())
            .env(PROJECT, self.path("repo"))
            .env(OUT, &out)
            .status()
            .expect("run parent");
        assert!(status.success(), "{parent} run failed");
        serde_json::from_str(&std::fs::read_to_string(&out).expect("report")).expect("json")
    }
}

/// A git checkout at `dir` whose origin maps to [`VAULT`].
fn init_checkout(dir: &Path) {
    std::fs::create_dir(dir).expect("checkout");
    for args in [
        vec!["init", "-q"],
        vec!["remote", "add", "origin", "git@github.com:Acme/Web.git"],
    ] {
        let ok = Command::new("git").arg("-C").arg(dir).args(&args).status();
        assert!(ok.expect("git").success(), "git {args:?}");
    }
}

fn wait_for(ready: impl Fn() -> bool) {
    let start = Instant::now();
    while !ready() {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "never became ready"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// This test, re-run from `exe` in `role`.
fn role(exe: &Path, role: &str) -> Command {
    let mut command = Command::new(exe);
    command
        .args([
            TEST,
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ROLE, role)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// The `console` role: the console's router on loopback, as `run_serve` serves it.
fn run_console() {
    use trusty_console::secrets_uds::SecretsBridge;
    use trusty_console::secrets_uds::dial::SecretsDial;
    use trusty_console::secrets_uds::verdict::PeerLookup;
    let socket = PathBuf::from(std::env::var_os(SOCKET).expect("socket"));
    let out = PathBuf::from(std::env::var_os(OUT).expect("out"));
    let registry = PathBuf::from(std::env::var_os(REGISTRY).expect("registry"));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async move {
        let bridge = SecretsBridge::new(SecretsDial::new(socket).with_external(true))
            .expect("mint")
            .with_registry(Some(registry));
        let state = trusty_console::server::AppState::new(Vec::new()).with_secrets(bridge);
        let secrets = state.secrets();
        let app = trusty_console::server::build_router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        secrets.set_authorities(&[addr]);
        std::fs::write(&out, addr.to_string()).expect("write addr");
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<PeerLookup>(),
        )
        .await
        .expect("serve");
    });
}

/// The `parent` role: run the client as this process's child.
fn run_parent() {
    let client = std::env::var_os(CLIENT).expect("client");
    let status = role(Path::new(&client), "client").status().expect("client");
    std::process::exit(if status.success() { 0 } else { 1 });
}

/// One `POST` with the page's headers; the status and the JSON body.
fn post(addr: SocketAddr, csrf: Option<&str>, path: &str, body: &Value) -> (u16, Value) {
    let body = body.to_string();
    let mut request = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nOrigin: http://{addr}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(csrf) = csrf {
        request.push_str(&format!("{CSRF}: {csrf}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(&body);
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream.write_all(request.as_bytes()).expect("write");
    let mut raw = String::new();
    stream.read_to_string(&mut raw).expect("read");
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let json = raw
        .split_once("\r\n\r\n")
        .and_then(|(_, body)| serde_json::from_str(body).ok())
        .unwrap_or(Value::Null);
    (status, json)
}

/// The `client` role: every bridged method once; statuses only in the report.
fn run_client() {
    let addr: SocketAddr = std::env::var(ADDR).expect("addr").parse().expect("addr");
    let project = std::env::var(PROJECT).expect("project");
    let (_, session) = post(addr, None, "/api/secrets/session", &json!({}));
    let csrf = session["csrf"].as_str().expect("csrf").to_owned();
    let call = |path: &str, body: Value| post(addr, Some(&csrf), path, &body).0;
    let target = json!({ "project": project, "vault": VAULT, "key": KEY });
    let flag = |allowed: bool| {
        let mut body = target.clone();
        body["allowed"] = json!(allowed);
        call("/api/secrets/agents_may_use", body)
    };
    let mut set = target.clone();
    set["value"] = json!(VALUE);
    // Each client sets the key first, so its flag calls find it.
    let set = call("/api/secrets/set", set);
    let list = call(
        "/api/secrets/list",
        json!({ "project": project, "vault": VAULT }),
    );
    let (flag_on, flag_off) = (flag(true), flag(false));
    let copy = json!({ "project": project, "from_backend": "file", "to_backend": "file" });
    let report = json!({
        "set": set,
        "list": list,
        "flag_on": flag_on,
        "flag_off": flag_off,
        "copy": call("/api/secrets/copy", copy),
        "delete": call("/api/secrets/delete", target.clone()),
    });
    let out = std::env::var_os(OUT).expect("out");
    std::fs::write(out, report.to_string()).expect("report");
}

fn records(audit_log: &Path) -> Vec<Value> {
    let text = std::fs::read_to_string(audit_log).unwrap_or_default();
    assert!(!text.contains(VALUE), "a value reached the audit log");
    text.lines()
        .map(|l| serde_json::from_str(l).expect("record"))
        .collect()
}

#[test]
#[ignore = "live: real trusty-secrets binary and process tree; run outside Claude Code"]
fn live_bridge_is_judged_by_the_real_server() {
    match std::env::var(ROLE).as_deref() {
        Ok("console") => return run_console(),
        Ok("parent") => return run_parent(),
        Ok("client") => return run_client(),
        _ => {}
    }
    eprintln!("trusty-secrets: {}", server_binary().display());
    let mut live = Live::new();
    for (tag, audit) in [("on", true), ("off", false)] {
        let (socket, audit_log) = live.serve(tag, audit);
        let addr = live.console(tag, &socket);
        let human = live.client("notclaude", addr);
        let agent = live.client("claude", addr);
        eprintln!("audit {tag}: notclaude {human} claude {agent}");
        assert_eq!(human["set"], 200, "{tag}: {human}");
        // #10386 Q5: the file backend holds the deny, so a person may both
        // remove and write it.
        assert_eq!(human["flag_on"], 200, "{tag}: not refused as an agent");
        assert_eq!(human["flag_off"], 200, "{tag}: {human}");
        // #10386 Q2: `set` and `delete` are operator-only.
        assert_eq!(agent["set"], 403, "{tag}: an agent may not set");
        assert_eq!(agent["delete"], 403, "{tag}: an agent may not delete");
        assert_eq!(
            agent["flag_on"], 403,
            "{tag}: an agent may not remove a deny"
        );
        // Writing a deny passes the agent check; the person's client deleted
        // the key first, so it then fails on the missing row (404).
        assert_eq!(agent["flag_off"], 404, "{tag}: an agent may write a deny");
        let records = records(&audit_log);
        if audit {
            let methods: std::collections::HashSet<&str> = records
                .iter()
                .filter_map(|r| r["method"].as_str())
                .collect();
            for method in ["set", "list", "set_agents_may_use", "copy", "delete"] {
                assert!(
                    methods.contains(format!("secrets.{method}").as_str()),
                    "{method}: {methods:?}"
                );
            }
            assert!(
                records.iter().all(|r| r["source"] == "mapped"),
                "{records:?}"
            );
            let flag_on = |decision: &str, agent: bool| {
                records.iter().any(|r| {
                    r["method"] == "secrets.set_agents_may_use"
                        && r["agents_allowed"] == true
                        && r["decision"] == decision
                        && r["agent_parent"] == agent
                })
            };
            assert!(flag_on("allow", false), "the non-agent was admitted");
            assert!(
                records
                    .iter()
                    .any(|r| r["reason"] == "agent_use_refused" && r["agent_parent"] == true),
                "the agent was refused as one"
            );
        } else {
            assert!(
                records.is_empty(),
                "audit off wrote {} records",
                records.len()
            );
        }
    }
}

/// The page's side of #9067 PR C: its CSRF token and the server's audit log.
struct Page {
    addr: SocketAddr,
    csrf: String,
    audit_log: PathBuf,
}

impl Page {
    /// One `POST /api/secrets/<route>` with the page's headers.
    fn call(&self, route: &str, body: Value) -> (u16, Value) {
        post(
            self.addr,
            Some(&self.csrf),
            &format!("/api/secrets/{route}"),
            &body,
        )
    }

    /// [`Page::call`], plus the audit records the server wrote for it.
    fn traced(&self, route: &str, body: Value) -> (u16, Value, Vec<Value>) {
        let before = records(&self.audit_log).len();
        let (status, answer) = self.call(route, body);
        let mut after = records(&self.audit_log);
        (status, answer, after.split_off(before))
    }
}

/// The key names a `list` answer carries.
fn key_names(list: &Value) -> Vec<&str> {
    list["keys"]
        .as_array()
        .expect("keys")
        .iter()
        .filter_map(|meta| meta["name"].as_str())
        .collect()
}

/// Why: #9067 PR C — the picker, scopes, backends, the registry gate and
/// the copy wire contract were proven only against a stub socket; only the
/// real server shows what reaches it.
/// What: T24's temp root and `console` role over an audit-on `file` server,
/// with a second checkout of the same repo that the registry does not list.
/// This process is the HTTP client. A refused call that never dialled is
/// one with no new audit record. Linux only: a macOS run returns before
/// spawning anything, so no Keychain is opened (ruling Q3).
/// Test: itself (`#[ignore]`; run by name with `--ignored --exact` on EVO).
#[test]
#[ignore = "live: real trusty-secrets binary; Linux only (EVO); run by name"]
fn live_page_flow_over_real_server_honours_registry() {
    if cfg!(target_os = "macos") {
        eprintln!("skipped: Linux only, so no macOS run opens the Keychain (#9067 PR C, Q3)");
        return;
    }
    eprintln!("trusty-secrets: {}", server_binary().display());
    let mut live = Live::new();
    let (repo, unlisted) = (live.path("repo"), live.path("unlisted"));
    init_checkout(&unlisted);
    let repo_s = repo.to_str().expect("utf-8").to_owned();
    let unlisted_s = unlisted.to_str().expect("utf-8").to_owned();
    let registry = live.path("registry.json");
    let rows = json!([
        { "alias": "web", "path": repo },
        { "alias": "ghost", "path": live.path("ghost") },
        { "alias": "relative", "path": "relative/web" },
    ]);
    std::fs::write(&registry, rows.to_string()).expect("registry");
    let (socket, audit_log) = live.serve("page", true);
    let addr = live.console("page", &socket);
    let (_, session) = post(addr, None, "/api/secrets/session", &json!({}));
    let csrf = session["csrf"].as_str().expect("csrf").to_owned();
    let page = Page {
        addr,
        csrf,
        audit_log,
    };
    let listed = json!({ "project": repo_s });
    let vault = |project: &str| json!({ "project": project, "vault": VAULT });

    // (1) The picker lists the one real registered directory and never dials.
    let (status, picked, new) = page.traced("projects", json!({}));
    assert_eq!(status, 200, "projects: {picked}");
    let only_repo = json!({ "projects": [{ "name": "web", "path": repo_s }] });
    assert_eq!(picked, only_repo, "the picker lists only the real dir");
    assert!(new.is_empty(), "the picker dialled: {new:?}");

    // (2) An unlisted checkout is refused on every project route, undialled.
    let entry = |key: &str| {
        let mut body = vault(&unlisted_s);
        body["key"] = json!(key);
        body
    };
    let mut set = entry(UNLISTED_KEY);
    set["value"] = json!(VALUE);
    let copy = json!({ "project": unlisted_s, "from_backend": "file", "to_backend": "file" });
    for (route, body) in [
        ("list", vault(&unlisted_s)),
        ("delete", entry(UNLISTED_KEY)),
        ("copy", copy),
        ("scopes", json!({ "project": unlisted_s })),
        ("backends", json!({ "project": unlisted_s })),
        ("set", set),
    ] {
        let (status, answer, new) = page.traced(route, body);
        assert_eq!(status, 400, "unlisted {route} was not refused: {answer}");
        assert!(
            new.is_empty(),
            "unlisted {route} reached the server: {new:?}"
        );
    }
    for route in ["list", "scopes", "backends"] {
        let body = if route == "list" {
            vault(&repo_s)
        } else {
            listed.clone()
        };
        let (status, answer) = page.call(route, body);
        assert_eq!(status, 200, "listed {route}, the control: {answer}");
    }
    // Same remote, same vault: a key the unlisted `set` stored would list here.
    let (_, held) = page.call("list", vault(&repo_s));
    assert!(key_names(&held).is_empty(), "the server holds {held}");

    // (3) A registry that cannot be read fails closed; restored, it serves.
    std::fs::write(&registry, "not json {").expect("garbage registry");
    let (status, answer, new) = page.traced("list", vault(&repo_s));
    assert_eq!(status, 503, "a garbage registry: {answer}");
    assert!(new.is_empty(), "a garbage registry dialled: {new:?}");
    std::fs::write(&registry, rows.to_string()).expect("restore registry");
    let (status, answer) = page.call("list", vault(&repo_s));
    assert_eq!(status, 200, "a restored registry: {answer}");

    // (4) Scopes: project and owner only, the project scope on this repo's vault.
    let (status, scopes) = page.call("scopes", listed.clone());
    assert_eq!(status, 200, "scopes: {scopes}");
    let scopes = scopes["scopes"].as_array().expect("scopes").clone();
    let kinds: Vec<&str> = scopes.iter().filter_map(|s| s["kind"].as_str()).collect();
    assert_eq!(kinds, ["project", "owner"], "{scopes:?}");
    assert_eq!(scopes[0]["vault"], VAULT, "{scopes:?}");

    // (5) Backends: ids and availability only; the file backend is selected.
    let (status, backends) = page.call("backends", listed.clone());
    assert_eq!(status, 200, "backends: {backends}");
    let fields = |v: &Value| -> Vec<String> {
        let mut keys: Vec<String> = v.as_object().expect("object").keys().cloned().collect();
        keys.sort();
        keys
    };
    assert_eq!(fields(&backends), ["backends", "selected"], "{backends}");
    assert_eq!(backends["selected"], "file", "{backends}");
    let rows_out = backends["backends"].as_array().expect("backends");
    assert!(
        rows_out
            .iter()
            .all(|row| fields(row) == ["available", "id"]),
        "{backends}"
    );
    let file = rows_out.iter().find(|row| row["id"] == "file");
    assert_eq!(file.map(|row| &row["available"]), Some(&json!(true)));

    // (6) Copy wire contract: the page's count, then the project it counted.
    for key in [KEY, KEY_B] {
        let mut body = vault(&repo_s);
        body["key"] = json!(key);
        body["value"] = json!(VALUE);
        let (status, answer) = page.call("set", body);
        assert_eq!(status, 200, "set {key}: {answer}");
    }
    let (_, listing) = page.call("list", vault(&repo_s));
    assert_eq!(key_names(&listing), [KEY, KEY_B], "the page's count");
    let copy =
        |from: &str| json!({ "project": repo_s, "from_backend": from, "to_backend": "file" });
    // Q4: the server refuses a same-backend copy, before it resolves the project.
    let (status, answer, new) = page.traced("copy", copy("file"));
    assert_eq!((status, &answer["kind"]), (400, &json!("same_backend")));
    let [record] = new.as_slice() else {
        panic!("same-backend copy records: {new:?}")
    };
    assert_eq!(record["method"], "secrets.copy", "{record}");
    assert_eq!(record["reason"], "same_backend", "{record}");
    // A Linux build has no Keychain, so this copy fails after the server has
    // resolved the project; its record names the checkout it acted on.
    let (status, answer, new) = page.traced("copy", copy("keychain"));
    assert_eq!((status, &answer["kind"]), (422, &json!("unknown_backend")));
    let canonical = std::fs::canonicalize(&repo).expect("canonical repo");
    let [record] = new.as_slice() else {
        panic!("keychain copy records: {new:?}")
    };
    assert_eq!(record["method"], "secrets.copy", "{record}");
    assert_eq!(
        record["project_root"],
        json!(canonical),
        "copy acted on another project"
    );
    let other = json!({ "project": unlisted_s, "from_backend": "file", "to_backend": "file" });
    let (status, answer, new) = page.traced("copy", other);
    assert_eq!(status, 400, "copy naming an unlisted project: {answer}");
    assert!(
        new.is_empty(),
        "copy naming an unlisted project dialled: {new:?}"
    );
}
