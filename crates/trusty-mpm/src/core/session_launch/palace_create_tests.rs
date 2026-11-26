//! Tests for the launch-time palace create (#11823).
//!
//! Why: the create only matters if the daemon sees `{name, cwd}` without
//! `force`, and a failure only helps if it warns; both need a daemon on a
//! socket to observe.
//! What: a stub trusty-memory built from `trusty_common::uds::server`, answering
//! the protocol handshake and `palace_create` per test and recording every call.
//! Test: this file IS the test module.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{Value, json};
use trusty_common::memory_rpc::{METHOD_PROTOCOL, TRUSTY_MEMORY_SOCKET_ENV};
use trusty_common::uds::server::{
    CODE_METHOD_NOT_FOUND, RpcError, RpcFallback, RpcRouter, RpcServeOptions, serve_until,
};

use super::*;
use crate::secret_source::test_env::EnvVarGuard;

/// The remote every fixture project carries; resolves to [`PALACE`].
pub(crate) const REMOTE: &str = "https://github.com/acme/widget.git";
/// The env-free resolver's name for a project with [`REMOTE`].
pub(crate) const PALACE: &str = "acme-widget";

/// How the stub answers `palace_create`.
#[derive(Clone, Copy)]
pub(crate) enum Reply {
    /// `{"status": "created"}`.
    Created,
    /// `{"status": "exists"}`.
    Exists,
    /// `{"status": "aliased", "alias_of": "widget"}` — linked to a folder palace.
    Aliased,
    /// `{"status": "exists", "alias_of": "widget"}` — a live alias held the id.
    ExistsAlias,
    /// A -32603 refusal naming the palace and the reason, as the daemon sends.
    Refused,
    /// Never answers within any launch budget.
    Hang,
}

type Calls = Arc<Mutex<Vec<(String, Value)>>>;

struct Stub {
    reply: Reply,
    calls: Calls,
}

#[async_trait]
impl RpcFallback for Stub {
    async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push((method.to_string(), params.clone()));
        }
        if method == METHOD_PROTOCOL {
            return Ok(json!({"protocol_version": 1}));
        }
        if method != CREATE_METHOD {
            return Err(RpcError::new(CODE_METHOD_NOT_FOUND, "Method not found"));
        }
        let name = params["name"].clone();
        match self.reply {
            Reply::Created => {
                Ok(json!({"palace_id": name, "status": "created", "bootstrap": null}))
            }
            Reply::Exists => Ok(json!({"palace_id": name, "status": "exists", "bootstrap": null})),
            Reply::Aliased => {
                Ok(json!({"palace_id": name, "status": "aliased", "alias_of": "widget"}))
            }
            Reply::ExistsAlias => {
                Ok(json!({"palace_id": name, "status": "exists", "alias_of": "widget"}))
            }
            Reply::Refused => Err(RpcError::new(
                -32603,
                format!("palace name '{name}' does not match the project palace 'other'"),
            )),
            Reply::Hang => {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok(json!({}))
            }
        }
    }
}

/// A stub trusty-memory on its own runtime, so sync tests can drive it.
pub(crate) struct StubDaemon {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    calls: Calls,
    _stop: tokio::sync::oneshot::Sender<()>,
    _runtime: tokio::runtime::Runtime,
}

impl StubDaemon {
    /// Bind a stub that answers `palace_create` with `reply`.
    pub(crate) fn start(reply: Reply) -> Self {
        let dir = crate::test_support::hermetic_temp_dir();
        let socket = dir.path().join("memory.sock");
        let calls: Calls = Arc::new(Mutex::new(Vec::new()));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("stub runtime");
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let router = Arc::new(RpcRouter::new().fallback(Stub {
            reply,
            calls: Arc::clone(&calls),
        }));
        let bind_at = socket.clone();
        runtime.block_on(async {
            let listener = trusty_common::uds::bind_hardened(&bind_at).expect("bind the stub");
            tokio::spawn(async move {
                serve_until(&listener, router, RpcServeOptions::default(), async {
                    let _ = rx.await;
                })
                .await;
            });
        });
        Self {
            _dir: dir,
            socket,
            calls,
            _stop: tx,
            _runtime: runtime,
        }
    }

