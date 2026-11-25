//! The warm process: an on-demand background server on a unix socket.
//!
//! Why: plan section 4. The start-up costs of a `summarize` call (embedding
//! model load, converter worker start, HTTP connection setup) are paid once
//! per server instead of once per call. No launchd entry, no install step.
//!
//! What: the first warm call starts `trusty-summarizer server run` in the
//! background; later calls connect to it. The server exits by itself after
//! `warm.idle_timeout_secs` with no request. Files live in one per-user
//! directory ([`RuntimeDir`]): `server.sock`, `start.lock`, `server.log`.
//!
//! Safety rules, and where each is enforced:
//!
//! - Same user only: the directory is created mode 0700 and must be owned by
//!   the caller with no group or other bits; the socket is mode 0600
//!   ([`RuntimeDir::prepare`]).
//! - One server: a client starts a server only while holding an exclusive
//!   lock on `start.lock`, after a second connect attempt fails
//!   ([`client`]).
//! - Stale socket: a socket file nobody listens on is removed under that
//!   lock before a new server binds.
//! - Same configuration: each request carries the client's full effective
//!   configuration, and the server computes under exactly that
//!   configuration. The binary and the environment the library reads
//!   ([`identity`]) must match too; a server whose identity differs is told
//!   to stop and the client starts a fresh one. A client never gets
//!   results computed under another configuration.
//! - Keys: sent with the request and held only for that request
//!   ([`crate::backend::Credentials`]); never on disk or in the log, and
//!   scrubbed from the server's own environment at spawn.
//! - Loss: a record stream that ends before the server's `done` message is
//!   an error for the client, never a partial success.

pub mod client;
pub mod daemon;

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::pipeline::BatchStats;
use crate::record::FileRecord;

/// Environment variable naming the runtime directory (tests use it to stay
/// away from a developer's real server).
pub const RUNTIME_DIR_ENV: &str = "TRUSTY_SUMMARIZER_RUNTIME_DIR";

/// The longest unix socket path macOS accepts, minus the terminator.
const MAX_SOCKET_PATH: usize = 103;

/// Environment variables the library reads, so a server started under other
/// values would compute differently. Part of [`identity`].
const IDENTITY_ENV: &[&str] = &[
    "HOME",
    "PATH",
    "XDG_CACHE_HOME",
    "XDG_CONFIG_HOME",
    crate::config::CACHE_DIR_ENV,
    crate::worker::WORKER_ENV,
    "FASTEMBED_CACHE_DIR",
    "HF_HOME",
    "HF_ENDPOINT",
    "ORT_DYLIB_PATH",
];

/// The per-user directory holding the socket, the start lock and the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeDir {
    /// The directory.
    pub dir: PathBuf,
}

impl RuntimeDir {
    /// `$TRUSTY_SUMMARIZER_RUNTIME_DIR`, else
    /// `$XDG_RUNTIME_DIR/trusty-summarizer`, else
    /// `$TMPDIR/trusty-summarizer-<uid>` (per user on macOS), else
    /// `/tmp/trusty-summarizer-<uid>`.
    pub fn locate() -> Self {
        let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
        let dir = if let Some(d) = env(RUNTIME_DIR_ENV) {
            PathBuf::from(d)
        } else if let Some(d) = env("XDG_RUNTIME_DIR") {
            PathBuf::from(d).join("trusty-summarizer")
        } else {
            let base = env("TMPDIR").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
            base.join(format!("trusty-summarizer-{}", uid()))
        };
        RuntimeDir { dir }
    }

    /// The socket path.
    pub fn socket(&self) -> PathBuf {
        self.dir.join("server.sock")
    }

    /// The lock a client holds while it starts a server.
    pub fn lock(&self) -> PathBuf {
        self.dir.join("start.lock")
    }

    /// The server's stderr.
    pub fn log(&self) -> PathBuf {
        self.dir.join("server.log")
    }

