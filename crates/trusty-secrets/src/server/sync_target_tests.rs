//! `secrets.copy` to a write-only sync target over real sockets (#9069).
//!
//! A child of `wire_tests`, so it shares that module's fixture and helpers:
//! every path is under a `TempDir`, the stores are in memory, and the socket
//! peer of every call is this test process, placed in a fake process table.
//!
//! Test: itself.

use super::*;
use crate::api::{OwnerName, RepoName};
use crate::store::RemoteIdentity;
#[cfg(all(unix, feature = "cli-backends"))]
use crate::store::github_actions::shim::GhShim;

/// The GitHub Actions sync target's id on the wire.
const GITHUB: &str = "github-actions";

/// `copy` params from the fixture's Keychain double to `to`.
fn push_params(fx: &Fixture, to: &str, keys: &[&str]) -> Value {
    json!({"project": fx.project(), "from_backend": "keychain", "to_backend": to, "keys": keys})
}

/// Why: D5 — a push with no key names would publish every project key; it
/// is refused before any target opens. Red while `copy` treats an empty
/// list as "every key" for a sync target.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_copy_to_sync_target_without_keys_is_invalid_params() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _grants) = state_with(&fx, procs_with_me());
    let server = fx.start_state(state).await;
    let refused = call(
        &fx.settings.socket,
        method::COPY,
        push_params(&fx, GITHUB, &[]),
    )
    .await;
    server.stop().await;

    assert_eq!(
        fixed_error(&refused, method::COPY),
        ErrorKind::InvalidParams
    );
    assert_no_value(&refused);
}

/// Why: D3 (owner ruling D63) — a pushed value is readable by any workflow
/// a repository writer adds, so a caller under Claude Code may not push,
/// whatever the key's "agents may use" flag says. One deny record names the
/// target and the verdict. Red while the push path has no ancestry check.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_copy_to_sync_target_by_agent_descendant_is_refused_and_audited() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, true);
    let procs = procs_with_me();
    procs.set_agent(10);
    let (state, _grants) = state_with(&fx, procs);
    let server = fx.start_state(state).await;
    let refused = call(
        &fx.settings.socket,
        method::COPY,
        push_params(&fx, GITHUB, &["API_KEY"]),
    )
    .await;
    server.stop().await;

    assert_eq!(
        fixed_error(&refused, method::COPY),
        ErrorKind::AgentUseRefused
    );
    assert_no_value(&refused);
    let all = records(&fx);
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(
        (all[0].method, all[0].decision),
        (AuditMethod::Copy, AuditDecision::Deny)
    );
    assert_eq!(
        all[0].reason.as_ref().map(AuditReason::as_str),
        Some("agent_use_refused")
    );
    assert_eq!(all[0].backend, Some(BackendId::new(GITHUB).unwrap()));
    assert_eq!(all[0].agent_parent, Some(true));
    assert_eq!(all[0].caller_pid, Some(me()));
}

/// Why: C8, D6 — a tracked project file may not aim a push at a repository
/// or project of its choosing, so a `github_actions:` or `vercel:` section
/// there is refused like the vendor CLI settings, naming the key only.
/// Red while those sections are ignored as unknown keys.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_tracked_sync_target_section_is_refused() {
    let fx = fixture();
    let config = fx.repo.join(PROJECT_CONFIG_SUBPATH);
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let server = fx.start().await;
    for section in ["github_actions", "vercel"] {
        std::fs::write(
            &config,
            format!("secrets:\n  {section}:\n    program: /tmp/{SENTINEL_VALUE}\n"),
        )
        .unwrap();
        let refused = call(
            &fx.settings.socket,
            method::SCOPES,
            json!({"project": fx.project()}),
        )
        .await;
        assert!(!wire(&refused).contains(SENTINEL_VALUE), "{section}");
        assert_eq!(
            fixed_error(&refused, method::SCOPES),
            ErrorKind::TrackedCliSettingRefused,
            "{section}"
        );
    }
    server.stop().await;
}

