//! The credential access audit trail over real sockets (#4567).
//!
//! A child of `server_tests`, so it shares that module's fixture: every path
//! is under a `TempDir`, including the audit log, and the backends are
//! in-memory. Each test names the acceptance criterion it proves.
//!
//! Test: itself.

use super::*;
use crate::server::audit::AuditSink;
use crate::store::INDEX_SUBDIR;

/// A value whose 8-character windows share nothing with the vault, key or
/// path text a record legitimately carries.
const FAKE_VALUE: &str = "FAKE-VALUE-q7Zx93LmWp2Rt8Kd";

fn audit_log(fx: &Fixture) -> &Path {
    &fx.settings.audit_log
}

/// Every record in the fixture's audit log, in order; none when it is absent.
fn records(fx: &Fixture) -> Vec<AuditRecord> {
    match std::fs::read_to_string(audit_log(fx)) {
        Ok(text) => text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => panic!("{e}"),
    }
}

/// The record's (method, decision, reason) triple.
fn shape(record: &AuditRecord) -> (AuditMethod, AuditDecision, Option<&str>) {
    (
        record.method,
        record.decision,
        record.reason.as_ref().map(AuditReason::as_str),
    )
}

/// Give the audit directory a mode the sink refuses, so no record can be
/// written. Returns the guard that restores 0700.
fn make_sink_unwritable(fx: &Fixture) -> RestoreMode {
    let dir = audit_log(fx).parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&dir).unwrap();
    set_mode(&dir, 0o755);
    RestoreMode(dir)
}

fn set_params(fx: &Fixture, vault: &str, key: &str) -> Value {
    json!({"project": fx.project(), "vault": vault, "key": key, "value": FAKE_VALUE})
}

fn target(fx: &Fixture, vault: &str, key: &str) -> Value {
    json!({"project": fx.project(), "vault": vault, "key": key})
}

/// Why: AC1 — the record holds typed fields only, so no value can reach the
/// audit file. Red when a record carries the value (proof A6).
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audit_value_never_reaches_the_audit_file() {
    let fx = fixture();
    let server = fx.start().await;
    ok(call(
        &fx.settings.socket,
        method::SET,
        set_params(&fx, "trusty/acme/web", "API_KEY"),
    )
    .await);
    server.stop().await;

    let text = std::fs::read_to_string(audit_log(&fx)).unwrap();
    for start in 0..=FAKE_VALUE.len() - 8 {
        let window = &FAKE_VALUE[start..start + 8];
        assert!(!text.contains(window), "audit file carries {window:?}");
    }
    let all = records(&fx);
    assert_eq!(all.len(), 1);
    let record = &all[0];
    assert_eq!(record.stream, AuditStream::CredentialAccess);
    assert_eq!(
        shape(record),
        (AuditMethod::Set, AuditDecision::Allow, None)
    );
    assert_eq!(record.vault, Some(vault("trusty/acme/web")));
    assert_eq!(record.key, Some(key("API_KEY")));
    assert_eq!(record.backend, Some(BackendId::keychain()));
    assert!(record.project_root.as_ref().unwrap().ends_with("repo"));
    assert_eq!(record.caller_pid, None);
    assert!(record.ts > 0);
    assert!(text.contains(&format!("\"stream\":\"{AUDIT_STREAM}\"")));
}

