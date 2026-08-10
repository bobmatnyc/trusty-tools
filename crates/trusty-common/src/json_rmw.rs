//! Cross-process read-modify-write for a whole-file JSON document.
//!
//! Why: several trusty crates persist a small JSON document that multiple
//! independent PROCESSES mutate — `trusty-mpm`'s `projects.json` registry,
//! `trusty-gworkspace`'s `tokens.json` (issue #3502), and the dual worktree
//! registry epic #4207 will reconcile. Every one of them is a
//! load → mutate → save-the-whole-file cycle. With no cross-process
//! serialisation, two writers that interleave read/read/write/write silently
//! lose one of the two updates and BOTH callers see success; worse, if they
//! share one temp path they can publish a half-written document and corrupt the
//! file outright. An in-process `Mutex` cannot fix either failure because the
//! writers are separate processes. This module is the single implementation of
//! that critical section so each call site inherits the fix instead of
//! re-deriving it.
//! What: [`update`] takes an advisory exclusive lock on a `<path>.lock` sidecar,
//! re-reads the document from disk under that lock (never trusting a caller's
//! possibly-stale copy), applies the caller's mutation, and publishes the result
//! atomically — unique temp file, `fsync`, `rename`, `fsync` of the parent
//! directory — before releasing the lock.
//! Test: `cargo test -p trusty-common -- json_rmw::tests`.
//!
//! # Atomicity contract
//!
//! Guarantees a caller may rely on:
//!
//! 1. **Serialisation.** The read, the mutation and the write happen while one
//!    writer holds an exclusive advisory lock, so no other [`update`] on the
//!    same FILE can observe or overwrite the intermediate state. The unit is
//!    the file, not the spelling: symlinks are resolved before the lock is
//!    taken, so two callers reaching one document by different paths still
//!    serialise against each other (#5264). The lock is
//!    `flock(2)`-style: it is held by the open file description, so it
//!    serialises separate processes AND separate threads that each call
//!    [`update`], on Unix and Windows alike.
//! 2. **All-or-nothing publish.** Readers of `path` see either the complete
//!    previous document or the complete new one, never a partial write: the
//!    document is built in a temp file and moved into place with `rename`.
//!    That is the whole of this guarantee — the temp name (pid plus a
//!    nanosecond stamp) is scratch-path hygiene, NOT a second line of defence
//!    for a writer that bypasses the lock. An earlier version of this comment
//!    claimed it was; #4906's review falsified that experimentally, with 16
//!    threads landing on a single nanosecond value and colliding. Only
//!    guarantee 1 keeps concurrent writers apart.
//! 3. **Never fail open.** Every failure — lock acquisition, read, parse,
//!    serialise, write, rename — returns `Err` and leaves `path` byte-for-byte
//!    unchanged. There is no path on which a failed update advances state, and
//!    an unreadable-but-present file is never silently replaced with a default
//!    (empty) document; only a genuinely absent file starts from
//!    [`Default`].
//! 4. **Crash safety.** A writer killed at any point leaves either the previous
//!    document intact or an orphaned `*.tmp` file that no reader consults.
//!
//! Explicitly NOT guaranteed:
//!
//! - **Advisory, not mandatory.** A process that writes `path` without going
//!   through [`update`] is not blocked. Every writer of a given file must use
//!   this entry point.
//! - **Not reentrant.** [`update`] must not be called from inside another
//!   [`update`] closure on the same path: the second acquisition uses a
//!   different file descriptor and will self-deadlock.
//! - **Blocking.** Lock acquisition blocks the calling thread. Async callers
//!   must run [`update`] on a blocking-safe thread (e.g.
//!   `tokio::task::spawn_blocking`).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Failure modes of a locked JSON read-modify-write.
///
/// Why: callers must be able to tell a lock-contention/permission problem apart
/// from a corrupt document, because the remedies differ (retry / operator
/// intervention vs. restore the file). Every variant carries the path it
/// concerns so a multi-file caller can report which document failed.
/// What: one variant per stage of the cycle — lock, I/O, (de)serialisation.
/// `Display`/`Error` are implemented by hand rather than derived: this crate
/// keeps `thiserror` behind an optional feature (`default = []`), and `json_rmw`
/// is an unconditional module, so deriving would force `thiserror` into every
/// minimal build of `trusty-common`. Crates that own their own error types
/// should still prefer `thiserror` and convert via [`From`].
/// Test: `update_lock_path_unopenable_errors`, `update_corrupt_file_errors`.
#[derive(Debug)]
pub enum JsonRmwError {
    /// The advisory lock could not be created or acquired.
    Lock {
        /// The document the lock guards (not the sidecar itself).
        path: PathBuf,
        /// The underlying OS error.
        source: std::io::Error,
    },

