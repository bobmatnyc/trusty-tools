//! #8878 PR2a critic MEDIUM: which key source `KeySource::ambient` picks.
//!
//! Why: `TRUSTY_SANDBOX=1` alone in a production Architect's environment
//! must not move its hook from the Keychain to a plain 0600 file.
//! What: sets `TRUSTY_SANDBOX` and `TRUSTY_DATA_DIR_OVERRIDE` in this process
//! (the `env_serial` target runs one test at a time) and reads the source
//! for a scratch root and for the account's own `~/.trusty-mpm`. Nothing
//! here reads or writes the Keychain: `ambient` only chooses.
//! Test: the two tests below.

use std::ffi::OsString;

use trusty_mpm::core::presence_key::KeySource;

/// Sets or removes one variable until it drops, then restores it.
struct EnvVar {
    key: &'static str,
    prev: Option<OsString>,
}

impl EnvVar {
    fn set(key: &'static str, value: Option<&std::ffi::OsStr>) -> Self {
        let prev = std::env::var_os(key);
        // SAFETY: the `env_serial` target runs one test at a time (#8345).
        unsafe {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        Self { key, prev }
    }
}

impl Drop for EnvVar {
    fn drop(&mut self) {
        // SAFETY: as `set`.
        unsafe {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

/// The account's own anchor root, from the password database (as `tm` reads it).
fn account_root() -> std::path::PathBuf {
    let user = nix::unistd::User::from_uid(nix::unistd::Uid::current())
        .expect("passwd lookup")
        .expect("passwd entry");
    user.dir.join(".trusty-mpm")
}

/// #8878: the flag alone, in a scratch home or the account's own, keeps the
/// Keychain; so does the full marker pointed at the account's own home.
#[test]
fn the_sandbox_flag_alone_keeps_the_keychain_8878() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let root = scratch.path().join(".trusty-mpm");
    let _flag = EnvVar::set("TRUSTY_SANDBOX", Some("1".as_ref()));
    let _data = EnvVar::set("TRUSTY_DATA_DIR_OVERRIDE", None);
    assert_eq!(KeySource::ambient(&root), KeySource::Keychain, "scratch");
    assert_eq!(
        KeySource::ambient(&account_root()),
        KeySource::Keychain,
        "account home"
    );
    let _data = EnvVar::set("TRUSTY_DATA_DIR_OVERRIDE", Some(scratch.path().as_os_str()));
    assert_eq!(
        KeySource::ambient(&account_root()),
        KeySource::Keychain,
        "full marker, account home"
    );
}

/// The sandbox's full marker — flag, data override, scratch home — selects
/// the key file; an empty override does not.
#[test]
fn the_full_sandbox_marker_selects_the_key_file_8878() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let root = scratch.path().join(".trusty-mpm");
    let _flag = EnvVar::set("TRUSTY_SANDBOX", Some("1".as_ref()));
    let _data = EnvVar::set("TRUSTY_DATA_DIR_OVERRIDE", Some(scratch.path().as_os_str()));
    assert!(
        matches!(KeySource::ambient(&root), KeySource::File(_)),
        "full marker"
    );
    let _empty = EnvVar::set("TRUSTY_DATA_DIR_OVERRIDE", Some("".as_ref()));
    assert_eq!(
        KeySource::ambient(&root),
        KeySource::Keychain,
        "empty override"
    );
}
