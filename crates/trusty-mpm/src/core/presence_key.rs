//! The key that seals Architect presence grants (#8878 PR2a, Bob D5b Q3).
//!
//! Why: a `<pid>.presence` grant is a file in `~/.trusty-mpm/architect-launch/`.
//! The pm-guard keeps PMs and agents from writing there, but a same-uid shell
//! outside any hook can write any file the user owns. Bob ruled (D5b Q3) that
//! a grant is HMAC-sealed with a Keychain-held key, so a hand-written grant
//! carries no valid seal.
//! What: [`GrantKey`] seals and verifies a message with HMAC-SHA256.
//! [`KeySource::Keychain`] keeps the 256-bit key in the login Keychain
//! (service [`KEYCHAIN_SERVICE`]); macOS trusts only the binary that created
//! the item, so another program reading it raises an access prompt.
//! [`KeySource::File`] is the test sandbox's key: a 0600 file in the scratch
//! home's anchor directory, chosen only under the full sandbox marker
//! ([`sandbox_isolated`]).
//! Residuals (owner ruling Q9b: only Bob types outside a hook):
//! - [`KeySource::load`] does not authenticate the item: it takes whatever
//!   item answers [`KEYCHAIN_SERVICE`]. A same-uid process outside any hook
//!   can pre-plant that item before the first `tm presence local`, or delete
//!   and replace it, and then seal its own grants. It can also replace the
//!   `tm` binary itself. Hooked callers — PMs, agents, subagents and the
//!   Architect — cannot, except the residuals below: the pm-guard floor
//!   `pm_guard_presence_key` denies every `security` add, delete or
//!   partition-list write of the item, every `security` verb built at run
//!   time, globbed or abbreviated, and every Keychain API write naming it.
//! - Hooked-caller residuals, accepted for PR2a: an interpreter that
//!   assembles the service name at run time (`python3` `keyring`,
//!   `osascript`), and `security list-keychains -s` shadowing the item.
//!   #10865 adds the access-list check (`SecKeychainItemCopyAccess`). Until
//!   it ships the key is not trusted for any approval decision: a grant only
//!   adds a requirement to the Architect's anchor writes.
//! - A rebuilt `tm` is a new binary to the Keychain, so its first read raises
//!   the access prompt once. A hook that meets it does not wait out
//!   [`KEYCHAIN_TIMEOUT`]: the guard's 8 s deadline (release builds,
//!   GP-049 `guard-timeout`) or Claude Code's 10 s hook timeout with
//!   `onFailure: "block"` (debug builds) ends the run first, and the call is
//!   refused with the timeout text, not the presence remedy.
//!
//! Test: `presence_tests.rs` (`a_forged_grant_without_the_key_is_refused`,
//! `a_sandbox_key_file_round_trips_and_refuses_a_symlink`);
//! `tests/presence_key_sandbox_8878.rs` for [`KeySource::ambient`].

use std::ffi::OsStr;
use std::io::Read as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use hmac::{Hmac, Mac as _};
use sha2::Sha256;

use crate::core::architect_launch::ARCHITECT_DIR;

/// The key length, in bytes.
pub const KEY_LEN: usize = 32;

/// The Keychain service of the grant key.
pub const KEYCHAIN_SERVICE: &str = "trusty-mpm.presence";

/// The Keychain account of the grant key.
pub const KEYCHAIN_ACCOUNT: &str = "grant-hmac-v1";

/// The sandbox key file, in the anchor directory.
pub const SANDBOX_KEY_FILE: &str = "presence-hmac.key";

/// How long one Keychain call may wait (an access prompt holds it open).
pub const KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(20);

/// Why the grant key could not be read or made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// The Keychain refused, timed out, or does not exist on this platform.
    #[error("the Keychain did not answer: {0}")]
    Keychain(String),
    /// The sandbox key file is unsafe or unreadable.
    #[error("the presence key file {0} is unusable: {1}")]
    File(PathBuf, String),
    /// No random bytes could be read.
    #[error("no random bytes for a new key: {0}")]
    Random(String),
}

/// A 256-bit HMAC-SHA256 key. `Debug` never prints it.
#[derive(Clone, PartialEq, Eq)]
pub struct GrantKey([u8; KEY_LEN]);

