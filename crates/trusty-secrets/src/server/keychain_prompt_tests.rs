//! A Keychain control-item read that waits on an authorization prompt
//! (#11786): doctor must say so, and the reply must name the item.
//!
//! A child of `server_tests`, so it shares that module's fixture. The
//! Keychain is [`HungControls`] behind the real [`TimeLimited`] wrapper; no
//! test touches the OS Keychain, and no test stores a value.
//!
//! Test: itself.

use super::*;
use crate::store::time_limit::TimeLimited;

/// The time limit under test; the production limit is far too long.
const LIMIT: Duration = Duration::from_millis(300);

/// The vault every test here indexes.
const VAULT: &str = "trusty/acme/web";

/// A Keychain double whose control-item reads block until the sender
/// [`HungControls::limited`] returns is dropped, as a read of an item an older
/// binary created does while its access prompt waits. Other calls answer
/// from memory.
#[derive(Debug)]
struct HungControls {
    inner: MemoryBackend,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl HungControls {
    /// The double behind [`TimeLimited`] at [`LIMIT`], and its release.
    fn limited() -> (Arc<dyn SecretBackend>, std::sync::mpsc::Sender<()>) {
        Self::limited_at(LIMIT)
    }

    /// The double behind [`TimeLimited`] at `limit`, and its release.
    fn limited_at(limit: Duration) -> (Arc<dyn SecretBackend>, std::sync::mpsc::Sender<()>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let hung = Arc::new(Self {
            inner: MemoryBackend::new(),
            release: std::sync::Mutex::new(rx),
        });
        (Arc::new(TimeLimited::new(hung, limit)), tx)
    }
}

impl SecretBackend for HungControls {
    fn id(&self) -> BackendId {
        BackendId::keychain()
    }
    fn capabilities(&self) -> crate::store::Capabilities {
        self.inner.capabilities()
    }
    fn get(&self, vault: &VaultName, key: &SecretKey) -> Result<Option<SecretValue>, SecretsError> {
        self.inner.get(vault, key)
    }
    fn set(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        value: &SecretValue,
    ) -> Result<(), SecretsError> {
        self.inner.set(vault, key, value)
    }
    fn delete(&self, vault: &VaultName, key: &SecretKey) -> Result<bool, SecretsError> {
        self.inner.delete(vault, key)
    }
    fn holds_agent_controls(&self) -> bool {
        true
    }
    fn control_item(
        &self,
        item: ControlItem,
        vault: &VaultName,
        key: Option<&SecretKey>,
    ) -> Result<bool, SecretsError> {
        if let Ok(rx) = self.release.lock() {
            let _ = rx.recv();
        }
        self.inner.control_item(item, vault, key)
    }
}

/// A server whose `keychain` is `keychain`.
async fn start_with_keychain(fx: &Fixture, keychain: Arc<dyn SecretBackend>) -> Running {
    let base = fx.backends();
    let factory: BackendFactory = Arc::new(move |id: &BackendId| match id.as_str() {
        BackendId::KEYCHAIN => Ok(Arc::clone(&keychain)),
        _ => base(id),
    });
    fx.start_with(factory).await
}

/// Index one key name in [`VAULT`], so the vault has an index file.
fn index_vault(fx: &Fixture) {
    NamesIndex::at(&fx.settings.index_root)
        .upsert(&vault(VAULT), &SecretKey::new("A").unwrap(), 8, 0)
        .unwrap();
}

/// The `keychain` row of a doctor call on `fx`'s server.
async fn keychain_row(fx: &Fixture) -> BackendStatus {
    let report: DoctorResponse =
        serde_json::from_value(ok(call(&fx.settings.socket, DOCTOR, Value::Null).await)).unwrap();
    report
        .backends
        .into_iter()
        .find(|b| b.id.as_str() == BackendId::KEYCHAIN)
        .expect("a keychain row")
}

/// Why: #11786 — the probe reads only an item this server creates, so no
/// access prompt fires and doctor said `available [READ, WRITE]` while every
/// `secrets.list` waited on the prompt an existing control item raised.
/// What: with the deny-migration marker read hanging behind the time limit,
/// the keychain row is unavailable with reason `authorization_pending`,
/// never `available` and never `open_failed`, and its detail names the
/// item's service, its vault and the console prompt.
/// Red before the fix: the row read `available: true`.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_keychain_row_names_a_pending_authorization_when_the_control_read_hangs() {
    let fx = fixture();
    index_vault(&fx);
    let (keychain, release) = HungControls::limited();
    let server = start_with_keychain(&fx, keychain).await;
    let row = keychain_row(&fx).await;
    drop(release);
    server.stop().await;

    assert!(!row.available, "{row:?}");
    assert_eq!(
        row.reason,
        Some(Unavailable::AuthorizationPending),
        "{row:?}"
    );
    assert_eq!(
        serde_json::to_value(Unavailable::AuthorizationPending).unwrap(),
        json!("authorization_pending")
    );
    let detail = row.detail.unwrap_or_default();
    for part in [
        "a Keychain authorization is pending",
        "trusty-secrets.agents-deny-migrated",
        VAULT,
        "console",
    ] {
        assert!(detail.contains(part), "{part:?} missing from {detail}");
    }
}