/// Why: AC2 — set and delete leave exactly one record per call, allow or
/// deny; an out-of-scope set is one `vault_out_of_scope` deny and no write.
/// Red when the out-of-scope path writes no record (proof A1).
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audit_set_and_delete_write_one_record_per_call() {
    let fx = fixture();
    let server = fx.start().await;
    let socket = &fx.settings.socket;
    let outside = call(
        socket,
        method::SET,
        set_params(&fx, "trusty/acme/other", "K"),
    )
    .await;
    assert_eq!(
        fixed_error(&outside, method::SET),
        ErrorKind::VaultOutOfScope
    );
    assert!(fx.keychain.is_empty(), "no backend write");
    ok(call(socket, method::SET, set_params(&fx, "trusty/acme/web", "K")).await);
    ok(call(socket, method::DELETE, target(&fx, "trusty/acme/web", "K")).await);
    let refused = call(
        socket,
        method::DELETE,
        target(&fx, "trusty/acme/other", "K"),
    )
    .await;
    assert_eq!(
        fixed_error(&refused, method::DELETE),
        ErrorKind::VaultOutOfScope
    );
    let malformed = call(socket, method::SET, json!({"project": fx.project()})).await;
    assert_eq!(
        fixed_error(&malformed, method::SET),
        ErrorKind::InvalidParams
    );
    server.stop().await;

    let all = records(&fx);
    let shapes: Vec<_> = all.iter().map(shape).collect();
    use AuditDecision::{Allow, Deny};
    assert_eq!(
        shapes,
        [
            (AuditMethod::Set, Deny, Some("vault_out_of_scope")),
            (AuditMethod::Set, Allow, None),
            (AuditMethod::Delete, Allow, None),
            (AuditMethod::Delete, Deny, Some("vault_out_of_scope")),
            (AuditMethod::Set, Deny, Some("invalid_params")),
        ]
    );
    assert_eq!(all[0].vault, Some(vault("trusty/acme/other")));
    assert_eq!((&all[4].vault, &all[4].key), (&None, &None));
}

/// Why: AC3 — copy leaves one record per key: allow when copied, deny with
/// its kind when it lands in `failed`; a refusal before the loop is one deny.
/// Red when the keys share one record (proof A5).
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audit_copy_writes_one_record_per_key() {
    let fx = fixture();
    let web = vault("trusty/acme/web");
    for name in ["A", "B"] {
        fx.keychain
            .set(&web, &key(name), &SecretValue::new(FAKE_VALUE))
            .unwrap();
    }
    let server = fx.start().await;
    let socket = &fx.settings.socket;
    let out = ok(call(
        socket,
        method::COPY,
        json!({"project": fx.project(), "from_backend": "keychain", "to_backend": "spare",
               "keys": ["A", "MISSING", "B"]}),
    )
    .await);
    assert_eq!(out, json!({"copied": ["A", "B"], "failed": ["MISSING"]}));
    let same = call(
        socket,
        method::COPY,
        json!({"project": fx.project(), "from_backend": "spare", "to_backend": "spare"}),
    )
    .await;
    assert_eq!(fixed_error(&same, method::COPY), ErrorKind::SameBackend);
    server.stop().await;

    let all = records(&fx);
    let per_key: Vec<_> = all
        .iter()
        .map(|r| (r.key.as_ref().map(SecretKey::as_str), shape(r)))
        .collect();
    use AuditDecision::{Allow, Deny};
    assert_eq!(
        per_key,
        [
            (Some("A"), (AuditMethod::Copy, Allow, None)),
            (
                Some("MISSING"),
                (AuditMethod::Copy, Deny, Some("not_found"))
            ),
            (Some("B"), (AuditMethod::Copy, Allow, None)),
            (None, (AuditMethod::Copy, Deny, Some("same_backend"))),
        ]
    );
    assert_eq!(all[0].backend, Some(BackendId::new("spare").unwrap()));
    assert_eq!(all[0].vault, Some(web));
}

/// Why: AC4 — list is recorded only when denied; scopes and doctor never.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audit_list_records_only_denials_and_scopes_doctor_none() {
    let fx = fixture();
    let server = fx.start().await;
    let socket = &fx.settings.socket;
    let project = json!({"project": fx.project()});
    ok(call(socket, method::SCOPES, project.clone()).await);
    ok(call(socket, DOCTOR, project).await);
    ok(call(socket, DOCTOR, Value::Null).await);
    ok(call(
        socket,
        method::LIST,
        json!({"project": fx.project(), "vault": "trusty/acme/web"}),
    )
    .await);
    assert!(records(&fx).is_empty(), "allowed reads leave no record");
    let refused = call(
        socket,
        method::LIST,
        json!({"project": fx.project(), "vault": "trusty/elsewhere"}),
    )
    .await;
    assert_eq!(
        fixed_error(&refused, method::LIST),
        ErrorKind::VaultOutOfScope
    );
    server.stop().await;
    let all = records(&fx);
    assert_eq!(all.len(), 1);
    assert_eq!(
        shape(&all[0]),
        (
            AuditMethod::List,
            AuditDecision::Deny,
            Some("vault_out_of_scope")
        )
    );
    assert_eq!(all[0].key, None);
}

