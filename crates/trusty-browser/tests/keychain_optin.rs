//! Opt-in real-Keychain check: `TRUSTY_BROWSER_KEYCHAIN_TEST=1`.
//!
//! The `keyring` backend cannot target a keychain file, so this drives a
//! throwaway keychain created in a temp dir through `/usr/bin/security`, naming
//! the file on every call. The login, default and search keychains are never
//! read or changed; the file is deleted when the guard drops. Only the vault's
//! store transport differs from production; the key format and AES-GCM path
//! are the vault's own.
#![cfg(target_os = "macos")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use common::state_dir;
use trusty_browser::vault::{KeyStore, Vault, VaultError};

const SERVICE: &str = "com.trusty.browser.rust-optin-test";

fn security(args: &[&str]) -> Output {
    Command::new("/usr/bin/security")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("security: {e}"))
}

/// A keychain file that is deleted on drop.
struct Throwaway {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl Throwaway {
    fn create() -> Throwaway {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let path = dir.path().join("optin.keychain-db");
        let p = path.to_string_lossy().into_owned();
        for args in [
            vec!["create-keychain", "-p", "optin-pw", p.as_str()],
            vec!["unlock-keychain", "-p", "optin-pw", p.as_str()],
            vec!["set-keychain-settings", p.as_str()],
        ] {
            let out = security(&args);
            assert!(
                out.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Throwaway { _dir: dir, path }
    }
}

impl Drop for Throwaway {
    fn drop(&mut self) {
        let _ = security(&["delete-keychain", &self.path.to_string_lossy()]);
    }
}

struct SecurityCliStore(PathBuf);

impl SecurityCliStore {
    fn run(&self, args: &[&str], service: &str) -> Result<Output, VaultError> {
        if service != SERVICE {
            return Err(VaultError::Store(
                "only the test service may be named".into(),
            ));
        }
        let keychain = self.0.to_string_lossy();
        let mut all: Vec<&str> = args.to_vec();
        all.push(&keychain);
        Ok(security(&all))
    }
}

impl KeyStore for SecurityCliStore {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, VaultError> {
        let out = self.run(
            &["find-generic-password", "-s", service, "-a", account, "-w"],
            service,
        )?;
        Ok(out
            .status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned()))
    }
    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), VaultError> {
        let out = self.run(
            &[
                "add-generic-password",
                "-U",
                "-s",
                service,
                "-a",
                account,
                "-w",
                value,
            ],
            service,
        )?;
        if out.status.success() {
            Ok(())
        } else {
            Err(VaultError::Store(
                String::from_utf8_lossy(&out.stderr).into_owned(),
            ))
        }
    }
    fn delete(&self, service: &str, account: &str) -> Result<bool, VaultError> {
        let out = self.run(
            &["delete-generic-password", "-s", service, "-a", account],
            service,
        )?;
        Ok(out.status.success())
    }
}

fn exists(p: &Path) -> bool {
    p.exists()
}

#[test]
fn throwaway_keychain_round_trip() {
    if std::env::var("TRUSTY_BROWSER_KEYCHAIN_TEST").as_deref() != Ok("1") {
        eprintln!("skipped: TRUSTY_BROWSER_KEYCHAIN_TEST != 1");
        return;
    }
    let keychain = Throwaway::create();
    let kc_path = keychain.path.clone();
    let state = state_dir();
    let vault = Vault::with_service(
        state.path(),
        SERVICE,
        Box::new(SecurityCliStore(kc_path.clone())),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let sealed = vault
        .seal("browser:p", "k")
        .unwrap_or_else(|e| panic!("{e}"));
    let reader = Vault::with_service(
        state.path(),
        SERVICE,
        Box::new(SecurityCliStore(kc_path.clone())),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        reader.unseal("browser:p", &sealed).ok().as_deref(),
        Some("k")
    );
    drop(keychain);
    assert!(!exists(&kc_path), "throwaway keychain deleted");
}