    /// Reading, writing, renaming or syncing the document failed.
    Io {
        /// The document being read or published.
        path: PathBuf,
        /// The underlying OS error.
        source: std::io::Error,
    },

    /// The document could not be parsed, or the new value could not be encoded.
    Serialize {
        /// The document being parsed or encoded.
        path: PathBuf,
        /// The `serde_json` message.
        message: String,
    },
}

impl std::fmt::Display for JsonRmwError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lock { path, source } => write!(
                f,
                "could not acquire the update lock for {}: {source}",
                path.display()
            ),
            Self::Io { path, source } => write!(
                f,
                "json read-modify-write I/O error on {}: {source}",
                path.display()
            ),
            Self::Serialize { path, message } => write!(
                f,
                "json read-modify-write serialization error on {}: {message}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for JsonRmwError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Lock { source, .. } | Self::Io { source, .. } => Some(source),
            Self::Serialize { .. } => None,
        }
    }
}

impl JsonRmwError {
    /// Wrap an I/O error against `path`.
    fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }

    /// Wrap a (de)serialisation failure against `path`.
    ///
    /// Takes the rendered message rather than a `serde_json::Error` so a
    /// non-JSON [`DocumentCodec`] can report through the same variant (#5264).
    pub fn serialize(path: &Path, message: impl std::fmt::Display) -> Self {
        Self::Serialize {
            path: path.to_path_buf(),
            message: message.to_string(),
        }
    }
}

/// Sidecar lock-file path for `path`.
///
/// Why: locking the document itself would mean opening it for write before we
/// know whether the update will succeed, and would be lost across the `rename`
/// that publishes a new version (the renamed-over inode, and any lock on it,
/// is discarded). A stable sidecar survives every publish.
/// What: appends `.lock` to the file name, keeping it in the same directory.
/// Test: `lock_path_is_a_sidecar`.
pub fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    path.with_file_name(name)
}

/// Scratch path for one publish attempt — unique per writer and per attempt.
///
/// Why: a SHARED temp name is itself a corruption bug. `trusty-mpm` used a fixed
/// `projects.json.tmp`: two processes writing it at once interleaved into one
/// file and then renamed the mangled result over the real registry, producing a
/// `projects.json` that no longer parsed. Uniqueness per attempt removes that
/// class of failure entirely, independently of the lock.
/// What: `<file_name>.<pid>.<nanos>.tmp`, alongside the target so the publish is
/// a same-filesystem `rename`.
fn temp_path(path: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.{nanos}.tmp", std::process::id()));
    path.with_file_name(name)
}

/// Read `path`, treating only genuine absence as "no document yet".
///
/// Why: this is the "never fail open" hinge. If any I/O error were treated as
/// "file absent", a transient permission or hardware fault would hand the caller
/// an empty `T`, and the publish at the end of [`update`] would overwrite a
/// perfectly good document with nothing — a total data loss dressed up as
/// success.
/// What: `NotFound` yields `Ok(None)`; every other error propagates.
fn read_bytes(path: &Path) -> Result<Option<Vec<u8>>, JsonRmwError> {
    match std::fs::read(path) {
        Ok(raw) => Ok(Some(raw)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(JsonRmwError::io(path, e)),
    }
}

/// Publish `bytes` at `path` atomically: unique temp, fsync, rename, fsync dir.
///
/// Why: `rename(2)` within a filesystem is atomic, so a reader sees the old file
/// or the new one and never a partial write. The `fsync` of the temp file before
/// the rename is what makes that true across a power loss rather than only
/// across a process crash; the `fsync` of the directory makes the rename itself
/// durable.
/// What: writes to [`temp_path`], syncs it, renames it over `path`, then syncs
/// the parent directory. Any failure removes the temp file and returns `Err`
/// with `path` untouched.
/// Test: `update_publishes_atomically_leaving_no_temp`,
/// `update_write_failure_leaves_original_intact`.
fn publish_atomic(
    path: &Path,
    bytes: &[u8],
    new_file_mode: Option<u32>,
) -> Result<(), JsonRmwError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    ensure_parent_dir(path, new_file_mode)?;

    // #5264: `File::create` applies 0644 minus umask. Two cases, both wrong by
    // default: republishing over a `chmod 600` document silently widens it, and
    // CREATING a document that holds MCP provider credentials leaves it
    // world-readable from birth. The existing file's mode wins when there is
    // one; otherwise the codec's declared mode for a fresh file applies.
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .ok()
            .map(|m| m.permissions().mode() & 0o777)
            .or(new_file_mode)
    };

    let tmp = temp_path(path);
    let write_result = (|| -> std::io::Result<()> {
        let mut file = File::create(&tmp)?;
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        }
        file.write_all(bytes)?;
        // Durability of the CONTENT must precede the rename that publishes it.
        file.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write_result {
        // Never leave a half-written scratch file behind.
        let _ = std::fs::remove_file(&tmp);
        return Err(JsonRmwError::io(path, e));
    }

    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(JsonRmwError::io(path, e));
    }

    // Durability of the rename itself. Unix-only: Windows has no directory
    // handle to sync, and its rename is already committed to the log.
    #[cfg(unix)]
    if let Ok(dir) = File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// Create `path`'s parent directory, narrowing it when the codec asks for a
