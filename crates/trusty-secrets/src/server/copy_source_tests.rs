//! `secrets.copy` from a named source (#10998).
//!
//! A child of `wire_tests`, so it shares that module's fixture and helpers:
//! every path is under a `TempDir`, the backends are in memory, and a
//! Keychain item is read through a fake `keychain_item_read`, never the OS.
//!
//! Test: itself.

use super::*;
use crate::api::methods::{CopySource, KeychainLabel};
use crate::server::copy_source::CopySourceAudit;
use crate::server::methods::{self, Caller};

/// The owner vault of the fixture's `Acme/Web` remote.
const OWNER_VAULT: &str = "trusty/acme";
/// The one Keychain item the fake reader holds.
const ITEM_SERVICE: &str = "gh:github.com";
const ITEM_ACCOUNT: &str = "octocat";

/// The fake Keychain: [`ITEM_SERVICE`]/[`ITEM_ACCOUNT`] holds the sentinel,
/// account `broken` fails with the sentinel in its diagnostic (a real one
/// never carries it; the server must drop it anyway), anything else misses.
fn fake_item(
    service: &KeychainLabel,
    account: &KeychainLabel,
) -> Result<Option<SecretValue>, SecretsError> {
    if account.as_str() == "broken" {
        return Err(SecretsError::Backend {
            backend: BackendId::KEYCHAIN.to_string(),
            vault: service.to_string(),
            key: account.to_string(),
            reason: SENTINEL_VALUE.to_string(),
        });
    }
    let held = service.as_str() == ITEM_SERVICE && account.as_str() == ITEM_ACCOUNT;
    Ok(held.then(|| SecretValue::new(SENTINEL_VALUE)))
}

/// The fixture's state over `procs`, reading Keychain items from [`fake_item`].
fn named_state(fx: &Fixture, procs: Arc<FakeProcs>) -> State {
    let (mut state, _grants) = state_with(fx, procs);
    state.keychain_item_read = fake_item;
    state
}

/// `procs_with_me` with the shared parent 10 marked as Claude Code.
fn agent_procs() -> Arc<FakeProcs> {
    let procs = procs_with_me();
    procs.set_agent(10);
    procs
}

fn item_source(service: &str, account: &str) -> Value {
    json!({"keychain_item": {"service": service, "account": account}})
}

fn vault_source(vault: &str, key: &str) -> Value {
    json!({"vault": {"vault": vault, "key": key}})
}

fn named_params(fx: &Fixture, from: &str, to: &str, source: Value, keys: &[&str]) -> Value {
    json!({"project": fx.project(), "from_backend": from, "to_backend": to,
        "keys": keys, "source": source})
}

/// Store `name` = `value` in `vault` on the fixture's Keychain double;
/// when `open` is false the key is denied to agents.
fn seed_in(fx: &Fixture, vault_name: &str, name: &str, value: &str, open: bool) {
    let store = SecretStore::new(
        Arc::clone(&fx.keychain) as Arc<dyn SecretBackend>,
        NamesIndex::at(&fx.settings.index_root),
    );
    let (v, k) = (vault(vault_name), key(name));
    store.set(&v, &k, &SecretValue::new(value)).unwrap();
    if !open {
        store.set_agents_may_use(&v, &k, false).unwrap();
    }
}

/// The `secrets.copy` records.
fn copy_records(fx: &Fixture) -> Vec<AuditRecord> {
    records(fx)
        .into_iter()
        .filter(|r| r.method == AuditMethod::Copy)
        .collect()
}

/// The deny-change records: (key, `agents_allowed`).
fn deny_changes(fx: &Fixture) -> Vec<(Option<String>, Option<bool>)> {
    records(fx)
        .into_iter()
        .filter(|r| r.method == AuditMethod::SetAgentsMayUse)
        .map(|r| (r.key.map(|k| k.to_string()), r.agents_allowed))
        .collect()
}

