//! The adapter's own grant file (SPEC-OKF-01 §2.15 "Inbound email", 3l C).
//!
//! Why: an operator-initiated Gmail import may read inbound mail only after
//! the capability-grant review DOC-63 requires, and the adapter, not the
//! host, enforces that (Appendix B "Acquire role"). The record of a passed
//! review therefore lives in the adapter's configuration, which trusty-okf
//! never reads.
//! What: `config.toml` holds `[[inbound_grant]]` rows, and (#10746) the
//! native-Doc export grants `[[gdocs_grant]]` (§2.15 "Native-Doc export",
//! item 10 B), each of `store`, `account` and `review` (the review's
//! reference). [`Config::load`] reads `--config`
//! when given (it must exist), else `$HOME/.config/trusty-connectors/
//! config.toml` when present; no file means no grant. A file that does not
//! parse is an error the Gmail adapter reports on every inbound request,
//! and the gdocs adapter on every request, so a broken grant file never
//! grants. [`Config::inbound_granted`] and [`Config::gdocs_granted`] need a
//! row with a non-blank review reference for exactly that store and account.
//! Test: `config::tests`, `c98_inbound_needs_an_operator_run_and_a_grant`
//! (`tests/conformance.rs`).

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// One passed capability-grant review for inbound mail.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboundGrant {
    /// The store ID the review covers.
    pub store: String,
    /// The account label the review covers.
    pub account: String,
    /// The review's reference (a document or ticket).
    pub review: String,
}

/// The adapter's configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Passed inbound reviews.
    #[serde(default)]
    pub inbound_grant: Vec<InboundGrant>,
    /// #10746: passed native-Doc export reviews (§2.15 "Native-Doc
    /// export", item 10 B), one row per store and account like
    /// `inbound_grant`.
    #[serde(default)]
    pub gdocs_grant: Vec<InboundGrant>,
}

/// The default grant file under `home`.
pub fn default_path(home: &Path) -> PathBuf {
    home.join(".config/trusty-connectors/config.toml")
}

impl Config {
    /// Load `explicit`, else the default file under `$HOME` when present.
    /// Errors: an unreadable or malformed file, or a missing `explicit` one.
    pub fn load(explicit: Option<&Path>) -> Result<Config, String> {
        let path = match explicit {
            Some(p) => p.to_path_buf(),
            None => match std::env::var_os("HOME") {
                Some(home) if !home.is_empty() => {
                    let p = default_path(Path::new(&home));
                    if !p.exists() {
                        return Ok(Config::default());
                    }
                    p
                }
                _ => return Ok(Config::default()),
            },
        };
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("config {}: {e}", path.display()))?;
        toml::from_str(&text).map_err(|e| format!("config {}: {e}", path.display()))
    }

    /// Whether a passed review covers inbound mail for `store` and `account`.
    pub fn inbound_granted(&self, store: &str, account: &str) -> bool {
        covers(&self.inbound_grant, store, account)
    }

    /// #10746: whether a passed review covers native-Doc export for `store`
    /// and `account`, with the semantics of [`Config::inbound_granted`].
    pub fn gdocs_granted(&self, store: &str, account: &str) -> bool {
        covers(&self.gdocs_grant, store, account)
    }
}

/// Whether a row with a non-blank review covers exactly `store` and
/// `account`.
fn covers(grants: &[InboundGrant], store: &str, account: &str) -> bool {
    grants
        .iter()
        .any(|g| g.store == store && g.account == account && !g.review.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grants_need_store_account_and_a_review() {
        let c: Config = toml::from_str(
            "[[inbound_grant]]\nstore = \"s\"\naccount = \"a\"\nreview = \"DOC-63 r1\"\n\
             [[inbound_grant]]\nstore = \"t\"\naccount = \"a\"\nreview = \" \"\n",
        )
        .unwrap();
        assert!(c.inbound_granted("s", "a"));
        assert!(!c.inbound_granted("s", "b"));
        assert!(
            !c.inbound_granted("t", "a"),
            "a blank review grants nothing"
        );
        assert!(toml::from_str::<Config>("[[inbound_grant]]\nstore = \"s\"\n").is_err());
        assert!(toml::from_str::<Config>("grant_all = true\n").is_err());
        // #10746: the gdocs grant is its own table; an inbound grant is not one.
        assert!(!c.gdocs_granted("s", "a"));
        let d: Config = toml::from_str(
            "[[gdocs_grant]]\nstore = \"s\"\naccount = \"a\"\nreview = \"DOC-63 r1\"\n",
        )
        .unwrap();
        assert!(d.gdocs_granted("s", "a") && !d.gdocs_granted("s", "b"));
        assert!(!d.inbound_granted("s", "a"));
    }

    #[test]
    fn a_missing_explicit_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Config::load(Some(&dir.path().join("none.toml"))).is_err());
        let p = dir.path().join("c.toml");
        std::fs::write(&p, "not = [toml").unwrap();
        assert!(Config::load(Some(&p)).is_err());
        std::fs::write(&p, "").unwrap();
        assert_eq!(Config::load(Some(&p)).unwrap(), Config::default());
    }
}
