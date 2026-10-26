//! `tm doctor` `gh_account_dir_tokens` probe (#10037).
//!
//! Why: before #10037, `--account-token-stdin` and every `--account` pin wrote
//! the proven gh token as plaintext into tm's per-account `hosts.yml`. tm now
//! keeps it in gh's keyring entry and migrates a legacy file the next time its
//! login is pinned. Until then the plaintext copy stays, and the operator must
//! be able to see it.
//! What: [`check_gh_account_dir_tokens`] scans
//! `<state_root>/gh-accounts/*/hosts.yml` through
//! [`crate::core::gh_keyring_token::plaintext_token_files`]: `Ok` when none
//! holds a token, `Warn` naming each file that does, `Unknown` (never `Ok`)
//! when a file or the dir cannot be read. Names paths only, never a value.
//! Read-only.
//! Test: `doctor_gh_account_dir_tokens_tests` below.

use std::path::{Path, PathBuf};

use crate::core::doctor::{CheckStatus, DoctorCheck};

/// The check's name.
const NAME: &str = "gh_account_dir_tokens";

/// Report whether any tm per-account gh `hosts.yml` still holds a token.
///
/// Why: see the module doc.
/// What: `Warn` when a file holds a token — the remedy is pinning its login
/// again, which moves the token to the keyring; `Unknown` when the scan could
/// not read a file or the dir, since a scan that did not run has not shown the
/// host clean; `Ok` otherwise.
/// Test: `a_host_with_no_account_dirs_is_ok`,
/// `a_legacy_token_file_warns_naming_the_path_not_the_token`,
/// `an_unreadable_hosts_yml_is_unknown_not_ok`.
pub(super) fn check_gh_account_dir_tokens(state_root: &Path) -> DoctorCheck {
    let (holding, unreadable) =
        match crate::core::gh_keyring_token::plaintext_token_files(state_root) {
            Ok(found) => found,
            Err(e) => return DoctorCheck::new(NAME, CheckStatus::Unknown, e),
        };
    if !holding.is_empty() {
        return DoctorCheck::new(
            NAME,
            CheckStatus::Warn,
            format!(
                "{} tm gh account file(s) still hold a plaintext gh token: {}. Pin each login \
                 again (`tm sessions start --account <login> <checkout>`, or add \
                 `--account-token-stdin`) to move the token into the keyring; tm then removes \
                 it from the file (issue #10037).{}",
                holding.len(),
                list(&holding),
                unreadable_note(&unreadable),
            ),
        );
    }
    if !unreadable.is_empty() {
        return DoctorCheck::new(
            NAME,
            CheckStatus::Unknown,
            format!(
                "cannot read {} — whether they hold a plaintext gh token is UNKNOWN",
                list(&unreadable)
            ),
        );
    }
    DoctorCheck::new(
        NAME,
        CheckStatus::Ok,
        "no tm gh account file holds a plaintext token",
    )
}

/// The paths, comma-separated.
fn list(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// A trailing sentence naming unreadable files, or nothing.
fn unreadable_note(paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        String::new()
    } else {
        format!(" Also unreadable: {}.", list(paths))
    }
}

#[cfg(test)]
mod doctor_gh_account_dir_tokens_tests {
    use super::*;

    fn write(root: &Path, login: &str, text: &str) -> PathBuf {
        let dir = root.join("gh-accounts").join(login);
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("hosts.yml"), text).expect("hosts.yml");
        dir.join("hosts.yml")
    }

    #[test]
    fn a_host_with_no_account_dirs_is_ok() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(
            tmp.path(),
            "clean",
            "github.com:\n    users:\n        clean:\n    user: clean\n",
        );
        let check = check_gh_account_dir_tokens(tmp.path());
        assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
        let empty = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            check_gh_account_dir_tokens(empty.path()).status,
            CheckStatus::Ok
        );
    }

    /// 🔴 #10037: a legacy token file warns, naming the path, never the token.
    #[test]
    fn a_legacy_token_file_warns_naming_the_path_not_the_token() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = write(
            tmp.path(),
            "octo",
            "github.com:\n    users:\n        octo:\n            oauth_token: tok-fake\n",
        );
        let check = check_gh_account_dir_tokens(tmp.path());
        assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
        assert!(
            check.message.contains(&path.display().to_string()),
            "{}",
            check.message
        );
        assert!(!check.message.contains("tok-fake"), "{}", check.message);
    }

    /// FAIL-OPEN CHECK: a file the scan cannot parse is UNKNOWN, never `Ok`.
    #[test]
    fn an_unreadable_hosts_yml_is_unknown_not_ok() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(tmp.path(), "broken", ":: [");
        let check = check_gh_account_dir_tokens(tmp.path());
        assert_eq!(check.status, CheckStatus::Unknown, "{}", check.message);
    }
}