/// A write-only target that records each pushed key and value.
#[derive(Debug, Default)]
struct FakeTarget {
    pushed: Mutex<Vec<(String, String)>>,
}

impl SecretBackend for FakeTarget {
    fn id(&self) -> BackendId {
        BackendId::github_actions()
    }
    fn capabilities(&self) -> crate::store::Capabilities {
        crate::store::Capabilities::WRITE | crate::store::Capabilities::SYNC_TARGET
    }
    fn get(&self, _: &VaultName, _: &SecretKey) -> Result<Option<SecretValue>, SecretsError> {
        unreachable!("a sync target is never read")
    }
    fn set(&self, _: &VaultName, key: &SecretKey, value: &SecretValue) -> Result<(), SecretsError> {
        let pushed = (key.to_string(), value.expose().to_string());
        self.pushed.lock().unwrap().push(pushed);
        Ok(())
    }
    fn delete(&self, _: &VaultName, _: &SecretKey) -> Result<bool, SecretsError> {
        unreachable!("a sync target is never swept")
    }
}

/// `state` with a sync-target opener over `target` that counts its opens.
fn with_fake_target(mut state: State) -> (State, Arc<FakeTarget>, Arc<AtomicUsize>) {
    let target = Arc::new(FakeTarget::default());
    let opens = Arc::new(AtomicUsize::new(0));
    let (shared, counter) = (Arc::clone(&target), Arc::clone(&opens));
    state.sync_targets = Some(Arc::new(move |_: &BackendId, _: &RemoteIdentity| {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::clone(&shared) as Arc<dyn SecretBackend>)
    }));
    (state, target, opens)
}

/// Replace the fixture's machine config, which is also the account's file.
fn machine(fx: &Fixture, yaml: &str) {
    std::fs::write(&fx.settings.machine_config, yaml).unwrap();
}

/// Why: C6 — a sync target is never the backend values live in, whether the
/// tracked project file or the machine config names it; a `set` is refused
/// and nothing is stored or opened.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_sync_target_as_backend_is_refused() {
    let fx = fixture();
    let (state, target, opens) = with_fake_target(fx.state(fx.backends()));
    let server = fx.start_state(state).await;
    let config = fx.repo.join(PROJECT_CONFIG_SUBPATH);
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let set = || {
        call(
            &fx.settings.socket,
            method::SET,
            json!({"project": fx.project(), "vault": PROJECT_VAULT, "key": "K", "value": SENTINEL_VALUE}),
        )
    };
    for id in [GITHUB, "vercel"] {
        std::fs::write(&config, format!("secrets:\n  backend: {id}\n")).unwrap();
        let refused = set().await;
        assert_eq!(
            fixed_error(&refused, method::SET),
            ErrorKind::ConfigInvalid,
            "{id}"
        );
        assert_no_value(&refused);
    }
    std::fs::remove_file(&config).unwrap();
    machine(
        &fx,
        "secrets:\n  default_backend: github-actions\n  github_actions: {}\n",
    );
    let refused = set().await;
    assert_eq!(fixed_error(&refused, method::SET), ErrorKind::ConfigInvalid);
    server.stop().await;

    assert!(fx.keychain.is_empty() && target.pushed.lock().unwrap().is_empty());
    assert_eq!(opens.load(Ordering::SeqCst), 0);
}

/// Why: C7, D12 — a delete sweeps local and CLI-backed stores only; it never
/// opens a sync target, even on a machine that enables one.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_delete_never_opens_a_sync_target() {
    let fx = fixture();
    machine(
        &fx,
        "secrets:\n  default_backend: keychain\n  github_actions: {}\n",
    );
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _target, opens) = with_fake_target(fx.state(fx.backends()));
    let server = fx.start_state(state).await;
    let deleted = call(
        &fx.settings.socket,
        method::DELETE,
        json!({"project": fx.project(), "vault": PROJECT_VAULT, "key": "API_KEY"}),
    )
    .await;
    server.stop().await;

    assert_eq!(ok(deleted), json!({"removed": true}));
    assert_eq!(opens.load(Ordering::SeqCst), 0);
}