fn held(backend: &MemoryBackend, vault_name: &str, name: &str) -> Option<String> {
    backend
        .get(&vault(vault_name), &key(name))
        .unwrap()
        .map(|v| v.expose().to_owned())
}

/// Why: #10998 closure conditions — a Keychain item outside any trusty vault
/// is copied into the project under a new name; the value reaches the
/// destination only, never the reply, the audit file or a log line, and the
/// one allow record names the source locator, destination, key and pid.
/// A miss is `not_found` with one deny record and nothing written.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn copy_named_keychain_item_never_leaks_its_value() {
    capture_logs();
    tracing::info!("copy-source capture check");
    let fx = fixture();
    let server = fx.start_state(named_state(&fx, procs_with_me())).await;
    let socket = &fx.settings.socket;
    let source = item_source(ITEM_SERVICE, ITEM_ACCOUNT);
    let copied = call(
        socket,
        method::COPY,
        named_params(&fx, "keychain", "keychain", source, &["GH_TOKEN"]),
    )
    .await;
    let missing = call(
        socket,
        method::COPY,
        named_params(
            &fx,
            "keychain",
            "keychain",
            item_source(ITEM_SERVICE, "nobody"),
            &["OTHER"],
        ),
    )
    .await;
    server.stop().await;

    assert!(
        !wire(&copied).contains(SENTINEL_VALUE),
        "value in the reply"
    );
    assert_eq!(ok(copied), json!({"copied": ["GH_TOKEN"], "failed": []}));
    assert_eq!(fixed_error(&missing, method::COPY), ErrorKind::NotFound);
    assert_no_value(&missing);
    assert_eq!(
        held(&fx.keychain, PROJECT_VAULT, "GH_TOKEN").as_deref(),
        Some(SENTINEL_VALUE)
    );
    assert_eq!(held(&fx.keychain, PROJECT_VAULT, "OTHER"), None);
    let index = NamesIndex::at(&fx.settings.index_root);
    assert!(
        index
            .get(&vault(PROJECT_VAULT), &key("GH_TOKEN"))
            .unwrap()
            .is_some()
    );

    let audit = std::fs::read_to_string(&fx.settings.audit_log).unwrap();
    assert!(!audit.contains(SENTINEL_VALUE), "value in the audit file");
    let logs = captured();
    assert!(
        logs.contains("copy-source capture check"),
        "capture is live"
    );
    assert!(!logs.contains(SENTINEL_VALUE), "value in a log line");

    let all = copy_records(&fx);
    let shapes: Vec<_> = all.iter().map(shape).collect();
    assert_eq!(
        shapes,
        [
            (
                AuditMethod::Copy,
                AuditDecision::Allow,
                None,
                Some("GH_TOKEN")
            ),
            (
                AuditMethod::Copy,
                AuditDecision::Deny,
                Some("not_found"),
                Some("OTHER")
            ),
        ]
    );
    let first = &all[0];
    assert_eq!(first.caller_pid, Some(me()));
    assert_eq!(first.agent_parent, Some(false));
    assert_eq!(first.vault, Some(vault(PROJECT_VAULT)));
    assert_eq!(first.backend, Some(BackendId::keychain()));
    let expected = CopySourceAudit {
        backend: BackendId::keychain(),
        locator: CopySource::keychain_item(
            KeychainLabel::new(ITEM_SERVICE).unwrap(),
            KeychainLabel::new(ITEM_ACCOUNT).unwrap(),
        ),
    };
    assert_eq!(first.copy_source, Some(expected));
}