/// Serve `fx` until it exits idle, after `calls` run against it.
async fn serve_until_idle(fx: &Fixture, calls: &[(&'static str, Value)]) {
    let (_tx, rx) = oneshot::channel::<()>();
    let task = tokio::spawn(serve(fx.settings.clone(), fx.backends(), async move {
        let _ = rx.await;
    }));
    wait_serving(&fx.settings.socket).await;
    for (name, params) in calls {
        ok(call(&fx.settings.socket, name, params.clone()).await);
    }
    let exit = tokio::time::timeout(Duration::from_secs(15), task)
        .await
        .expect("server exits inside the bound")
        .unwrap()
        .unwrap();
    assert_eq!(exit, ServeExit::Idle);
    assert!(!fx.settings.socket.exists());
}

/// Why: AC5 — records are written before the reply, so they survive the
/// on-demand server's idle exit; a second instance appends, never truncates.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audit_records_survive_idle_exit_and_a_second_instance_appends() {
    use std::os::unix::fs::PermissionsExt;
    let fx = fixture_with_idle(Duration::from_secs(1));
    serve_until_idle(
        &fx,
        &[(method::SET, set_params(&fx, "trusty/acme/web", "K"))],
    )
    .await;
    let first = std::fs::read_to_string(audit_log(&fx)).unwrap();
    assert_eq!(records(&fx).len(), 1);
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode(audit_log(&fx)), 0o600);
    assert_eq!(mode(audit_log(&fx).parent().unwrap()), 0o700);

    serve_until_idle(
        &fx,
        &[(method::DELETE, target(&fx, "trusty/acme/web", "K"))],
    )
    .await;
    let second = std::fs::read_to_string(audit_log(&fx)).unwrap();
    assert!(
        second.starts_with(&first),
        "the first instance's record kept"
    );
    let methods: Vec<_> = records(&fx).iter().map(|r| r.method).collect();
    assert_eq!(methods, [AuditMethod::Set, AuditMethod::Delete]);
}

/// Why: AC5 — the sink creates 0600 in 0700 and refuses a symlink, a wrong
/// mode, or a wrong file type, as the #9326 file backend does.
/// Test: itself.
#[test]
fn audit_sink_creates_0600_in_0700_and_refuses_a_wrong_mode() {
    let tmp = TempDir::new().unwrap();
    let log = tmp.path().join("a").join("b").join("audit.jsonl");
    let sink = AuditSink::new(log.clone(), DEFAULT_AUDIT_MAX_BYTES);
    let record = AuditRecord::new(1, AuditMethod::Set, None);
    sink.open().unwrap().append(&record).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode(&log), 0o600);
        assert_eq!(mode(log.parent().unwrap()), 0o700);
    }

    set_mode(&log, 0o644);
    let refused = sink.open().unwrap_err();
    assert!(
        matches!(refused, SecretsError::StorageRefused { .. }),
        "{refused:?}"
    );
    set_mode(&log, 0o600);

    let elsewhere = tmp.path().join("elsewhere.jsonl");
    std::fs::write(&elsewhere, "").unwrap();
    set_mode(&elsewhere, 0o600);
    std::fs::remove_file(&log).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &log).unwrap();
    let refused = sink.open().unwrap_err();
    assert!(
        matches!(refused, SecretsError::StorageRefused { .. }),
        "{refused:?}"
    );
    assert_eq!(std::fs::read_to_string(&elsewhere).unwrap(), "");
}

/// Why: AC5 — nothing is resident to rotate on a timer, so the cap is
/// applied when the log is opened; an open below the cap never truncates.
/// Test: itself.
#[test]
fn audit_sink_rotates_at_open_and_never_truncates() {
    let tmp = TempDir::new().unwrap();
    let log = tmp.path().join("audit").join("audit.jsonl");
    let record = AuditRecord::new(1, AuditMethod::Delete, Some(ErrorKind::NotFound));
    let line = serde_json::to_vec(&record).unwrap().len() as u64 + 1;
    let sink = AuditSink::new(log.clone(), line * 3);
    for _ in 0..3 {
        sink.open().unwrap().append(&record).unwrap();
    }
    let full = std::fs::read_to_string(&log).unwrap();
    assert_eq!(full.lines().count(), 3, "no open below the cap truncated");

    sink.open().unwrap().append(&record).unwrap();
    let rotated = log.with_file_name("audit.jsonl.1");
    assert_eq!(std::fs::read_to_string(&rotated).unwrap(), full);
    assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 1);
}

