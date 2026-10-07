//! The audit gate's post-success arms, against a write that fails on the held
//! log only (#4567, review finding F1).
//!
//! Why: DOC-45 C-7.7a — when the record append fails after a backend call
//! succeeded, the change stands but the reply is `audit_unavailable`, and
//! `copy` stops before its next key. Those arms run only after the log
//! opened, so an unwritable directory cannot reach them.
//! What: this test binary sets `RLIMIT_FSIZE` to [`LIMIT`] and ignores
//! `SIGXFSZ` for its whole process; no other suite shares the limit. The
//! limit is 1 GiB, because it also caps this binary's own stdout and stderr
//! when a gate redirects them to a log file. Each audit log is pre-filled,
//! sparsely, to exactly [`LIMIT`] bytes ending in `\n`, so the
//! open, its checks and its torn-line test all succeed, and the first append
//! fails with `EFBIG` — after the backend call. The backends are in memory
//! and the index files are a few hundred bytes, so nothing else reaches the
//! limit. Rotation is off (`audit_max_bytes` is `u64::MAX`), so open never
//! renames the full log. `setup` proves the mechanism on this host before any
//! test relies on it. macOS checks the limit against the descriptor's offset, not the
//! append position; the sink's torn-line check leaves the descriptor at
//! end-of-file, so the limit applies to its first append too.
//! Test: itself.

#![cfg(unix)]

use std::collections::HashMap;
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use trusty_common::uds::server::RpcResponse;
use trusty_common::uds::{send_framed_request, socket_is_serving};
use trusty_secrets::server::{BackendFactory, ServeError, ServeExit, ServerSettings, serve};
use trusty_secrets::store::{Capabilities, SecretBackend};
use trusty_secrets::{BackendId, SecretKey, SecretValue, SecretsError, VaultName};

/// The file-size limit for this process, and the pre-filled log's size.
// #4567: far above any gate log this binary's output is redirected into.
const LIMIT: u64 = 1024 * 1024 * 1024;

const VAULT: &str = "trusty/acme/web";

/// Set the limit and ignore `SIGXFSZ` once, then prove a write past the
/// limit fails with `EFBIG` instead of killing the process.
fn setup() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: `signal`, `getrlimit` and `setrlimit` take plain values and
        // pointers to live locals; all affect only this test process.
        unsafe {
            libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
            let mut current = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            assert_eq!(libc::getrlimit(libc::RLIMIT_FSIZE, &mut current), 0);
            assert!(
                current.rlim_max >= LIMIT as libc::rlim_t,
                "hard RLIMIT_FSIZE is below {LIMIT}"
            );
            // Keep the hard limit: raising it is EPERM where it is finite.
            let limit = libc::rlimit {
                rlim_cur: LIMIT as libc::rlim_t,
                rlim_max: current.rlim_max,
            };
            assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &limit), 0);
        }
        let probe = TempDir::new().unwrap();
        let path = probe.path().join("probe");
        // Sparse: no gigabyte is written.
        std::fs::File::create(&path)
            .unwrap()
            .set_len(LIMIT)
            .unwrap();
        // The same shape as the audit sink: append-only, positioned at EOF.
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .append(true)
            .open(&path)
            .unwrap();
        file.seek(SeekFrom::End(0)).unwrap();
        let past = file.write(b"y").unwrap_err();
        assert_eq!(past.raw_os_error(), Some(libc::EFBIG), "{past:?}");
    });
}

/// An in-memory backend, so no backend write touches the disk.
#[derive(Debug)]
struct Mem {
    id: &'static str,
    values: Mutex<HashMap<String, String>>,
}

impl Mem {
    fn new(id: &'static str) -> Arc<Self> {
        Arc::new(Self {
            id,
            values: Mutex::new(HashMap::new()),
        })
    }

    fn put(&self, key: &str, value: &str) {
        self.values
            .lock()
            .unwrap()
            .insert(key.to_string(), value.to_string());
    }

    fn value(&self, key: &str) -> Option<String> {
        self.values.lock().unwrap().get(key).cloned()
    }
}

impl SecretBackend for Mem {
    fn id(&self) -> BackendId {
        BackendId::new(self.id).unwrap()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::READ | Capabilities::WRITE
    }

    fn get(&self, _: &VaultName, key: &SecretKey) -> Result<Option<SecretValue>, SecretsError> {
        Ok(self.value(key.as_str()).map(|v| SecretValue::new(&v)))
    }

    fn set(&self, _: &VaultName, key: &SecretKey, value: &SecretValue) -> Result<(), SecretsError> {
        self.put(key.as_str(), value.expose());
        Ok(())
    }

    fn delete(&self, _: &VaultName, key: &SecretKey) -> Result<bool, SecretsError> {
        Ok(self.values.lock().unwrap().remove(key.as_str()).is_some())
    }
}

/// A checkout, a machine config, a full audit log, and two backends:
/// `keychain` (the project's) and `src` (a copy source).
struct Fixture {
    _tmp: TempDir,
    repo: PathBuf,
    settings: ServerSettings,
    keychain: Arc<Mem>,
    src: Arc<Mem>,
}