/// Why: #11786 — doctor's control read runs under its own
/// [`KEYCHAIN_PROBE_TIMEOUT`], not the backend's 20 s call limit or the
/// 15 s request deadline; without that bound one doctor call waits the
/// longer of those on a prompt nobody answers.
/// What: the hung read sits behind a 60 s wrapper limit, so only doctor's
/// bound can end it. The call answers no sooner than the bound and within
/// it plus 3 s, as `authorization_pending` naming the item and its vault.
/// The client waits 30 s, so a missing bound fails on the elapsed time.
/// Red without the bound: the row came back after ~15 s.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_control_read_returns_within_the_probe_bound_when_the_peer_never_answers() {
    use crate::store::KEYCHAIN_PROBE_TIMEOUT;
    let fx = fixture();
    index_vault(&fx);
    let (keychain, release) = HungControls::limited_at(Duration::from_secs(60));
    let server = start_with_keychain(&fx, keychain).await;
    let request = json!({"jsonrpc": "2.0", "id": 7, "method": DOCTOR, "params": Value::Null});
    let started = std::time::Instant::now();
    let reply = send_framed_request(&fx.settings.socket, &request, Duration::from_secs(30))
        .await
        .unwrap();
    let elapsed = started.elapsed();
    drop(release);
    server.stop().await;

    let bound = KEYCHAIN_PROBE_TIMEOUT;
    assert_eq!(bound, Duration::from_secs(5));
    assert!(
        elapsed >= bound && elapsed < bound + Duration::from_secs(3),
        "doctor answered after {elapsed:?}, want within {bound:?} + 3 s"
    );
    let report: DoctorResponse = serde_json::from_value(ok(reply)).unwrap();
    let row = report
        .backends
        .into_iter()
        .find(|b| b.id.as_str() == BackendId::KEYCHAIN)
        .expect("a keychain row");
    assert!(!row.available, "{row:?}");
    assert_eq!(
        row.reason,
        Some(Unavailable::AuthorizationPending),
        "{row:?}"
    );
    let detail = row.detail.unwrap_or_default();
    for part in ["trusty-secrets.agents-deny-migrated", VAULT] {
        assert!(detail.contains(part), "{part:?} missing from {detail}");
    }
}

/// Why: #11786 — doctor reads an existing control item; with none to read it
/// says so and does not fail on that alone, and a read that answers leaves
/// the row as it was.
/// What: no indexed vault, and an indexed vault with no marker, are both an
/// available row whose detail says no control item was read; with the
/// marker present the row is available with no detail.
/// Red before the fix: the row carried no detail.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_keychain_row_says_when_no_control_item_exists() {
    let fx = fixture();
    let server = fx.start().await;
    let none = keychain_row(&fx).await;
    index_vault(&fx);
    let unmarked = keychain_row(&fx).await;
    fx.keychain
        .set_control_item(ControlItem::DenyMigrated, &vault(VAULT), None, true)
        .unwrap();
    let marked = keychain_row(&fx).await;
    server.stop().await;

    for row in [&none, &unmarked] {
        assert!(row.available, "{row:?}");
        assert_eq!(row.reason, None, "{row:?}");
        let detail = row.detail.as_deref().unwrap_or_default();
        assert!(
            detail.contains("no existing Keychain control item was read"),
            "{row:?}"
        );
    }
    assert!(marked.available, "{marked:?}");
    assert_eq!((marked.reason, marked.detail.as_deref()), (None, None));
    assert_eq!(marked.capabilities, ["READ", "WRITE"]);
}

/// Why: #11786 — a `secrets.list` that waited on a Keychain prompt answered
/// only "the Keychain did not answer in time", naming no item and no prompt.
/// What: the hung deny-migration marker read answers the stable kind
/// `keychain_access_prompt` and code -32089, and its message names the
/// item's service, says an authorization prompt on the console is the
/// likely cause and to answer it there.
/// Red before the fix: the message named no item.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_timeout_reply_names_the_hung_keychain_item_and_the_prompt() {
    let fx = fixture();
    index_vault(&fx);
    let (keychain, release) = HungControls::limited();
    let server = start_with_keychain(&fx, keychain).await;
    let params = json!({"project": fx.project(), "vault": VAULT});
    let reply = call(&fx.settings.socket, method::LIST, params).await;
    drop(release);
    server.stop().await;

    let error = reply.error.as_ref().expect("an error reply");
    let data = error.data.as_ref().expect("error data");
    assert_eq!(data["kind"], json!("keychain_access_prompt"), "{error:?}");
    assert_eq!(error.code, -32089, "{error:?}");
    for part in [
        "the Keychain item `trusty-secrets.agents-deny-migrated`",
        "Keychain authorization prompt",
        "answer it there",
        "console",
    ] {
        assert!(error.message.contains(part), "{part:?} missing: {error:?}");
    }
}