/// Why: AC6, allow arm — fail-closed: with the sink unwritable, an allowed
/// set, delete or copy returns `audit_unavailable` and changes no backend.
/// Red when an allowed set proceeds without its record (proof A2).
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audit_unwritable_sink_refuses_an_allowed_set_before_the_backend() {
    let fx = fixture();
    let web = vault("trusty/acme/web");
    fx.keychain
        .set(&web, &key("KEEP"), &SecretValue::new(FAKE_VALUE))
        .unwrap();
    let _restore = make_sink_unwritable(&fx);
    let server = fx.start().await;
    let socket = &fx.settings.socket;
    let set = call(
        socket,
        method::SET,
        set_params(&fx, "trusty/acme/web", "NEW"),
    )
    .await;
    assert_eq!(fixed_error(&set, method::SET), ErrorKind::AuditUnavailable);
    let text = wire(&set);
    assert!(
        !text.contains(FAKE_VALUE) && !text.contains("audit.jsonl"),
        "{text}"
    );
    assert!(fx.keychain.get(&web, &key("NEW")).unwrap().is_none());
    let delete = call(
        socket,
        method::DELETE,
        target(&fx, "trusty/acme/web", "KEEP"),
    )
    .await;
    assert_eq!(
        fixed_error(&delete, method::DELETE),
        ErrorKind::AuditUnavailable
    );
    assert!(fx.keychain.get(&web, &key("KEEP")).unwrap().is_some());
    let copy = call(
        socket,
        method::COPY,
        json!({"project": fx.project(), "from_backend": "keychain", "to_backend": "spare",
               "keys": ["KEEP"]}),
    )
    .await;
    assert_eq!(
        fixed_error(&copy, method::COPY),
        ErrorKind::AuditUnavailable
    );
    assert!(fx.spare.is_empty());
    server.stop().await;
    assert_eq!(ErrorKind::AuditUnavailable.code(), -32072);
    assert_eq!(ErrorKind::AuditUnavailable.as_str(), "audit_unavailable");
}

/// Why: AC6, deny arm — best-effort: with the sink unwritable, a refusal is
/// still answered with its own kind, never `audit_unavailable`.
/// Red when a deny-path audit failure fails the reply (proof A3).
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audit_unwritable_sink_still_returns_the_deny_reply() {
    let fx = fixture();
    let _restore = make_sink_unwritable(&fx);
    let server = fx.start().await;
    let socket = &fx.settings.socket;
    let cases = [
        (
            method::SET,
            set_params(&fx, "trusty/acme/other", "K"),
            ErrorKind::VaultOutOfScope,
        ),
        (
            method::DELETE,
            json!({"project": fx.project()}),
            ErrorKind::InvalidParams,
        ),
        (
            method::LIST,
            json!({"project": fx.project(), "vault": "trusty/elsewhere"}),
            ErrorKind::VaultOutOfScope,
        ),
        (
            method::COPY,
            json!({"project": fx.project(), "from_backend": "spare", "to_backend": "spare"}),
            ErrorKind::SameBackend,
        ),
    ];
    for (name, params, expected) in cases {
        let response = call(socket, name, params).await;
        assert_eq!(fixed_error(&response, name), expected, "{name}");
    }
    assert!(fx.keychain.is_empty());
    server.stop().await;
}