fn fixture() -> Fixture {
    setup();
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["remote", "add", "origin", "git@github.com:Acme/Web.git"],
    ] {
        let status = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(&args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }
    let machine = tmp.path().join("machine.yaml");
    std::fs::write(&machine, "secrets:\n  default_backend: keychain\n").unwrap();
    let log = tmp.path().join("audit").join("audit.jsonl");
    prefill(&log);
    let settings = ServerSettings::new(
        tmp.path().join("run").join("s.sock"),
        tmp.path().join("index"),
        machine,
        Duration::from_secs(60),
    )
    .with_audit_log(log)
    .with_audit_max_bytes(u64::MAX);
    Fixture {
        _tmp: tmp,
        repo,
        settings,
        keychain: Mem::new("keychain"),
        src: Mem::new("src"),
    }
}

/// Make a sparse 0600 log of exactly `LIMIT` bytes ending in `\n`, in a
/// 0700 directory.
fn prefill(log: &Path) {
    let dir = log.parent().unwrap();
    std::fs::create_dir(dir).unwrap();
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(log)
        .unwrap();
    file.set_len(LIMIT - 1).unwrap();
    file.seek(SeekFrom::End(0)).unwrap();
    file.write_all(b"\n").unwrap();
}

impl Fixture {
    fn log_len(&self) -> u64 {
        std::fs::metadata(&self.settings.audit_log).unwrap().len()
    }

    fn factory(&self) -> BackendFactory {
        let (keychain, src) = (Arc::clone(&self.keychain), Arc::clone(&self.src));
        Arc::new(move |id: &BackendId| {
            let backend: Arc<dyn SecretBackend> = match id.as_str() {
                "keychain" => keychain.clone(),
                "src" => src.clone(),
                other => {
                    return Err(SecretsError::UnknownBackend {
                        backend: other.to_string(),
                    });
                }
            };
            Ok(backend)
        })
    }

    async fn start(
        &self,
    ) -> (
        JoinHandle<Result<ServeExit, ServeError>>,
        oneshot::Sender<()>,
    ) {
        let (tx, rx) = oneshot::channel::<()>();
        let task = tokio::spawn(serve(self.settings.clone(), self.factory(), async move {
            let _ = rx.await;
        }));
        for _ in 0..400 {
            if socket_is_serving(&self.settings.socket, Duration::from_millis(200)).await {
                return (task, tx);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("socket never served");
    }

    async fn call(&self, method: &str, mut params: Value) -> RpcResponse {
        params["project"] = json!(self.repo.display().to_string());
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        send_framed_request(&self.settings.socket, &request, Duration::from_secs(10))
            .await
            .unwrap()
    }
}

/// The wire kind of an error response; panics on a success.
fn kind(response: &RpcResponse) -> String {
    let error = response.error.as_ref().expect("an error response");
    error.data.as_ref().unwrap()["kind"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Why: C-7.7a — an allowed set whose record cannot be appended after the
/// backend call answers `audit_unavailable`; the value stands, and the log
/// gains no torn record. Red when `Gate::finish` passes the success through.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn post_success_append_failure_is_audit_unavailable_and_the_change_stands() {
    let fx = fixture();
    let (task, stop) = fx.start().await;
    let set = fx
        .call(
            "secrets.set",
            json!({"vault": VAULT, "key": "K", "value": "v-1234567890"}),
        )
        .await;
    assert_eq!(kind(&set), "audit_unavailable");
    assert_eq!(fx.keychain.value("K").as_deref(), Some("v-1234567890"));
    assert_eq!(fx.log_len(), LIMIT, "no torn record appended");
    let _ = stop.send(());
    task.await.unwrap().unwrap();
}

/// Why: C-7.7a — copy answers `audit_unavailable` when a copied key's record
/// cannot be appended, even on the last key, and never moves the next key.
/// Red when `Gate::record_key` lets an unrecorded copied key pass.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn copy_stops_after_a_key_whose_record_cannot_be_appended() {
    let fx = fixture();
    for key in ["A", "B", "C"] {
        fx.src.put(key, "copied-value");
    }
    let (task, stop) = fx.start().await;
    let copy = |keys: Value| {
        fx.call(
            "secrets.copy",
            json!({"from_backend": "src", "to_backend": "keychain", "keys": keys}),
        )
    };
    let last = copy(json!(["A"])).await;
    assert_eq!(kind(&last), "audit_unavailable");
    assert!(fx.keychain.value("A").is_some(), "key 1 was copied");
    let two = copy(json!(["B", "C"])).await;
    assert_eq!(kind(&two), "audit_unavailable");
    assert!(fx.keychain.value("B").is_some(), "key 1 was copied");
    assert!(fx.keychain.value("C").is_none(), "stopped before key 2");
    assert_eq!(fx.log_len(), LIMIT, "no torn record appended");
    let _ = stop.send(());
    task.await.unwrap().unwrap();
}

/// Why: C-7.7a — once any record failed to append, copy refuses before the
/// next key's backend call. Here key 1 is a miss (a deny record, which is
/// best-effort), so only `Gate::ready` can stop key 2.
/// Red when `Gate::ready` lets the next key start.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn copy_stops_before_the_next_key_after_a_failed_deny_append() {
    let fx = fixture();
    fx.src.put("A", "copied-value");
    let (task, stop) = fx.start().await;
    let copy = fx
        .call(
            "secrets.copy",
            json!({"from_backend": "src", "to_backend": "keychain", "keys": ["MISSING", "A"]}),
        )
        .await;
    assert_eq!(kind(&copy), "audit_unavailable");
    assert!(fx.keychain.value("A").is_none(), "key 2 never started");
    assert_eq!(fx.log_len(), LIMIT);
    let _ = stop.send(());
    task.await.unwrap().unwrap();
}