/// Why: #10998 decision (d) relaxes DOC-74 §13 Q6 — an owner-vault key may
/// be copied into this project's vault under another name, here into the
/// `spare` backend; the source keeps its value.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn copy_named_owner_vault_key_lands_under_a_new_name() {
    let fx = fixture();
    seed_in(&fx, OWNER_VAULT, "GH_PAT", OPEN_VALUE, true);
    let server = fx.start_state(named_state(&fx, procs_with_me())).await;
    let params = named_params(
        &fx,
        "keychain",
        "spare",
        vault_source(OWNER_VAULT, "GH_PAT"),
        &["GITHUB_TOKEN"],
    );
    let answer = call(&fx.settings.socket, method::COPY, params).await;
    server.stop().await;

    assert_eq!(
        ok(answer),
        json!({"copied": ["GITHUB_TOKEN"], "failed": []})
    );
    assert_eq!(
        held(&fx.spare, PROJECT_VAULT, "GITHUB_TOKEN").as_deref(),
        Some(OPEN_VALUE)
    );
    assert_eq!(
        held(&fx.keychain, OWNER_VAULT, "GH_PAT").as_deref(),
        Some(OPEN_VALUE)
    );
    let all = copy_records(&fx);
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(all[0].backend, Some(BackendId::from_static("spare")));
}

/// Why: #10998 — every refusal is a typed, fixed error that echoes no value
/// (the `broken` item's diagnostic carries the sentinel), writes nothing,
/// and leaves exactly one deny record. A Keychain item in trusty's own
/// namespace is refused, so a trusty key cannot skip its agent deny.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn copy_named_refusals_are_typed_and_write_nothing() {
    let fx = fixture();
    seed(&fx, "DEST", OPEN_VALUE, true);
    let server = fx.start_state(named_state(&fx, procs_with_me())).await;
    let item = || item_source(ITEM_SERVICE, ITEM_ACCOUNT);
    let cases = [
        (
            "spare",
            "keychain",
            item(),
            vec!["NEW"],
            ErrorKind::InvalidParams,
        ),
        (
            "keychain",
            "keychain",
            item_source(PROJECT_VAULT, "DEST"),
            vec!["NEW"],
            ErrorKind::InvalidParams,
        ),
        (
            "keychain",
            "keychain",
            item_source("Trusty-Secrets.agents-deny", "x"),
            vec!["NEW"],
            ErrorKind::InvalidParams,
        ),
        (
            "keychain",
            "keychain",
            item_source("bad\nservice", "x"),
            vec!["NEW"],
            ErrorKind::InvalidParams,
        ),
        (
            "keychain",
            "keychain",
            item(),
            vec![],
            ErrorKind::InvalidParams,
        ),
        (
            "keychain",
            "keychain",
            item(),
            vec!["NEW", "TWO"],
            ErrorKind::InvalidParams,
        ),
        (
            "keychain",
            "vercel",
            item(),
            vec!["NEW"],
            ErrorKind::Unsupported,
        ),
        (
            "keychain",
            "keychain",
            vault_source(PROJECT_VAULT, "DEST"),
            vec!["DEST"],
            ErrorKind::SameBackend,
        ),
        (
            "keychain",
            "keychain",
            vault_source(OWNER_VAULT, "ABSENT"),
            vec!["NEW"],
            ErrorKind::NotFound,
        ),
        (
            "keychain",
            "keychain",
            item_source(ITEM_SERVICE, "broken"),
            vec!["NEW"],
            ErrorKind::BackendFailed,
        ),
    ];
    let count = cases.len();
    for (from, to, source, keys, kind) in cases {
        let response = call(
            &fx.settings.socket,
            method::COPY,
            named_params(&fx, from, to, source.clone(), &keys),
        )
        .await;
        assert_eq!(fixed_error(&response, method::COPY), kind, "{source}");
        assert_no_value(&response);
    }
    server.stop().await;

    for name in ["NEW", "TWO"] {
        assert_eq!(held(&fx.keychain, PROJECT_VAULT, name), None, "{name}");
        assert_eq!(held(&fx.spare, PROJECT_VAULT, name), None, "{name}");
    }
    assert_eq!(
        held(&fx.keychain, PROJECT_VAULT, "DEST").as_deref(),
        Some(OPEN_VALUE)
    );
    let audit = std::fs::read_to_string(&fx.settings.audit_log).unwrap();
    assert!(!audit.contains(SENTINEL_VALUE), "value in the audit file");
    let all = copy_records(&fx);
    assert_eq!(all.len(), count, "{all:?}");
    assert!(all.iter().all(|r| r.decision == AuditDecision::Deny));
}