/// Why: C9 — a sandboxed server opens no sync target, whatever opener it
/// holds, so a sandboxed live check cannot push.
/// Test: itself.
#[test]
fn server_sandbox_never_opens_a_sync_target() {
    let fx = fixture();
    let (mut state, _target, opens) = with_fake_target(fx.state(fx.backends()));
    let identity = RemoteIdentity::new(
        OwnerName::new("acme").unwrap(),
        RepoName::new("web").unwrap(),
    );
    assert!(
        state
            .open_sync_target(&BackendId::github_actions(), &identity)
            .is_ok()
    );
    state.sandboxed = true;
    assert!(matches!(
        state.open_sync_target(&BackendId::github_actions(), &identity),
        Err(SecretsError::BackendNotEnabled { .. })
    ));
    assert_eq!(opens.load(Ordering::SeqCst), 1, "only the unsandboxed open");
}

/// Why: a sync target is write-only, so a copy from one is refused before
/// anything opens; nothing is ever read back.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_copy_from_a_sync_target_is_unsupported() {
    let fx = fixture();
    let (state, _target, opens) = with_fake_target(fx.state(fx.backends()));
    let server = fx.start_state(state).await;
    let refused = call(
        &fx.settings.socket,
        method::COPY,
        json!({"project": fx.project(), "from_backend": GITHUB, "to_backend": "keychain",
               "keys": ["API_KEY"]}),
    )
    .await;
    server.stop().await;

    assert_eq!(fixed_error(&refused, method::COPY), ErrorKind::Unsupported);
    assert_eq!(opens.load(Ordering::SeqCst), 0);
    assert!(fx.keychain.is_empty());
}

/// Why: D3 on the console route — a bridged push is judged on the console's
/// verdict about its HTTP client: an agent client is refused before the
/// target opens, a person's push goes through.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_copy_to_sync_target_uses_the_console_verdict() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let procs = procs_with_me();
    procs.set_executable(me(), "/opt/trusty/bin/trusty-console");
    let (state, _grants) = state_with(&fx, procs);
    let (state, target, opens) = with_fake_target(state);
    let server = fx.start_state(state).await;
    let bridged = |agent_parent: bool| {
        let mut params = push_params(&fx, GITHUB, &["API_KEY"]);
        params["bridge"] = json!({"agent_parent": agent_parent, "source": "mapped"});
        call(&fx.settings.socket, "secrets.bridge.copy", params)
    };
    let refused = bridged(true).await;
    let data = refused.error.as_ref().and_then(|e| e.data.clone());
    assert_eq!(data.unwrap()["kind"], "agent_use_refused");
    assert_eq!(opens.load(Ordering::SeqCst), 0);
    let pushed = ok(bridged(false).await);
    server.stop().await;

    assert_eq!(pushed, json!({"copied": ["API_KEY"], "failed": []}));
    let sent = target.pushed.lock().unwrap().clone();
    assert_eq!(sent, [("API_KEY".to_string(), SENTINEL_VALUE.to_string())]);
}

/// Why: C5 — the audit gate is fail-closed for a push as for any copy: an
/// unwritable log refuses before the target opens.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_copy_to_sync_target_with_unwritable_audit_spawns_nothing() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let dir = fx.settings.audit_log.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&dir).unwrap();
    set_mode(&dir, 0o755);
    let _restore = RestoreMode(dir);
    let (state, _grants) = state_with(&fx, procs_with_me());
    let (state, target, _opens) = with_fake_target(state);
    let server = fx.start_state(state).await;
    let refused = call(
        &fx.settings.socket,
        method::COPY,
        push_params(&fx, GITHUB, &["API_KEY"]),
    )
    .await;
    server.stop().await;

    assert_eq!(
        fixed_error(&refused, method::COPY),
        ErrorKind::AuditUnavailable
    );
    assert!(
        target.pushed.lock().unwrap().is_empty(),
        "nothing was pushed"
    );
}