    /// Creates the directory (mode 0700) if absent and checks it is safe:
    /// a real directory, owned by this user, no group or other access.
    ///
    /// Errors: an I/O error naming the directory and the rule it breaks;
    /// the caller reports it and does not fall back silently.
    /// Test: `runtime_dir_must_be_private`
    pub fn prepare(&self) -> io::Result<()> {
        if let Some(parent) = self.dir.parent() {
            fs::create_dir_all(parent)?;
        }
        match fs::DirBuilder::new().mode(0o700).create(&self.dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        let meta = fs::symlink_metadata(&self.dir)?;
        let unsafe_dir = |why: &str| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("runtime directory {} {why}", self.dir.display()),
            )
        };
        if !meta.is_dir() {
            return Err(unsafe_dir("is not a directory"));
        }
        if meta.uid() != uid() {
            return Err(unsafe_dir("is owned by another user"));
        }
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(unsafe_dir(
                "is readable by other users; remove it or chmod 700 it",
            ));
        }
        let len = self.socket().as_os_str().len();
        if len > MAX_SOCKET_PATH {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "socket path {} is {len} bytes, over the {MAX_SOCKET_PATH}-byte limit; \
                     set {RUNTIME_DIR_ENV} to a shorter directory",
                    self.socket().display()
                ),
            ));
        }
        Ok(())
    }
}

/// The current user id.
pub fn uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// A hash of what, besides the request's configuration, decides a result:
/// the crate version, the executable (path, size, modification time) and
/// the environment variables the library reads.
///
/// Why: a rebuilt binary, or a server started under another `HOME` or model
/// cache, must not serve a client whose own run would differ.
/// Test: `server_with_other_identity_is_replaced`
pub fn identity() -> String {
    let exe = std::env::current_exe().and_then(|p| p.canonicalize());
    let (path, len, mtime) = match &exe {
        Ok(p) => match fs::metadata(p) {
            Ok(m) => (
                p.display().to_string(),
                m.len(),
                m.mtime_nsec() + m.mtime() * 1_000_000_000,
            ),
            Err(_) => (p.display().to_string(), 0, 0),
        },
        Err(_) => (String::new(), 0, 0),
    };
    let env: BTreeMap<&str, Option<String>> = IDENTITY_ENV
        .iter()
        .map(|k| {
            (
                *k,
                std::env::var_os(k).map(|v| v.to_string_lossy().into_owned()),
            )
        })
        .collect();
    let value = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "exe": path,
        "exe_len": len,
        "exe_mtime_ns": mtime,
        "env": env,
    });
    let digest = Sha256::digest(value.to_string().as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether an environment variable name looks like it holds a secret; such
/// variables are removed from a server's environment when it is spawned.
pub fn looks_secret(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    ["KEY", "TOKEN", "SECRET", "PASSWORD", "CREDENTIAL"]
        .iter()
        .any(|w| n.contains(w))
}

/// The environment variable names the configuration's backends read keys
/// from (`api_key_env`).
pub fn key_variables(config: &Config) -> Vec<String> {
    let mut names: Vec<String> = config
        .backends
        .values()
        .filter_map(|t| t.get("api_key_env").and_then(toml::Value::as_str))
        .map(str::to_string)
        .collect();
    names.sort();
    names.dedup();
    names
}

/// One request line from a client. `Debug` and `Display` print the
/// operation only: a request may carry keys.
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Run a batch.
    Summarize {
        /// The client's [`identity`].
        identity: String,
        /// The client's effective configuration, paths resolved.
        config: Box<Config>,
        /// The client's working directory.
        base: PathBuf,
        /// The roots as the client was given them.
        paths: Vec<PathBuf>,
        /// API keys for this batch, by variable name.
        #[serde(default)]
        credentials: BTreeMap<String, String>,
    },
    /// Report the server's state.
    Status,
    /// Stop accepting, finish in-flight batches, exit.
    Stop,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Request({self})")
    }
}

impl std::fmt::Display for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the request itself: it may carry keys.
        f.write_str(match self {
            Request::Summarize { .. } => "summarize",
            Request::Status => "status",
            Request::Stop => "stop",
        })
    }
}

/// One line from the server.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    /// The request was accepted; records follow.
    Accepted,
    /// The server's identity differs from the client's; nothing was run.
    Mismatch {
        /// The server's identity.
        server_identity: String,
    },
    /// One record, in input order.
    Record {
        /// The record.
        record: Box<FileRecord>,
    },
    /// The batch finished; the counts cover every record sent.
    Done {
        /// Records sent.
        files: usize,
        /// Of those, error records.
        errors: usize,
        /// Of those, cache hits.
        cache_hits: usize,
    },
    /// The request could not run at all (for example, a bad configuration).
    Fatal {
        /// Why.
        message: String,
    },
    /// The answer to [`Request::Status`].
    Status(ServerStatus),
    /// The answer to [`Request::Stop`]: the socket is gone.
    Stopping {
        /// The server's process id.
        pid: u32,
    },
}