/// Why: #10998 — the per-key agent deny (#10386, #10459) still binds an
/// agent caller: a vault source denied to agents is refused before its
/// value is read and no deny is carried; a destination key denied to agents
/// is refused before the item is read; an undenied copy works, its record
/// carries `default_policy`, and (fix round 1) the copied item lands push-only.
/// Red when either deny check is absent.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn copy_named_by_agent_of_a_denied_source_or_destination_is_refused() {
    let fx = fixture();
    seed_in(&fx, OWNER_VAULT, "OWNER_KEY", SENTINEL_VALUE, false);
    let web = vault(PROJECT_VAULT);
    fx.keychain
        .set_control_item(
            ControlItem::AgentDeny,
            &web,
            Some(&key("DENIED_DEST")),
            true,
        )
        .unwrap();
    let before = deny_changes(&fx);
    let server = fx.start_state(named_state(&fx, agent_procs())).await;
    let socket = &fx.settings.socket;
    let denied_source = call(
        socket,
        method::COPY,
        named_params(
            &fx,
            "keychain",
            "spare",
            vault_source(OWNER_VAULT, "OWNER_KEY"),
            &["COPIED"],
        ),
    )
    .await;
    let item = || item_source(ITEM_SERVICE, ITEM_ACCOUNT);
    let denied_dest = call(
        socket,
        method::COPY,
        named_params(&fx, "keychain", "keychain", item(), &["DENIED_DEST"]),
    )
    .await;
    let open = call(
        socket,
        method::COPY,
        named_params(&fx, "keychain", "keychain", item(), &["AGENT_OK"]),
    )
    .await;
    server.stop().await;

    for refused in [&denied_source, &denied_dest] {
        assert_eq!(
            fixed_error(refused, method::COPY),
            ErrorKind::AgentKeyDenied
        );
        assert_no_value(refused);
    }
    assert_eq!(ok(open), json!({"copied": ["AGENT_OK"], "failed": []}));
    assert_eq!(held(&fx.spare, PROJECT_VAULT, "COPIED"), None);
    assert_eq!(held(&fx.keychain, PROJECT_VAULT, "DENIED_DEST"), None);
    // #10998 fix round 1: no deny was carried for a refused key; the one
    // deny written is the agent's Keychain copy landing push-only.
    assert!(before.is_empty(), "{before:?}");
    assert_eq!(
        deny_changes(&fx),
        [(Some("AGENT_OK".to_string()), Some(false))]
    );
    let shapes: Vec<_> = copy_records(&fx)
        .iter()
        .map(|r| (r.decision, r.agent_parent, r.default_policy))
        .collect();
    assert_eq!(
        shapes,
        [
            (AuditDecision::Deny, Some(true), None),
            (AuditDecision::Deny, Some(true), None),
            (AuditDecision::Allow, Some(true), Some(true)),
        ]
    );
}