/// Set in the child process that [`server_copy_to_sync_target_stops_after_a_key_whose_record_cannot_be_appended`]
/// spawns; names the one test body that may run there.
#[cfg(unix)]
const FSIZE_CHILD: &str = "TRUSTY_SECRETS_TEST_9069_FSIZE_CHILD";
/// The child's file-size limit, and its pre-filled audit log's size.
#[cfg(unix)]
const FSIZE_LIMIT: u64 = 1024 * 1024 * 1024;

/// Why: C-7.7a for the push loop — `push` has its own loop beside
/// `methods::copy`'s, so a push whose record cannot be appended after the
/// target took the value must stop before the next key and answer
/// `audit_unavailable`. Red when `push` drops the `?` on `Gate::record_key`.
/// What: the audit log can fail only after it opened, which needs
/// `RLIMIT_FSIZE`, a limit for the whole process. So this test re-runs the
/// test binary on [`fsize_limited_push_child`] and checks that it passed.
/// Test: itself.
#[cfg(unix)]
#[test]
fn server_copy_to_sync_target_stops_after_a_key_whose_record_cannot_be_appended() {
    let exe = std::env::current_exe().unwrap();
    let out = std::process::Command::new(exe)
        .args([
            "fsize_limited_push_child",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(FSIZE_CHILD, "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "child failed:\n{stdout}\n{stderr}");
    assert!(stdout.contains("1 passed"), "child did not run:\n{stdout}");
}

/// The body of the re-run above; a no-op outside that child.
/// Test: `server_copy_to_sync_target_stops_after_a_key_whose_record_cannot_be_appended`.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fsize_limited_push_child() {
    use std::io::{Seek, SeekFrom};
    use std::os::unix::fs::OpenOptionsExt;

    if std::env::var_os(FSIZE_CHILD).is_none() {
        return;
    }
    // SAFETY: plain values and pointers to live locals; the effect is on
    // this child process alone.
    unsafe {
        libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
        let mut current = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(libc::getrlimit(libc::RLIMIT_FSIZE, &mut current), 0);
        let limit = libc::rlimit {
            rlim_cur: FSIZE_LIMIT as libc::rlim_t,
            rlim_max: current.rlim_max,
        };
        assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &limit), 0);
    }
    let mut fx = fixture();
    // Rotation is off, so open never renames the full log.
    fx.settings.audit_max_bytes = u64::MAX;
    let log = fx.settings.audit_log.clone();
    let dir = log.parent().unwrap();
    std::fs::create_dir(dir).unwrap();
    set_mode(dir, 0o700);
    // A sparse 0600 log of exactly the limit, ending in `\n`: it opens and
    // checks clean, and its first append fails with `EFBIG`.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&log)
        .unwrap();
    file.set_len(FSIZE_LIMIT - 1).unwrap();
    file.seek(SeekFrom::End(0)).unwrap();
    std::io::Write::write_all(&mut file, b"\n").unwrap();
    drop(file);
    for name in ["A", "B", "C"] {
        seed(&fx, name, SENTINEL_VALUE, false);
    }
    let (state, _grants) = state_with(&fx, procs_with_me());
    let (state, target, _opens) = with_fake_target(state);
    let server = fx.start_state(state).await;
    let push = |keys: &[&str]| {
        call(
            &fx.settings.socket,
            method::COPY,
            push_params(&fx, GITHUB, keys),
        )
    };
    // The last key: only `record_key`'s own result can refuse it.
    let last = push(&["A"]).await;
    // Two keys: `Gate::ready` or `record_key` must stop before key 2.
    let two = push(&["B", "C"]).await;
    server.stop().await;

    for refused in [&last, &two] {
        assert_eq!(
            fixed_error(refused, method::COPY),
            ErrorKind::AuditUnavailable
        );
        assert_no_value(refused);
    }
    let names: Vec<_> = target
        .pushed
        .lock()
        .unwrap()
        .iter()
        .map(|p| p.0.clone())
        .collect();
    assert_eq!(names, ["A", "B"], "each call pushed key 1; C never started");
    assert_eq!(std::fs::metadata(&log).unwrap().len(), FSIZE_LIMIT);
}