    pub(crate) fn socket(&self) -> &Path {
        &self.socket
    }

    /// Every `palace_create` the stub received, as its params.
    pub(crate) fn creates(&self) -> Vec<Value> {
        self.calls
            .lock()
            .expect("calls")
            .iter()
            .filter(|(m, _)| m == CREATE_METHOD)
            .map(|(_, p)| p.clone())
            .collect()
    }
}

/// `git init` `dir` with `remote` as `origin`; no network.
pub(crate) fn git_project(dir: &Path, remote: &str) {
    let run = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git on PATH");
        assert!(status.success(), "git {args:?} failed");
    };
    run(&["init", "-q"]);
    run(&["config", "remote.origin.url", remote]);
}

/// A fixture project resolving to [`PALACE`].
fn widget() -> tempfile::TempDir {
    let dir = crate::test_support::hermetic_temp_dir();
    git_project(dir.path(), REMOTE);
    dir
}

/// Assert `creates` holds exactly one create from `project`: for [`PALACE`],
/// with `cwd` and no `force`.
///
/// Why filter on `cwd`: a launch test sets `TRUSTY_MEMORY_SOCKET` for the
/// process, so a launch in a concurrent unserialised test can reach this stub.
pub(crate) fn assert_one_create(creates: &[Value], project: &Path) {
    let cwd = project.to_str().expect("utf-8");
    let mine: Vec<&Value> = creates.iter().filter(|c| c["cwd"] == cwd).collect();
    assert_eq!(mine.len(), 1, "exactly one create from {cwd}: {creates:?}");
    assert_eq!(mine[0]["name"], PALACE);
    assert!(mine[0].get("force").is_none(), "never force: {mine:?}");
}

/// Why: the headline fix — a missing palace and a serving daemon get a
/// create carrying the project dir as `cwd` and no `force`.
/// Test: itself.
#[test]
fn a_missing_palace_is_created_with_cwd_and_no_force() {
    let daemon = StubDaemon::start(Reply::Created);
    let project = widget();
    let out = create_launch_palace_at(daemon.socket(), project.path(), PALACE, CREATE_BUDGET);
    assert_eq!(
        out,
        LaunchPalace::Created {
            palace: PALACE.into()
        }
    );
    assert_eq!(out.warning(), None);
    assert_one_create(&daemon.creates(), project.path());
}

/// Why: an existing palace is success, never a warning.
/// Test: itself.
#[test]
fn an_existing_palace_is_not_a_warning() {
    let daemon = StubDaemon::start(Reply::Exists);
    let project = widget();
    let out = create_launch_palace_at(daemon.socket(), project.path(), PALACE, CREATE_BUDGET);
    assert_eq!(
        out,
        LaunchPalace::Exists {
            palace: PALACE.into()
        }
    );
    assert_eq!(out.warning(), None);
}

/// Why (#11823): a create the daemon answers by linking to the folder-named
/// palace, or an id already held by a live alias, is success — never a warning.
/// Test: itself.
#[test]
fn an_alias_reply_is_success_not_a_warning() {
    let project = widget();
    for (reply, want) in [
        (
            Reply::Aliased,
            LaunchPalace::Aliased {
                palace: PALACE.into(),
                target: "widget".into(),
            },
        ),
        (
            Reply::ExistsAlias,
            LaunchPalace::Exists {
                palace: PALACE.into(),
            },
        ),
    ] {
        let daemon = StubDaemon::start(reply);
        let out = create_launch_palace_at(daemon.socket(), project.path(), PALACE, CREATE_BUDGET);
        assert_eq!(out, want);
        assert_eq!(out.warning(), None, "{out:?}");
    }
}