/// Why: AC7 — only the untracked machine config suppresses the audit; then
/// no write is attempted, so an unwritable sink cannot refuse a set.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audit_machine_suppression_lets_set_succeed_with_an_unwritable_sink() {
    let fx = fixture();
    std::fs::write(
        &fx.settings.machine_config,
        "secrets:\n  default_backend: keychain\n  audit: false\n",
    )
    .unwrap();
    let _restore = make_sink_unwritable(&fx);
    let server = fx.start().await;
    ok(call(
        &fx.settings.socket,
        method::SET,
        set_params(&fx, "trusty/acme/web", "K"),
    )
    .await);
    server.stop().await;
    assert_eq!(fx.keychain.len(), 1);
    assert!(!audit_log(&fx).exists(), "no write was attempted");
}

/// Why: AC7 — a tracked project config may not suppress the audit. It is
/// refused (the #9326 `tracked_backend_refused` precedent) and the refusal
/// is itself recorded. Red when the tracked key suppresses (proof A4).
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audit_tracked_project_config_cannot_suppress_the_audit() {
    let fx = fixture();
    let config = fx.repo.join(PROJECT_CONFIG_SUBPATH);
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, "secrets:\n  audit: false\n").unwrap();
    let server = fx.start().await;
    let set = call(
        &fx.settings.socket,
        method::SET,
        set_params(&fx, "trusty/acme/web", "K"),
    )
    .await;
    assert_eq!(
        fixed_error(&set, method::SET),
        ErrorKind::TrackedAuditRefused
    );
    assert!(wire(&set).contains("secrets.audit"));
    server.stop().await;
    assert!(fx.keychain.is_empty());
    let all = records(&fx);
    assert_eq!(all.len(), 1, "the audit was still written");
    assert_eq!(
        shape(&all[0]),
        (
            AuditMethod::Set,
            AuditDecision::Deny,
            Some("tracked_audit_refused")
        )
    );
    assert_eq!(ErrorKind::TrackedAuditRefused.code(), -32073);
}

/// Why: AC8 — the #9326 refusals are deny records: `storage_refused` from the
/// file backend's checks, and `tracked_backend_refused` on a Keychain build.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audit_storage_and_tracked_backend_refusals_are_deny_records() {
    let fx = fixture();
    std::fs::write(
        &fx.settings.machine_config,
        "secrets:\n  default_backend: file\n",
    )
    .unwrap();
    let (factory, file) = with_file_backend(&fx);
    std::fs::create_dir_all(file.root()).unwrap();
    set_mode(file.root(), 0o755);
    let _restore = RestoreMode(file.root().to_path_buf());
    let server = fx.start_with(factory).await;
    let set = call(
        &fx.settings.socket,
        method::SET,
        set_params(&fx, "trusty/acme/web", "K"),
    )
    .await;
    assert_eq!(fixed_error(&set, method::SET), ErrorKind::StorageRefused);

    let config = fx.repo.join(PROJECT_CONFIG_SUBPATH);
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, "secrets:\n  backend: file\n").unwrap();
    let tracked = call(
        &fx.settings.socket,
        method::DELETE,
        target(&fx, "trusty/acme/web", "K"),
    )
    .await;
    server.stop().await;

    let all = records(&fx);
    assert_eq!(
        shape(&all[0]),
        (
            AuditMethod::Set,
            AuditDecision::Deny,
            Some("storage_refused")
        )
    );
    assert_eq!(all[0].backend, Some(BackendId::file()));
    if cfg!(target_os = "macos") {
        assert_eq!(
            fixed_error(&tracked, method::DELETE),
            ErrorKind::TrackedBackendRefused
        );
        assert_eq!(
            shape(&all[1]),
            (
                AuditMethod::Delete,
                AuditDecision::Deny,
                Some("tracked_backend_refused")
            )
        );
    }
    assert_eq!(all.len(), 2);
}