/// Every file under the fixture's index directory, by path, with its bytes.
fn index_snapshot(fx: &Fixture) -> BTreeSet<(PathBuf, Vec<u8>)> {
    std::fs::read_dir(&fx.settings.index_root)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = std::fs::read(&path).unwrap_or_default();
            (path, bytes)
        })
        .collect()
}

/// Why: D4 — the index describes local values, so a push neither adds a row
/// nor bumps the pushed key's `updated_at`.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_copy_to_sync_target_leaves_the_index_untouched() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let before = index_snapshot(&fx);
    let (state, _grants) = state_with(&fx, procs_with_me());
    let (state, target, _opens) = with_fake_target(state);
    let server = fx.start_state(state).await;
    // One key the index lists, and one only the Keychain double holds.
    fx.keychain
        .set(
            &vault(PROJECT_VAULT),
            &key("UNINDEXED"),
            &SecretValue::new(OPEN_VALUE),
        )
        .unwrap();
    let pushed = ok(call(
        &fx.settings.socket,
        method::COPY,
        push_params(&fx, GITHUB, &["API_KEY", "UNINDEXED"]),
    )
    .await);
    server.stop().await;

    assert_eq!(
        pushed,
        json!({"copied": ["API_KEY", "UNINDEXED"], "failed": []})
    );
    assert_eq!(target.pushed.lock().unwrap().len(), 2);
    assert_eq!(index_snapshot(&fx), before);
}

/// The machine config enabling the GitHub sync target over `shim`.
#[cfg(all(unix, feature = "cli-backends"))]
fn github_enabled(shim: &GhShim, extra: &str) -> String {
    format!(
        "secrets:\n  default_backend: keychain\n  github_actions:\n    program: {}\n{extra}",
        shim.program().display()
    )
}

/// Why: G1, D6 — a machine `project_vaults` override changes which vault a
/// push reads, never which repository it writes: the production opener
/// reads the account's own file and pushes to the origin remote.
/// Test: itself.
#[cfg(all(unix, feature = "cli-backends"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn github_push_targets_the_remote_repo_not_the_vault_override() {
    let fx = fixture();
    let shim = GhShim::new();
    let shared = "trusty/acme/shared";
    machine(
        &fx,
        &github_enabled(
            &shim,
            &format!("  project_vaults:\n    acme/web: {shared}\n"),
        ),
    );
    fx.keychain
        .set(
            &vault(shared),
            &key("API_KEY"),
            &SecretValue::new(SENTINEL_VALUE),
        )
        .unwrap();
    let (state, _grants) = state_with(&fx, procs_with_me());
    let server = fx.start_state(state).await;
    let pushed = call(
        &fx.settings.socket,
        method::COPY,
        push_params(&fx, GITHUB, &["API_KEY"]),
    )
    .await;
    server.stop().await;

    assert!(!wire(&pushed).contains(SENTINEL_VALUE));
    assert_eq!(ok(pushed), json!({"copied": ["API_KEY"], "failed": []}));
    assert_eq!(shim.stdin("API_KEY").as_deref(), Some(SENTINEL_VALUE));
    let calls = shim.calls();
    assert!(calls.contains("--repo github.com/acme/web "), "{calls}");
    assert!(!calls.contains("shared"), "{calls}");
}