impl Reply {
    /// The `done` message for `stats`.
    pub fn done(stats: BatchStats) -> Self {
        Reply::Done {
            files: stats.files,
            errors: stats.errors,
            cache_hits: stats.cache_hits,
        }
    }
}

/// What `server status` prints for a running server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerStatus {
    /// Process id.
    pub pid: u32,
    /// Crate version.
    pub version: String,
    /// The server's [`identity`].
    pub identity: String,
    /// The socket path.
    pub socket: PathBuf,
    /// Seconds since start.
    pub uptime_secs: f64,
    /// Seconds since the last batch ended (or since start).
    pub idle_secs: f64,
    /// The idle time after which it exits.
    pub idle_timeout_secs: u64,
    /// Batches running now.
    pub active_batches: usize,
    /// Distinct configurations it holds warm.
    pub configs_loaded: usize,
}

/// Removes a stale server socket at `path` before a bind.
///
/// Why: the path is replaced on every start; removing whatever sits there
/// would delete a file, or a symlink's place, that is not the server's.
/// What: removes `path` only when it is a socket, judged without following
/// a symlink; a missing path is `Ok`. A regular file, directory, symlink or
/// other entry is an `AlreadyExists` error naming the path, and nothing is
/// removed.
/// Test: `stale_socket_removal_touches_only_sockets`
pub(crate) fn remove_stale_socket(path: &std::path::Path) -> io::Result<()> {
    use std::os::unix::fs::FileTypeExt;
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if !meta.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "{} exists and is not a socket; remove it by hand",
                path.display()
            ),
        ));
    }
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_dir_must_be_private() {
        let tmp = tempfile::tempdir().unwrap();
        let rd = RuntimeDir {
            dir: tmp.path().join("run"),
        };
        rd.prepare().unwrap();
        let mode = fs::metadata(&rd.dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        fs::set_permissions(&rd.dir, fs::Permissions::from_mode(0o755)).unwrap();
        let err = rd.prepare().unwrap_err();
        assert!(err.to_string().contains("readable by other users"), "{err}");
        let long = RuntimeDir {
            dir: tmp.path().join("x".repeat(120)),
        };
        assert!(long.prepare().is_err());
    }

    #[test]
    fn secrets_are_recognised_and_requests_never_print_keys() {
        assert!(looks_secret("OPENROUTER_API_KEY") && looks_secret("gh_token"));
        assert!(!looks_secret("HOME"));
        let r = Request::Summarize {
            identity: "i".into(),
            config: Box::default(),
            base: "/".into(),
            paths: vec![],
            credentials: [("K".to_string(), "sk-visible-no".to_string())].into(),
        };
        assert_eq!(r.to_string(), "summarize");
        assert!(!format!("{r:?}").contains("sk-visible-no"));
    }

    #[test]
    fn stale_socket_removal_touches_only_sockets() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("server.sock");
        remove_stale_socket(&path).unwrap();

        fs::write(&path, b"keep").unwrap();
        let err = remove_stale_socket(&path).unwrap_err();
        assert!(err.to_string().contains(&*path.to_string_lossy()), "{err}");
        assert_eq!(fs::read(&path).unwrap(), b"keep");
        fs::remove_file(&path).unwrap();

        let target = tmp.path().join("target.sock");
        drop(std::os::unix::net::UnixListener::bind(&target).unwrap());
        std::os::unix::fs::symlink(&target, &path).unwrap();
        let err = remove_stale_socket(&path).unwrap_err();
        assert!(err.to_string().contains("not a socket"), "{err}");
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        fs::remove_file(&path).unwrap();

        fs::create_dir(&path).unwrap();
        assert!(remove_stale_socket(&path).is_err());
        assert!(path.is_dir());
        fs::remove_dir(&path).unwrap();

        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        remove_stale_socket(&path).unwrap();
        assert!(fs::symlink_metadata(&path).is_err());
    }
}