/// restricted file mode.
///
/// Why (#5264): this has to run where the directory is FIRST created, and that
/// is the lock setup in [`update_with_decision`], not [`publish_atomic`] — by
/// the time the publish runs, the lock sidecar has already brought the
/// directory into existence at 0755 and the "did we create it?" test is dead.
/// Only a directory this call creates is narrowed; an operator's existing
/// `~/.codex` is theirs to set.
/// What: `create_dir_all`, then on Unix `chmod` the directory to the file mode
/// plus owner traverse (0o600 → 0o700) when it did not already exist.
/// Test: `publish_uses_the_codec_mode_for_a_new_file`,
/// `codex_config::tests::patch_mcp_server_creates_a_private_file`.
fn ensure_parent_dir(path: &Path, new_file_mode: Option<u32>) -> Result<(), JsonRmwError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let existed = parent.exists();
    std::fs::create_dir_all(parent).map_err(|e| JsonRmwError::io(path, e))?;
    #[cfg(unix)]
    if let (false, Some(mode)) = (existed, new_file_mode) {
        use std::os::unix::fs::PermissionsExt;
        let dir_mode = mode | ((mode & 0o444) >> 2);
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(dir_mode));
    }
    Ok(())
}

/// Follow `path` through every symlink hop to the file that will be written.
///
/// Why (#5264): renaming over a symlink replaces the LINK, detaching a document
/// an operator symlinked into a dotfiles repo. Resolving only one hop is worse
/// than not resolving at all in one respect: with
/// `config.toml -> mid.toml -> real.toml` the write lands on `mid.toml`,
/// converting it to a regular file while `real.toml` — the file under version
/// control — keeps the stale content, and the outer link still looks healthy in
/// `ls -l`, so the break is invisible one level down.
/// What: follows `read_link` until it stops being a symlink, resolving relative
/// targets against the link's own directory. A cycle is bounded by
/// [`MAX_LINK_HOPS`] and yields the last hop, whose open then fails with the
/// platform's own `ELOOP` rather than this function hanging.
/// Test: `publish_follows_a_symlink_chain`, `publish_survives_a_symlink_cycle`.
fn resolve_link(path: &Path) -> PathBuf {
    let mut current = path.to_path_buf();
    for _ in 0..MAX_LINK_HOPS {
        let Ok(target) = std::fs::read_link(&current) else {
            return current;
        };
        current = if target.is_absolute() {
            target
        } else {
            current
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(target)
        };
    }
    current
}

/// Hop limit for [`resolve_link`], matching the usual kernel `SYMLOOP_MAX`.
const MAX_LINK_HOPS: usize = 32;