impl std::fmt::Debug for GrantKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GrantKey(..)")
    }
}

impl GrantKey {
    /// A key holding `bytes`.
    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// A fresh key from `/dev/urandom`.
    pub fn generate() -> Result<Self, KeyError> {
        let mut bytes = [0u8; KEY_LEN];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut bytes))
            .map_err(|e| KeyError::Random(e.to_string()))?;
        Ok(Self(bytes))
    }

    /// A key read from stored bytes; `None` unless exactly [`KEY_LEN`] long.
    fn from_slice(bytes: &[u8]) -> Option<Self> {
        <[u8; KEY_LEN]>::try_from(bytes).ok().map(Self)
    }

    /// The HMAC-SHA256 tag of `message`, hex.
    pub fn seal(&self, message: &[u8]) -> String {
        hex::encode(self.mac(message).finalize().into_bytes())
    }

    /// Whether `tag` (hex) is `message`'s tag, compared in constant time.
    pub fn verify(&self, message: &[u8], tag: &str) -> bool {
        hex::decode(tag).is_ok_and(|tag| self.mac(message).verify_slice(&tag).is_ok())
    }

    fn mac(&self, message: &[u8]) -> Hmac<Sha256> {
        let mut mac = <Hmac<Sha256> as hmac::Mac>::new_from_slice(&self.0)
            .expect("HMAC accepts a key of any length");
        mac.update(message);
        mac
    }
}

/// Where the grant key lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    /// The login Keychain, service [`KEYCHAIN_SERVICE`].
    Keychain,
    /// A 0600 file: the test sandbox's key only.
    File(PathBuf),
}

impl KeySource {
    /// The key source for `root` (`~/.trusty-mpm`): the sandbox file when
    /// `sandbox` ([`sandbox_isolated`]), else the Keychain.
    pub fn for_root(root: &Path, sandbox: bool) -> Self {
        if sandbox {
            Self::File(root.join(ARCHITECT_DIR).join(SANDBOX_KEY_FILE))
        } else {
            Self::Keychain
        }
    }

    /// The process's key source: [`Self::for_root`] under
    /// [`sandbox_isolated`] over this process's environment.
    ///
    /// Test: `the_sandbox_flag_alone_keeps_the_keychain_8878`,
    /// `the_full_sandbox_marker_selects_the_key_file_8878`.
    pub fn ambient(root: &Path) -> Self {
        // #8878: `TRUSTY_SANDBOX=1` alone does not move the key to a file.
        let sandbox = sandbox_isolated(
            std::env::var_os("TRUSTY_SANDBOX").as_deref(),
            std::env::var_os(trusty_common::DATA_DIR_OVERRIDE_ENV).as_deref(),
            root,
            crate::core::host_state_gate::passwd_home_dir().as_deref(),
        );
        Self::for_root(root, sandbox)
    }

    /// The stored key; `Ok(None)` when none was ever made.
    ///
    /// Test: `a_sandbox_key_file_round_trips_and_refuses_a_symlink`.
    pub fn load(&self) -> Result<Option<GrantKey>, KeyError> {
        let bytes = match self {
            Self::Keychain => keychain::get()?,
            Self::File(path) => read_key_file(path)?,
        };
        match bytes {
            None => Ok(None),
            Some(bytes) => GrantKey::from_slice(&bytes)
                .map(Some)
                .ok_or_else(|| self.bad_length()),
        }
    }

    /// The stored key, made and stored first when none exists.
    ///
    /// Test: `a_sandbox_key_file_round_trips_and_refuses_a_symlink`.
    pub fn load_or_create(&self) -> Result<GrantKey, KeyError> {
        if let Some(key) = self.load()? {
            return Ok(key);
        }
        let key = GrantKey::generate()?;
        match self {
            Self::Keychain => keychain::set(&key.0)?,
            Self::File(path) => crate::core::architect_session::write_owner_only(path, &key.0)
                .map_err(|why| KeyError::File(path.clone(), why))?,
        }
        // #8878 PR2a: read back, so a key that did not land grants nothing.
        self.load()?.ok_or_else(|| self.bad_length())
    }

