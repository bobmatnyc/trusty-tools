//! `[okg]` config section — which directories a doc-store ingest may read.
//!
//! Why: `okg_ingest_docstore` takes a path from a MODEL, so the set of readable
//! directories is a security boundary and therefore belongs to the OPERATOR,
//! not to the model or to a hardcoded constant. Real corpora live in arbitrary
//! places (`~/Duetto/cto-resources`), so the list has to be extensible; the
//! default has to be useful without being `/`.
//!
//! What: [`OkgConfig`] is the `[okg]` section of `~/.trusty-agents/config.toml`.
//! `docstore_roots` defaults to the user's home directory, and
//! [`trusty_kb::okg::policy::DocStorePolicy`] additionally excludes every
//! hidden path below a root — so `~/Documents` works out of the box while
//! `~/.ssh`, `~/.aws`, and `~/.gnupg` are refused without needing to be named.
//!
//! Test: `super::tests::config_defaults_to_home`,
//! `config_roots_expand_tilde`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use trusty_kb::okg::policy::DocStorePolicy;

/// `[okg]` section of the global config.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct OkgConfig {
    /// Directories a doc-store ingest may read, `~`-expandable.
    ///
    /// Empty (the default, and the state of every config file predating this
    /// field) means "the user's home directory". It deliberately does NOT mean
    /// "everything" — see [`OkgConfig::policy`].
    #[serde(default)]
    pub docstore_roots: Vec<String>,
}

impl OkgConfig {
    /// Build the read-confinement policy this config describes.
    ///
    /// Why: the tool layer should never assemble a policy by hand — one place
    /// decides what the default means, so the default cannot drift open.
    /// What: expands `~` in each configured root; falls back to `[home]` when
    /// none are configured.
    /// Test: `config_defaults_to_home`, `config_roots_expand_tilde`.
    pub fn policy(&self, home: &std::path::Path) -> DocStorePolicy {
        if self.docstore_roots.is_empty() {
            return DocStorePolicy::home_default(home);
        }
        let roots = self
            .docstore_roots
            .iter()
            .map(|raw| expand_tilde(raw, home))
            .collect();
        DocStorePolicy::new(roots)
    }
}

/// Expand a leading `~` against `home`; other paths pass through unchanged.
pub(super) fn expand_tilde(raw: &str, home: &std::path::Path) -> PathBuf {
    match raw.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if raw == "~" => home.to_path_buf(),
        _ => PathBuf::from(raw),
    }
}

/// Load the `[okg]` section from the global config, tolerating its absence.
///
/// Why: an unreadable or `[okg]`-less config is the normal state for every
/// existing install; it must yield the default policy, never an error that
/// blocks ingestion outright.
/// What: reads `~/.trusty-agents/config.toml` and pulls out `[okg]`.
pub fn load() -> OkgConfig {
    let Some(home) = home_dir() else {
        return OkgConfig::default();
    };
    let path = home.join(".trusty-agents").join("config.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return OkgConfig::default();
    };
    #[derive(Deserialize)]
    struct Wrapper {
        #[serde(default)]
        okg: OkgConfig,
    }
    toml::from_str::<Wrapper>(&text)
        .map(|w| w.okg)
        .unwrap_or_default()
}

/// The user's home directory, from the environment.
pub(super) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}