/// Run a read-modify-write on the JSON document at `path` under an exclusive
/// cross-process lock.
///
/// Why: see the module-level rationale — this is the one place the
/// load → mutate → save cycle is made safe against concurrent writers, so
/// callers stop hand-rolling (and getting wrong) their own version of it.
/// What: acquires the exclusive advisory lock on [`lock_path`] (blocking until
/// it is available), re-reads and parses `path` under that lock (an absent file
/// starts from [`Default`]; an unreadable or malformed one is an error, never a
/// silent reset), calls `f` with the freshly-read value, and — only when `f`
/// returns `Ok` — publishes the mutated document via [`publish_atomic`]. When
/// `f` returns `Err` the document is left byte-for-byte unchanged and the error
/// is propagated, so a rejected mutation cannot advance state. The lock is
/// released by RAII on every exit path, including panics.
///
/// `f`'s error type `E` need only be constructible from [`JsonRmwError`], which
/// lets a caller keep its own domain error as the single return type.
///
/// Blocking: acquisition blocks the calling thread; async callers must wrap this
/// in `tokio::task::spawn_blocking`. Not reentrant — see the module docs.
/// Test: `update_serialises_concurrent_threads`,
/// `update_creates_file_when_absent`, `update_closure_error_does_not_write`,
/// `update_lock_path_unopenable_errors`.
pub fn update_with<C, R, E, F>(path: &Path, f: F) -> Result<R, E>
where
    C: DocumentCodec,
    E: From<JsonRmwError>,
    F: FnOnce(&mut C::Document) -> Result<R, E>,
{
    update_with_decision::<C, R, E, _>(path, |doc| f(doc).map(|r| (r, true)))
}

/// [`update_with`], but the closure also decides whether to publish.
///
/// Why (#5264): an idempotent upsert that finds the document already correct
/// must not rewrite it — republishing byte-identical content still churns the
/// mtime and burns an `fsync` on every re-run of a setup command. `update_with`
/// always publishes, so the no-op case needs a way to say so from INSIDE the
/// lock, where the decision can be trusted. Deciding before taking the lock
/// would race a concurrent writer in the dangerous direction: the check could
/// pass, another process could then break the entry, and this call would skip
/// the write it now owed.
/// What: the closure returns `(value, publish)`. `publish == false` leaves the
/// file byte-for-byte untouched and still returns `value`. Everything else —
/// lock, re-read, atomic publish — is [`update_with`]'s.
/// Test: `update_with_decision_false_does_not_write`.
pub fn update_with_decision<C, R, E, F>(path: &Path, f: F) -> Result<R, E>
where
    C: DocumentCodec,
    E: From<JsonRmwError>,
    F: FnOnce(&mut C::Document) -> Result<(R, bool), E>,
{
    // #5264: resolve the symlink ONCE, here, and use the result for the lock,
    // the read and the publish alike. Locking the caller's path while writing
    // the resolved one means two writers reaching the same file by different
    // names take different `.lock` sidecars and lose each other's updates —
    // the exact defect this module exists to prevent, made newly reachable by
    // following links at all.
    let resolved = resolve_link(path);
    let path = resolved.as_path();

    // The guard borrows the lock, so both must stay on this frame; a helper
    // returning the guard would need a self-referential struct. Acquiring here
    // keeps it plain RAII — released on every exit path, including panics.
    // Creates the directory (with the codec's mode) BEFORE the lock sidecar
    // lands in it, so a credential-bearing config is never briefly world-listable.
    ensure_parent_dir(path, C::new_file_mode())?;
    let lock = lock_path(path);
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock)
        .map_err(|e| JsonRmwError::Lock {
            path: path.to_path_buf(),
            source: e,
        })?;
    let mut rw = fd_lock::RwLock::new(lock_file);
    // Blocking exclusive acquisition. Failure is an error, never a bypass:
    // proceeding unlocked is exactly the lost-update bug this module exists to
    // remove.
    let _guard = rw.write().map_err(|e| JsonRmwError::Lock {
        path: path.to_path_buf(),
        source: e,
    })?;

    let mut value = C::decode(path, read_bytes(path)?.as_deref())?;
    let (result, publish) = f(&mut value)?;
    if publish {
        let bytes = C::encode(path, &value)?;
        publish_atomic(path, &bytes, C::new_file_mode())?;
    }
    Ok(result)
}

/// How one document format is read back and written out inside [`update_with`].
///
/// Why (#5264): the locking, atomic publish, permission and symlink handling in
/// this module are format-independent, but [`update`] hard-wires `serde_json`.
/// Codex's `~/.codex/config.toml` needs the same critical section and must be
/// edited with `toml_edit` to preserve the operator's comments — a fourth
/// hand-rolled atomic writer to get there would be exactly the duplication this
/// module exists to end. This trait is the seam; the safety properties stay in
/// one place.
/// What: `decode` turns the on-disk bytes (or `None`, meaning absent) into the
/// working document; `encode` renders it back. Both take `path` so failures name
/// the file.
/// Test: `update_with_a_text_codec_round_trips`, `text_codec_rejects_invalid_utf8`,
/// plus every existing `update` test — [`update`] is a specialisation of
/// [`update_with`] over [`JsonCodec`].
///
/// Sealed: the seam exists to serve the documents in this workspace, and an
/// unsealed public trait with no provided methods cannot gain one — such as
/// [`DocumentCodec::new_file_mode`] below — without breaking every downstream
/// implementor. Adding a codec means adding it here.
pub trait DocumentCodec: sealed::Sealed {
    /// The in-memory document the caller mutates.
    type Document;