/// Why: #10998 fix round 1 (code-critic HIGH; Architect 2026-11-11 02:35Z)
/// — an agent that copies a Keychain item must not then read it: the key
/// lands push-only for agents (no deny), written and recorded before the
/// value; `list` shows the mark, and the agent's grant is
/// `agent_key_denied` with no token minted. Red when the copy writes the
/// key open.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn copy_named_by_agent_of_a_keychain_item_lands_push_only() {
    let fx = fixture();
    let (mut state, grants) = state_with(&fx, agent_procs());
    state.keychain_item_read = fake_item;
    let server = fx.start_state(state).await;
    let socket = &fx.settings.socket;
    let copied = call(
        socket,
        method::COPY,
        named_params(
            &fx,
            "keychain",
            "keychain",
            item_source(ITEM_SERVICE, ITEM_ACCOUNT),
            &["ITEM_COPY"],
        ),
    )
    .await;
    let grant = call(
        socket,
        method::GRANT,
        grant_params(&fx, me(), &["ITEM_COPY"]),
    )
    .await;
    let listed = ok(call(
        socket,
        method::LIST,
        json!({"project": fx.project(), "vault": PROJECT_VAULT}),
    )
    .await);
    server.stop().await;

    assert_eq!(ok(copied), json!({"copied": ["ITEM_COPY"], "failed": []}));
    assert_eq!(
        held(&fx.keychain, PROJECT_VAULT, "ITEM_COPY").as_deref(),
        Some(SENTINEL_VALUE)
    );
    let (web, name) = (vault(PROJECT_VAULT), key("ITEM_COPY"));
    let control = |item| fx.keychain.control_item(item, &web, Some(&name)).unwrap();
    assert!(control(ControlItem::PushOnly), "lands push-only");
    assert!(!control(ControlItem::AgentDeny), "not denied");
    assert_eq!(listed["keys"][0]["name"], "ITEM_COPY");
    assert_eq!(listed["keys"][0]["agents_push_only"], true);
    assert_eq!(listed["keys"][0]["agents_may_use"], false);
    assert!(!listed.to_string().contains(SENTINEL_VALUE));
    assert_eq!(
        fixed_error(&grant, method::GRANT),
        ErrorKind::AgentKeyDenied
    );
    assert_no_value(&grant);
    assert_eq!(grants.has_unexpired(), Ok(false), "nothing was minted");
    assert_eq!(
        deny_changes(&fx),
        [(Some("ITEM_COPY".to_string()), Some(false))]
    );
}

/// Seed `name` in the project vault and mark it push-only for agents.
fn seed_push_only(fx: &Fixture, name: &str) {
    seed(fx, name, SENTINEL_VALUE, true);
    fx.keychain
        .set_control_item(
            ControlItem::PushOnly,
            &vault(PROJECT_VAULT),
            Some(&key(name)),
            true,
        )
        .unwrap();
}

/// Why: #10998 (Architect 2026-11-11 02:35Z) — for an agent caller a
/// push-only key behaves as denied on every value-yielding path: `grant`
/// (so `tm secrets exec`), `resolve` of a token a person minted, a local
/// `copy` from the project vault, a named `copy` with it as the vault
/// source, and an agent `set` over it. A person's grant and resolve still
/// work. Red when the mark is not read.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn push_only_key_is_refused_to_every_agent_read_and_open_to_a_person() {
    let fx = fixture();
    seed_push_only(&fx, "PUSHED");
    let procs = procs_with_me();
    let state = named_state(&fx, Arc::clone(&procs));
    let server = fx.start_state(state).await;
    let socket = &fx.settings.socket;
    let token = granted(&fx, me(), &["PUSHED"]).await;
    let person = ok(call(socket, method::RESOLVE, resolve_params(&token, "PUSHED")).await);
    procs.set_agent(10);
    let resolve = call(socket, method::RESOLVE, resolve_params(&token, "PUSHED")).await;
    let grant = call(socket, method::GRANT, grant_params(&fx, me(), &["PUSHED"])).await;
    let local = call(
        socket,
        method::COPY,
        json!({"project": fx.project(), "from_backend": "keychain",
            "to_backend": "spare", "keys": ["PUSHED"]}),
    )
    .await;
    let named = call(
        socket,
        method::COPY,
        named_params(
            &fx,
            "keychain",
            "spare",
            vault_source(PROJECT_VAULT, "PUSHED"),
            &["OUT"],
        ),
    )
    .await;
    let set = call(
        socket,
        method::SET,
        json!({"project": fx.project(), "vault": PROJECT_VAULT, "key": "PUSHED",
            "value": OPEN_VALUE}),
    )
    .await;
    server.stop().await;

    assert_eq!(
        person["value"], SENTINEL_VALUE,
        "a person's use is unaffected"
    );
    for (response, name) in [
        (&resolve, method::RESOLVE),
        (&grant, method::GRANT),
        (&named, method::COPY),
        (&set, method::SET),
    ] {
        assert_eq!(
            fixed_error(response, name),
            ErrorKind::AgentKeyDenied,
            "{name}"
        );
        assert_no_value(response);
    }
    assert_eq!(ok(local), json!({"copied": [], "failed": ["PUSHED"]}));
    let local_reason = copy_records(&fx)
        .into_iter()
        .find(|r| r.key == Some(key("PUSHED")))
        .and_then(|r| r.reason.map(|reason| reason.as_str().to_owned()));
    assert_eq!(local_reason.as_deref(), Some("agent_key_denied"));
    assert_eq!(held(&fx.spare, PROJECT_VAULT, "PUSHED"), None);
    assert_eq!(held(&fx.spare, PROJECT_VAULT, "OUT"), None);
    assert_eq!(
        held(&fx.keychain, PROJECT_VAULT, "PUSHED").as_deref(),
        Some(SENTINEL_VALUE),
        "the agent's set wrote nothing"
    );
}