/// Why: -32603 is both "refused" and "internal fault"; either way the daemon
/// answered, so the warning names the palace and the daemon's reason, and
/// never reads as "unreachable".
/// Test: itself.
#[test]
fn a_refusal_warns_naming_the_palace_and_the_reason() {
    let daemon = StubDaemon::start(Reply::Refused);
    let project = widget();
    let out = create_launch_palace_at(daemon.socket(), project.path(), PALACE, CREATE_BUDGET);
    assert!(
        matches!(out, LaunchPalace::Refused { code: -32603, .. }),
        "{out:?}"
    );
    let warning = out.warning().expect("a refusal warns");
    assert!(warning.contains(PALACE), "{warning}");
    assert!(
        warning.contains("does not match the project palace"),
        "{warning}"
    );
    assert!(warning.contains("refused"), "{warning}");
    assert!(!warning.contains("could not be reached"), "{warning}");
}

/// Why: an absent daemon must warn, naming the palace, and return at once.
/// Test: itself.
#[test]
fn an_unreachable_daemon_warns_and_returns_within_the_budget() {
    let dir = crate::test_support::hermetic_temp_dir();
    let project = widget();
    let started = Instant::now();
    let out = create_launch_palace_at(
        &dir.path().join("absent.sock"),
        project.path(),
        PALACE,
        CREATE_BUDGET,
    );
    assert!(started.elapsed() < CREATE_BUDGET, "{:?}", started.elapsed());
    assert!(matches!(out, LaunchPalace::NotServing { .. }), "{out:?}");
    let warning = out.warning().expect("an absent daemon warns");
    assert!(warning.contains(PALACE), "{warning}");
    assert!(warning.contains("not running"), "{warning}");
}