/// Why: AC9 — S8 fills `caller_pid`, and a newer server may write a reason
/// or method this build does not know; both must round-trip.
/// Test: itself.
#[test]
fn audit_record_round_trips_a_pid_and_an_unknown_reason() {
    let record = AuditRecord::new(7, AuditMethod::Set, None).with_caller_pid(Some(4242));
    let back: AuditRecord = serde_json::from_str(&serde_json::to_string(&record).unwrap()).unwrap();
    assert_eq!(back, record);
    assert_eq!(back.caller_pid, Some(4242));

    let newer = r#"{"ts":9,"stream":"credential_access","method":"secrets.rotate",
        "decision":"deny","reason":"a_newer_kind","caller_pid":17,"extra":true}"#;
    let read: AuditRecord = serde_json::from_str(newer).unwrap();
    assert_eq!(read.method, AuditMethod::Other);
    assert_eq!(read.reason.as_ref().unwrap().as_str(), "a_newer_kind");
    assert_eq!(read.reason.as_ref().unwrap().kind(), None);
    assert_eq!(read.vault, None);
    let again: Value = serde_json::to_value(&read).unwrap();
    assert_eq!(again["reason"], "a_newer_kind");
    assert_eq!(again["caller_pid"], 17);

    let known = AuditReason::from(ErrorKind::VaultOutOfScope);
    assert_eq!(known.kind(), Some(ErrorKind::VaultOutOfScope));
    for (method_name, audited) in [
        (method::SET, AuditMethod::Set),
        (method::DELETE, AuditMethod::Delete),
        (method::COPY, AuditMethod::Copy),
        (method::LIST, AuditMethod::List),
    ] {
        assert_eq!(serde_json::to_value(audited).unwrap(), method_name);
    }
}

/// Why: AC5 — the default audit log is
/// `~/.trusty-tools/trusty-secrets/audit/audit.jsonl`, and a redirected index
/// takes the audit log with it out of `$HOME`.
/// Test: itself.
#[test]
fn settings_audit_log_defaults_beside_the_index() {
    let home = PathBuf::from("/home/u");
    assert_eq!(
        settings::audit_log_beside(&home.join(INDEX_SUBDIR)),
        home.join(AUDIT_LOG_SUBPATH)
    );
    let parsed = parse_settings(
        ["serve", "--index-dir", "/x/index"].map(std::ffi::OsString::from),
        |_| None,
    )
    .unwrap();
    assert_eq!(parsed.audit_log, PathBuf::from("/x/audit/audit.jsonl"));
    assert_eq!(parsed.audit_max_bytes, DEFAULT_AUDIT_MAX_BYTES);
}

/// Why: F3 of the #4567 review, DOC-45 C-7.12 — the on-demand client passes
/// the caller's environment through, so no environment variable may move the
/// audit trail into a project tree: neither `TRUSTY_SECRETS_AUDIT_LOG` nor
/// `TRUSTY_SECRETS_INDEX_DIR`. Only the `--index-dir` flag moves it.
/// Red when `from_args` places the log beside an index taken from the
/// environment.
/// Test: itself.
#[test]
fn settings_ignore_an_audit_log_environment_variable() {
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let env = |name: &str| match name {
        "TRUSTY_SECRETS_AUDIT_LOG" => Some("/repo/checkout/audit.jsonl".to_string()),
        INDEX_DIR_ENV => Some("/repo/checkout/index".to_string()),
        SOCKET_ENV => Some("/x/s.sock".to_string()),
        _ => None,
    };
    let parsed = parse_settings(["serve"].map(std::ffi::OsString::from), env).unwrap();
    assert_eq!(parsed.index_root, PathBuf::from("/repo/checkout/index"));
    assert_eq!(parsed.audit_log, home.join(AUDIT_LOG_SUBPATH));
}

/// Why: F4 of the #4567 review — a write cut short leaves a line with no
/// `\n`; the next record must start on its own line and parse.
/// Red when `open` does not end the torn line.
/// Test: itself.
#[test]
fn audit_sink_ends_a_torn_line_before_the_next_record() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("audit");
    std::fs::create_dir(&dir).unwrap();
    set_mode(&dir, 0o700);
    let log = dir.join("audit.jsonl");
    std::fs::write(&log, "{\"ts\":1,\"stream\":\"credential_acc").unwrap();
    set_mode(&log, 0o600);
    let sink = AuditSink::new(log.clone(), DEFAULT_AUDIT_MAX_BYTES);
    let record = AuditRecord::new(2, AuditMethod::Set, None);
    sink.open().unwrap().append(&record).unwrap();
    let text = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text:?}");
    let parsed: AuditRecord = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(parsed, record);
    // A log already ending in `\n` gets no extra blank line.
    sink.open().unwrap().append(&record).unwrap();
    assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 3);
}