/// Why: #10998 — a person lifts the push-only mark (and any deny) with
/// `set_agents_may_use` `allowed: true`; agents may then use the key.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn person_lifts_push_only_with_set_agents_may_use() {
    let fx = fixture();
    seed_push_only(&fx, "PUSHED");
    let server = fx.start_state(named_state(&fx, procs_with_me())).await;
    let lifted = call(
        &fx.settings.socket,
        method::SET_AGENTS_MAY_USE,
        json!({"project": fx.project(), "vault": PROJECT_VAULT, "key": "PUSHED",
            "allowed": true}),
    )
    .await;
    server.stop().await;

    ok(lifted);
    let store = SecretStore::new(
        Arc::clone(&fx.keychain) as Arc<dyn SecretBackend>,
        NamesIndex::at(&fx.settings.index_root),
    );
    let (web, name) = (vault(PROJECT_VAULT), key("PUSHED"));
    assert!(matches!(store.is_push_only(&web, &name), Ok(false)));
    assert!(matches!(store.agents_may_use(&web, &name), Ok(true)));
}

/// Why: #10998 Fail-Open Check — the mark's read API (#10999 calls it from
/// `server/sync_target.rs`) reports the mark, reads `false` only where no
/// control item can exist, and returns a failed lookup as an error; an
/// agent's use of the key then fails too, never reads it open.
/// Test: itself.
#[test]
fn push_only_mark_is_read_and_fails_closed() {
    let fx = fixture();
    seed_push_only(&fx, "PUSHED");
    seed(&fx, "OPEN", OPEN_VALUE, true);
    let (web, pushed, open) = (vault(PROJECT_VAULT), key("PUSHED"), key("OPEN"));
    let backend: &dyn SecretBackend = fx.keychain.as_ref();
    assert!(matches!(
        crate::store::is_push_only(backend, &web, &pushed),
        Ok(true)
    ));
    assert!(matches!(
        crate::store::is_push_only(backend, &web, &open),
        Ok(false)
    ));
    let bare = MemoryBackend::without_agent_controls();
    assert!(matches!(
        crate::store::is_push_only(&bare, &web, &pushed),
        Ok(false)
    ));
    let store = SecretStore::new(
        Arc::clone(&fx.keychain) as Arc<dyn SecretBackend>,
        NamesIndex::at(&fx.settings.index_root),
    );
    assert!(matches!(store.agents_may_use(&web, &pushed), Ok(false)));
    fx.keychain.fail_controls(true);
    assert!(crate::store::is_push_only(backend, &web, &open).is_err());
    assert!(store.agents_may_use(&web, &open).is_err());
}