/// Why: a daemon that accepts the connection and never answers must not
/// stall the launch past the budget.
/// Test: itself.
#[test]
fn a_hung_daemon_is_unreachable_within_the_budget() {
    let daemon = StubDaemon::start(Reply::Hang);
    let project = widget();
    let budget = Duration::from_millis(400);
    let started = Instant::now();
    let out = create_launch_palace_at(daemon.socket(), project.path(), PALACE, budget);
    assert!(
        started.elapsed() < budget + Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert!(matches!(out, LaunchPalace::Unreachable { .. }), "{out:?}");
    let warning = out.warning().expect("a hung daemon warns");
    assert!(warning.contains(PALACE), "{warning}");
    assert!(warning.contains("could not be reached"), "{warning}");
}

/// Why: a malformed pin blocks the create — no fallback name, no daemon call —
/// and the warning names the pin path.
/// Test: itself.
#[test]
#[serial_test::serial]
fn a_malformed_pin_blocks_the_create_and_sends_nothing() {
    let daemon = StubDaemon::start(Reply::Created);
    let _socket = EnvVarGuard::set(TRUSTY_MEMORY_SOCKET_ENV, daemon.socket());
    let _palace = EnvVarGuard::unset("TRUSTY_MEMORY_PALACE");
    let project = widget();
    let pin = project.path().join(".trusty-tools/trusty-memory.yaml");
    std::fs::create_dir_all(pin.parent().expect("parent")).expect("mkdir");
    std::fs::write(&pin, "palace: [unclosed\n").expect("write pin");

    let out = ensure_launch_palace(project.path());

    assert!(matches!(out, LaunchPalace::PinBlocked { .. }), "{out:?}");
    let warning = out.warning().expect("a malformed pin warns");
    assert!(warning.contains("trusty-memory.yaml"), "{warning}");
    let cwd = project.path().to_str().expect("utf-8");
    assert!(
        daemon.creates().iter().all(|c| c["cwd"] != cwd),
        "no create may be sent: {:?}",
        daemon.creates()
    );
}

/// Why: the standalone launch path (`prepare_session_inner`) must send the
/// create after its alias step; deleting the call fails this test.
/// Test: itself.
#[test]
#[serial_test::serial]
fn standalone_launch_sends_the_create() {
    let daemon = StubDaemon::start(Reply::Created);
    let _socket = EnvVarGuard::set(TRUSTY_MEMORY_SOCKET_ENV, daemon.socket());
    let _palace = EnvVarGuard::unset("TRUSTY_MEMORY_PALACE");
    let home = crate::test_support::hermetic_temp_dir();
    let _home = EnvVarGuard::set("HOME", home.path());
    let registry = crate::test_support::hermetic_temp_dir();
    let fw_base = crate::test_support::hermetic_temp_dir();
    let fw = crate::core::paths::FrameworkPaths::under(fw_base.path());
    std::fs::create_dir_all(&fw.root).expect("fw root");
    // Catch-up on with both network sources off, so it dials nothing.
    std::fs::write(
        fw.root.join("config.toml"),
        "[catchup]\nauto = true\ninclude_git = false\ninclude_palace = false\n",
    )
    .expect("config");
    let project = widget();

    super::super::prepare_session_inner(
        &fw,
        project.path(),
        None,
        false,
        None,
        None,
        super::super::HostInputs {
            home: Some(home.path()),
            palace_registry: Some(registry.path()),
            ..super::super::HostInputs::default()
        },
    )
    .expect("prep succeeds");

    assert_one_create(&daemon.creates(), project.path());
    assert_no_pin_written(project.path());
}

/// Assert the launch wrote no `<project>/.trusty-tools/trusty-memory.yaml` pin.
pub(crate) fn assert_no_pin_written(project: &Path) {
    let pin = project.join(".trusty-tools/trusty-memory.yaml");
    assert!(!pin.exists(), "the launch must not write a pin: {pin:?}");
}

/// Why (#11823): a tm started inside a session inherits `TRUSTY_MEMORY_PALACE`;
/// when it equals the project palace the create must still be sent.
/// Test: itself.
#[test]
#[serial_test::serial]
fn a_session_palace_equal_to_the_project_palace_still_sends_the_create() {
    let daemon = StubDaemon::start(Reply::Created);
    let _socket = EnvVarGuard::set(TRUSTY_MEMORY_SOCKET_ENV, daemon.socket());
    let _palace = EnvVarGuard::set("TRUSTY_MEMORY_PALACE", PALACE);
    let project = widget();

    let out = ensure_launch_palace(project.path());

    assert_eq!(
        out,
        LaunchPalace::Created {
            palace: PALACE.into()
        }
    );
    assert_one_create(&daemon.creates(), project.path());
}

/// Why (#11823): a differing `TRUSTY_MEMORY_PALACE` is a name mismatch; it
/// warns naming both palaces and the reason, sends nothing, and never skips
/// silently.
/// Test: itself.
#[test]
#[serial_test::serial]
fn a_differing_session_palace_warns_naming_both_and_sends_nothing() {
    let daemon = StubDaemon::start(Reply::Created);
    let _socket = EnvVarGuard::set(TRUSTY_MEMORY_SOCKET_ENV, daemon.socket());
    let _palace = EnvVarGuard::set("TRUSTY_MEMORY_PALACE", "other-palace");
    let project = widget();

    let out = ensure_launch_palace(project.path());

    assert_eq!(
        out,
        LaunchPalace::OverrideMismatch {
            palace: PALACE.into(),
            session: "other-palace".into(),
        }
    );
    let warning = out.warning().expect("a mismatch warns");
    assert!(warning.contains(PALACE), "{warning}");
    assert!(warning.contains("other-palace"), "{warning}");
    assert!(warning.contains("TRUSTY_MEMORY_PALACE"), "{warning}");
    let cwd = project.path().to_str().expect("utf-8");
    assert!(
        daemon.creates().iter().all(|c| c["cwd"] != cwd),
        "no create may be sent: {:?}",
        daemon.creates()
    );
}

/// Why: a daemon spawn runs both launch paths; the second ask for the same
/// project must not send (or warn) again.
/// Test: itself.
#[test]
fn a_second_ask_in_the_window_is_skipped() {
    let project = PathBuf::from("/tmp/11823-window");
    let now = Instant::now();
    assert!(first_ask_in_window(&project, PALACE, now));
    assert!(!first_ask_in_window(&project, PALACE, now));
    assert!(first_ask_in_window(&project, PALACE, now + REPEAT_WINDOW));
}
