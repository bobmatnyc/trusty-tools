//! `tm doctor` `[accounts]` probe (#9091).
//!
//! Why: a malformed `[accounts]` table refuses clones and gives spawned
//! sessions a `gh` that authenticates as nobody. The operator must be able to
//! see that where they look, not only in the daemon log.
//! What: [`check_org_accounts`] reads `~/.trusty-mpm/config.toml` through
//! [`OrgAccounts::inspect`], the same strict read every clone and spawn makes,
//! and reports `Ok` with the mapped orgs, `Warn` for a syntax error in a file
//! where no line names `accounts`, and `Fail` with the error for a table that
//! cannot be read.
//! Read-only.
//! Test: `an_absent_table_is_ok_and_says_missing`,
//! `a_valid_table_is_ok_and_counts_the_orgs`,
//! `a_syntax_error_outside_the_table_warns`,
//! `a_broken_table_fails_with_the_error`.

use std::path::Path;

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::gh_org_accounts::OrgAccounts;

/// The check's name.
const NAME: &str = "org_accounts";

/// Report whether `[accounts]` reads, and what it maps.
///
/// Why: see the module doc.
/// What: `Ok` "missing" for a file or table that is absent, `Ok` with the
/// org count otherwise; `Warn` with the parse error when no line of the file
/// names `accounts`; `Fail` with the error for every [`OrgAccounts::inspect`]
/// `Err`.
/// Test: the four tests named in the module doc.
pub(super) fn check_org_accounts(home: &Path) -> DoctorCheck {
    match OrgAccounts::inspect(&home.join(".trusty-mpm")) {
        Ok((_, Some(warning))) => DoctorCheck::new(NAME, CheckStatus::Warn, warning),
        Ok((accounts, None)) if accounts.is_empty() => DoctorCheck::new(
            NAME,
            CheckStatus::Ok,
            "[accounts] missing in ~/.trusty-mpm/config.toml — no org maps to a gh account; \
             clones and spawns use the registry pin or the ambient identity",
        ),
        Ok((accounts, None)) => DoctorCheck::new(
            NAME,
            CheckStatus::Ok,
            format!(
                "[accounts] maps {} GitHub org(s) to a gh account",
                accounts.len()
            ),
        ),
        Err(e) => DoctorCheck::new(
            NAME,
            CheckStatus::Fail,
            format!(
                "{e} — clones refuse and spawned sessions' gh authenticates as nobody until \
                 it is fixed"
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch home whose `.trusty-mpm/config.toml` holds `body`.
    fn home_with(body: &str) -> tempfile::TempDir {
        let home = tempfile::tempdir().expect("tempdir");
        let root = home.path().join(".trusty-mpm");
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(root.join("config.toml"), body).expect("write");
        home
    }

    #[test]
    fn an_absent_table_is_ok_and_says_missing() {
        for home in [
            tempfile::tempdir().expect("tempdir"),
            home_with("[models]\n"),
        ] {
            let check = check_org_accounts(home.path());
            assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
            assert!(check.message.contains("missing"), "{}", check.message);
        }
    }

    #[test]
    fn a_valid_table_is_ok_and_counts_the_orgs() {
        let home = home_with("[accounts]\nduettoresearch = \"bob-duetto\"\nacme = \"octo\"\n");
        let check = check_org_accounts(home.path());
        assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
        assert!(check.message.contains("maps 2"), "{}", check.message);
    }

    /// #9091: a syntax error with no `[accounts]` header is read past.
    #[test]
    fn a_syntax_error_outside_the_table_warns() {
        let home = home_with("[models]\ndefault = sonnet\n");
        let check = check_org_accounts(home.path());
        assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
        assert!(check.message.contains("config.toml"), "{}", check.message);
    }

    /// 🔴 #9091: a broken table is a `Fail` naming the file and the error.
    #[test]
    fn a_broken_table_fails_with_the_error() {
        let home = home_with("[accounts]\nduettoresearch = bob-duetto\n");
        let check = check_org_accounts(home.path());
        assert_eq!(check.status, CheckStatus::Fail, "{}", check.message);
        assert!(check.message.contains("config.toml"), "{}", check.message);
        assert!(
            check.message.contains("not valid TOML"),
            "{}",
            check.message
        );
    }
}