/// Why: #10386's escape, renamed — a person's copy of a vault key denied to
/// agents carries the deny to the destination key's new name before the
/// value lands, with its write-ahead record. Red when the carry uses the
/// source's name or is skipped.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn copy_named_carries_a_vault_source_deny_to_the_destination_key() {
    let fx = fixture();
    seed_in(&fx, OWNER_VAULT, "OWNER_KEY", SENTINEL_VALUE, false);
    let server = fx.start_state(named_state(&fx, procs_with_me())).await;
    let params = named_params(
        &fx,
        "keychain",
        "spare",
        vault_source(OWNER_VAULT, "OWNER_KEY"),
        &["RENAMED"],
    );
    let answer = call(&fx.settings.socket, method::COPY, params).await;
    server.stop().await;

    assert_eq!(ok(answer), json!({"copied": ["RENAMED"], "failed": []}));
    let web = vault(PROJECT_VAULT);
    assert!(
        fx.spare
            .control_item(ControlItem::AgentDeny, &web, Some(&key("RENAMED")))
            .unwrap(),
        "the deny travels to the new name"
    );
    let carried: Vec<_> = deny_changes(&fx)
        .into_iter()
        .filter(|(k, _)| k.as_deref() == Some("RENAMED"))
        .collect();
    assert_eq!(carried, [(Some("RENAMED".to_string()), Some(false))]);
}

/// Why: Fail-Open Check — with the audit log unwritable the named copy is
/// `audit_unavailable` and nothing is written, never a success without its
/// record. Red when the move runs before `gate.admit`.
/// Test: itself.
#[cfg(unix)]
#[test]
fn copy_named_with_unwritable_audit_writes_nothing() {
    let fx = fixture();
    let dir = fx.settings.audit_log.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&dir).unwrap();
    set_mode(&dir, 0o755);
    let _restore = RestoreMode(dir);
    let state = named_state(&fx, procs_with_me());
    let params = named_params(
        &fx,
        "keychain",
        "keychain",
        item_source(ITEM_SERVICE, ITEM_ACCOUNT),
        &["GH_TOKEN"],
    );
    let copied = methods::copy(&state, Caller::new(Some(me())), params);
    assert_eq!(copied, Err(ErrorKind::AuditUnavailable));
    assert_eq!(held(&fx.keychain, PROJECT_VAULT, "GH_TOKEN"), None);
}

/// Why: #10998 regression — a copy without `source` keeps its pre-#10998
/// reply and records, which carry no `copy_source`.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn copy_without_source_is_unchanged_by_10998() {
    let fx = fixture();
    seed(&fx, "API_KEY", OPEN_VALUE, true);
    let server = fx.start_state(named_state(&fx, procs_with_me())).await;
    let params = json!({"project": fx.project(), "from_backend": "keychain",
        "to_backend": "spare", "keys": ["API_KEY", "ABSENT"]});
    let answer = call(&fx.settings.socket, method::COPY, params).await;
    let same = json!({"project": fx.project(), "from_backend": "keychain",
        "to_backend": "keychain", "keys": ["API_KEY"]});
    let refused = call(&fx.settings.socket, method::COPY, same).await;
    server.stop().await;

    assert_eq!(
        ok(answer),
        json!({"copied": ["API_KEY"], "failed": ["ABSENT"]})
    );
    assert_eq!(fixed_error(&refused, method::COPY), ErrorKind::SameBackend);
    let all = copy_records(&fx);
    assert_eq!(all.len(), 3, "{all:?}");
    assert!(all.iter().all(|r| r.copy_source.is_none()));
    let audit = std::fs::read_to_string(&fx.settings.audit_log).unwrap();
    assert!(!audit.contains("copy_source"), "the field is not written");
}