    fn bad_length(&self) -> KeyError {
        let why = format!("the stored key is not {KEY_LEN} bytes");
        match self {
            Self::Keychain => KeyError::Keychain(why),
            Self::File(path) => KeyError::File(path.clone(), why),
        }
    }
}

/// Whether the key may be the sandbox file: the marker
/// `scripts/sandbox_daemon.sh` sets in full.
///
/// Why: #8878 critic MEDIUM — `TRUSTY_SANDBOX=1` alone in an Architect's
/// environment would move its hook to a 0600 file any same-uid process can
/// write. The sandbox also sets `TRUSTY_DATA_DIR_OVERRIDE` and a scratch
/// `$HOME`.
/// What: `flag` is exactly `1`, `data_override` is non-empty, and `root`'s
/// parent (the home it sits in) is not `account_home`, the password
/// database's home, compared canonical when both resolve. No account home,
/// or a root with no parent, is not a sandbox.
/// Test: `the_sandbox_flag_alone_keeps_the_keychain_8878`,
/// `the_full_sandbox_marker_selects_the_key_file_8878`.
pub fn sandbox_isolated(
    flag: Option<&OsStr>,
    data_override: Option<&OsStr>,
    root: &Path,
    account_home: Option<&Path>,
) -> bool {
    let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let away = match (root.parent(), account_home) {
        (Some(home), Some(account)) => canonical(home) != canonical(account),
        _ => false,
    };
    flag.is_some_and(|v| v == "1") && data_override.is_some_and(|v| !v.is_empty()) && away
}

/// The sandbox key file's bytes: opened without following a symlink, a
/// regular file of this user's, not readable by others.
fn read_key_file(path: &Path) -> Result<Option<Vec<u8>>, KeyError> {
    let fail = |why: String| KeyError::File(path.to_path_buf(), why);
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(fail(e.to_string())),
        Ok(file) => file,
    };
    let meta = file.metadata().map_err(|e| fail(e.to_string()))?;
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    if !meta.is_file() || meta.mode() & 0o077 != 0 || meta.uid() != euid {
        return Err(fail("not a 0600 regular file of this user".to_owned()));
    }
    let mut bytes = Vec::new();
    file.take(KEY_LEN as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| fail(e.to_string()))?;
    Ok(Some(bytes))
}

/// Run `f` on its own thread, giving up after [`KEYCHAIN_TIMEOUT`].
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn timed<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, KeyError> + Send + 'static,
) -> Result<T, KeyError> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(KEYCHAIN_TIMEOUT).unwrap_or_else(|_| {
        Err(KeyError::Keychain(
            "no answer in time (an access prompt may be open)".to_owned(),
        ))
    })
}

#[cfg(target_os = "macos")]
mod keychain {
    use super::{KEYCHAIN_ACCOUNT, KEYCHAIN_SERVICE, KeyError, timed};

    fn entry() -> Result<keyring::Entry, KeyError> {
        keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)
            .map_err(|e| KeyError::Keychain(e.to_string()))
    }

    /// The key item's bytes; `None` when there is no item.
    pub(super) fn get() -> Result<Option<Vec<u8>>, KeyError> {
        timed(|| match entry()?.get_secret() {
            Ok(bytes) => Ok(Some(bytes)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(KeyError::Keychain(e.to_string())),
        })
    }

    /// Store `bytes` as the key item.
    pub(super) fn set(bytes: &[u8]) -> Result<(), KeyError> {
        let bytes = bytes.to_vec();
        timed(move || {
            entry()?
                .set_secret(&bytes)
                .map_err(|e| KeyError::Keychain(e.to_string()))
        })
    }
}

#[cfg(not(target_os = "macos"))]
mod keychain {
    use super::KeyError;

    const NONE: &str = "this platform has no login Keychain";

    pub(super) fn get() -> Result<Option<Vec<u8>>, KeyError> {
        Err(KeyError::Keychain(NONE.to_owned()))
    }

    pub(super) fn set(_: &[u8]) -> Result<(), KeyError> {
        Err(KeyError::Keychain(NONE.to_owned()))
    }
}