    /// Parse `bytes` (`None` when the file does not exist) into a document.
    fn decode(path: &Path, bytes: Option<&[u8]>) -> Result<Self::Document, JsonRmwError>;

    /// Render `doc` to the bytes that will be published.
    fn encode(path: &Path, doc: &Self::Document) -> Result<Vec<u8>, JsonRmwError>;

    /// Mode to give the document when this call CREATES it, if it should not be
    /// the platform default.
    ///
    /// Why (#5264): mode PRESERVATION only helps a file that already exists, and
    /// the call that creates `~/.codex/config.toml` is the one that puts an MCP
    /// provider credential in it. `File::create` would leave that at 0644.
    /// What: `None` (the default) means 0644 minus umask, which is right for a
    /// registry or a ledger. A codec whose document can hold a secret returns
    /// `Some(0o600)`.
    /// Test: `publish_uses_the_codec_mode_for_a_new_file`,
    /// `codex_config::tests::patch_mcp_server_creates_a_private_file`.
    fn new_file_mode() -> Option<u32> {
        None
    }
}

/// Seals [`DocumentCodec`] against out-of-workspace implementors.
pub(crate) mod sealed {
    /// Implemented only by the codecs in this module and `codex_config`.
    pub trait Sealed {}
}

/// [`DocumentCodec`] for pretty-printed JSON — the behaviour [`update`] has
/// always had, now expressed through the seam.
pub struct JsonCodec<T>(std::marker::PhantomData<T>);

impl<T> sealed::Sealed for JsonCodec<T> {}

impl<T> DocumentCodec for JsonCodec<T>
where
    T: DeserializeOwned + Serialize + Default,
{
    type Document = T;

    fn decode(path: &Path, bytes: Option<&[u8]>) -> Result<T, JsonRmwError> {
        match bytes {
            Some(raw) => serde_json::from_slice(raw).map_err(|e| JsonRmwError::serialize(path, e)),
            None => Ok(T::default()),
        }
    }

    fn encode(path: &Path, doc: &T) -> Result<Vec<u8>, JsonRmwError> {
        serde_json::to_vec_pretty(doc).map_err(|e| JsonRmwError::serialize(path, e))
    }
}

/// [`DocumentCodec`] for a whole-file UTF-8 text document.
///
/// Why (#4827): `trusty-search`'s `daemon.env` is an operator-owned dotenv file
/// that needs exactly this module's lock, atomic publish and symlink handling —
/// but it is not JSON, and [`DocumentCodec`] is sealed, so the codec has to live
/// here rather than in the consuming crate.
/// What: an absent file decodes to an empty `String`. Invalid UTF-8 is an
/// ERROR, never an empty document — a lossy decode would let a caller's
/// merge-and-republish silently destroy a file it could not read.
/// Test: `update_with_a_text_codec_round_trips`,
/// `text_codec_rejects_invalid_utf8`.
pub struct TextCodec;

impl sealed::Sealed for TextCodec {}

impl DocumentCodec for TextCodec {
    type Document = String;

    fn decode(path: &Path, bytes: Option<&[u8]>) -> Result<String, JsonRmwError> {
        match bytes {
            None => Ok(String::new()),
            Some(raw) => std::str::from_utf8(raw)
                .map(str::to_owned)
                .map_err(|e| JsonRmwError::serialize(path, e)),
        }
    }

    fn encode(_path: &Path, doc: &String) -> Result<Vec<u8>, JsonRmwError> {
        Ok(doc.as_bytes().to_vec())
    }
}

/// Run a locked read-modify-write on the JSON document at `path`.
///
/// Why/What/Test: see [`update_with`] — this is that function specialised to
/// [`JsonCodec`], and is the entry point every existing caller uses.
pub fn update<T, R, E, F>(path: &Path, f: F) -> Result<R, E>
where
    T: DeserializeOwned + Serialize + Default,
    E: From<JsonRmwError>,
    F: FnOnce(&mut T) -> Result<R, E>,
{
    update_with::<JsonCodec<T>, R, E, F>(path, f)
}

#[cfg(test)]
#[path = "json_rmw_tests.rs"]
mod tests;