/// Why: C4, C5 — each key leaves one record naming the target; a key `gh`
/// rejects lands in `failed` with a deny record, and the next key still
/// runs. Neither the reply nor the audit file holds a value.
/// Test: itself.
#[cfg(all(unix, feature = "cli-backends"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_copy_to_sync_target_leaves_one_record_per_key_naming_the_target() {
    let fx = fixture();
    let shim = GhShim::new();
    shim.fail_push("B");
    machine(&fx, &github_enabled(&shim, ""));
    for name in ["A", "B", "C"] {
        seed(&fx, name, SENTINEL_VALUE, false);
    }
    let (state, _grants) = state_with(&fx, procs_with_me());
    let server = fx.start_state(state).await;
    let pushed = call(
        &fx.settings.socket,
        method::COPY,
        push_params(&fx, GITHUB, &["A", "B", "C"]),
    )
    .await;
    server.stop().await;

    assert!(!wire(&pushed).contains(SENTINEL_VALUE));
    assert_eq!(ok(pushed), json!({"copied": ["A", "C"], "failed": ["B"]}));
    assert_eq!(
        shim.stdin("C").as_deref(),
        Some(SENTINEL_VALUE),
        "C ran after B failed"
    );
    let audit = std::fs::read_to_string(&fx.settings.audit_log).unwrap();
    assert!(!audit.contains(SENTINEL_VALUE));
    let all: Vec<_> = records(&fx)
        .into_iter()
        .filter(|r| r.method == AuditMethod::Copy)
        .collect();
    let shapes: Vec<_> = all.iter().map(shape).collect();
    assert_eq!(
        shapes,
        [
            (AuditMethod::Copy, AuditDecision::Allow, None, Some("A")),
            (
                AuditMethod::Copy,
                AuditDecision::Deny,
                Some("backend_failed"),
                Some("B")
            ),
            (AuditMethod::Copy, AuditDecision::Allow, None, Some("C")),
        ]
    );
    for record in &all {
        assert_eq!(record.backend, Some(BackendId::github_actions()));
        assert_eq!(record.agent_parent, Some(false));
    }
}

/// Why: §3.1 item 7, D15 — doctor lists the GitHub sync target from the
/// account's own file by detection only: the program is found, never run.
/// Without the section the row says which section to add.
/// Test: itself.
#[cfg(all(unix, feature = "cli-backends"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_doctor_lists_sync_targets_without_spawning() {
    let fx = fixture();
    let shim = GhShim::new();
    let server = fx.start().await;
    let row = || async {
        let doctor: DoctorResponse =
            serde_json::from_value(ok(call(&fx.settings.socket, DOCTOR, Value::Null).await))
                .unwrap();
        doctor
            .backends
            .into_iter()
            .find(|b| b.id.as_str() == GITHUB)
            .expect("a github-actions row")
    };
    let off = row().await;
    assert_eq!(
        (off.available, off.reason),
        (false, Some(Unavailable::NotEnabled))
    );
    assert!(off.detail.unwrap().contains("`secrets.github_actions:`"));

    machine(&fx, &github_enabled(&shim, ""));
    let on = row().await;
    server.stop().await;
    assert!(on.available, "{on:?}");
    assert_eq!(on.capabilities, ["WRITE", "SYNC_TARGET"]);
    assert_eq!(shim.calls(), "", "doctor spawned nothing");
}

/// Why: D3 fail-open check — an ancestry that cannot be read is a refusal,
/// never "no agent": the push is `grant_refused`, the target never opens,
/// and one deny record is left.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_copy_to_sync_target_with_unreadable_ancestry_is_refused() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let procs = procs_with_me();
    procs.set_unreadable();
    let (state, _grants) = state_with(&fx, procs);
    let (state, target, opens) = with_fake_target(state);
    let server = fx.start_state(state).await;
    let refused = call(
        &fx.settings.socket,
        method::COPY,
        push_params(&fx, GITHUB, &["API_KEY"]),
    )
    .await;
    server.stop().await;

    assert_eq!(fixed_error(&refused, method::COPY), ErrorKind::GrantRefused);
    assert_eq!(opens.load(Ordering::SeqCst), 0);
    assert!(target.pushed.lock().unwrap().is_empty());
    let all = records(&fx);
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(all[0].decision, AuditDecision::Deny);
}
