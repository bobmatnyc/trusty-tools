//! The agent roster, read at runtime from instructional content (ADR-0064).
//!
//! Why: #9011 drops the 43 compiled-in agent texts. Agents are instructional
//! content, released on their own `content-vX.Y.Z` cadence, so a binary must
//! read them from the one resolver every harness shares — a trusty-tools
//! checkout during development, the sha256-pinned installed bundle otherwise.
//! A missing roster is a loud, typed error that names the fix; no loader ever
//! answers with an empty roster.
//! What: [`resolve_content`] / [`resolve_content_in`] / [`checkout_content`]
//! pick the source; [`AgentRoster`] lists and reads `agents/*.md` from it;
//! [`AgentContentError`] is every failure, each naming `tm content install`
//! or `tm content update`. [`crate::harness_doc::HarnessDoc`] reads the
//! harness-understanding docs from the same source.
//! Test: `not_installed_names_tm_content_install`,
//! `an_unverifiable_bundle_is_a_content_error`, `an_empty_roster_is_an_error`,
//! `a_roster_without_the_foundation_file_is_an_error`, and the roster-content
//! assertions that used to live beside the consts (`agent_content_tests.rs`).
//!
//! # Spec References
//! - ADR-0064 decision 5: `docs/adr/0064-instructional-content-tracked-separately-from-code.md`

use std::path::{Path, PathBuf};

pub use trusty_common::content::{ContentError, ContentSource, DevOverride, ResolvedContent};
use trusty_common::content::{ResolveOptions, default_cache_dir, resolve};

/// The content class holding the agent roster.
pub const AGENTS_CLASS: &str = "agents";

/// The root of every `extends:` chain; a roster without it cannot compose.
pub const FOUNDATION_FILE: &str = "BASE-AGENT.md";

/// Every way loading the agent roster or a harness doc can fail.
///
/// Why: content is runtime-only after #9011, so each failure means a harness
/// has no agents to serve. Every arm tells the operator what to run; none is
/// answered by serving nothing and calling it success.
/// What: `#[non_exhaustive]`; built from a [`ContentError`] by `From`
/// (`NotInstalled` stays `NotInstalled`, anything else is `Content`).
/// Test: `not_installed_names_tm_content_install`,
/// `an_unverifiable_bundle_is_a_content_error`, `an_empty_roster_is_an_error`,
/// `a_roster_without_the_foundation_file_is_an_error`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AgentContentError {
    /// Nothing serves content: no trusted checkout and no `content-lock.toml`.
    #[error(
        "no instructional content is installed ({source}); run `tm content install` \
         (offline: `tm content install --from <bundle.tar.gz>`)"
    )]
    NotInstalled {
        /// The resolver's `NotInstalled` error.
        #[source]
        source: ContentError,
    },
    /// The home directory is unknown, so the cache cannot be located.
    #[error("cannot locate the content cache (no home directory); run `tm content install`")]
    NoCacheDir,
    /// Any other resolver failure (checksum, schema, untrusted checkout, I/O).
    #[error("instructional content could not be loaded: {source}")]
    Content {
        /// The resolver error.
        #[source]
        source: ContentError,
    },
    /// The source resolved but holds no `agents/*.md`.
    #[error("the content from {origin} holds no agents; run `tm content update`")]
    EmptyRoster {
        /// Where the content came from (see [`describe_source`]).
        origin: String,
    },
    /// A required file (`BASE-AGENT.md`, a harness doc, a named agent) is absent.
    #[error("the content from {origin} has no `{path}`; run `tm content update`")]
    Missing {
        /// Where the content came from (see [`describe_source`]).
        origin: String,
        /// The bundle path or roster file name that is absent.
        path: String,
    },
    /// Writing a materialized roster failed.
    #[error("could not write {}: {source}", path.display())]
    Write {
        /// The file or directory that could not be written.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
}

impl From<ContentError> for AgentContentError {
    fn from(source: ContentError) -> Self {
        match source {
            ContentError::NotInstalled { .. } => Self::NotInstalled { source },
            other => Self::Content { source: other },
        }
    }
}

/// Names a content source for an error message: `dev checkout <root>` or the
/// installed release tag (`content-v0.2.0`).
pub fn describe_source(source: &ContentSource) -> String {
    match source {
        ContentSource::DevCheckout { root } => format!("dev checkout {}", root.display()),
        ContentSource::Installed { tag, .. } => tag.clone(),
        other => format!("{other:?}"),
    }
}

/// Resolves content from the default cache (`~/.trusty-mpm/content`) under `dev`.
///
/// Why: the production entry point, so every binary resolves the same way.
/// What: [`AgentContentError::NoCacheDir`] when the home directory is
/// unknown; otherwise [`resolve_content_in`] on the default cache.
/// Test: `not_installed_names_tm_content_install` (via [`resolve_content_in`]).
pub fn resolve_content(dev: DevOverride) -> Result<ResolvedContent, AgentContentError> {
    let cache = default_cache_dir().ok_or(AgentContentError::NoCacheDir)?;
    resolve_content_in(&cache, dev)
}

/// Resolves content from an explicit cache directory under `dev`.
///
/// Test: `not_installed_names_tm_content_install`,
/// `an_unverifiable_bundle_is_a_content_error`.
pub fn resolve_content_in(
    cache_dir: &Path,
    dev: DevOverride,
) -> Result<ResolvedContent, AgentContentError> {
    Ok(resolve(&ResolveOptions::new(cache_dir, dev))?)
}

/// Resolves the checkout at `root` (`DevOverride::At`). Never reads HOME, the
/// cwd or the cache; a `root` that is not a trusted checkout is an error.
///
/// Test: `the_repository_roster_carries_every_agent`.
pub fn checkout_content(root: &Path) -> Result<ResolvedContent, AgentContentError> {
    // `At` either serves `root` or fails; it never reaches the cache, so the
    // cache path is never read.
    resolve_content_in(root, DevOverride::At(root.to_path_buf()))
}

/// The `agents/*.md` files of one resolved content source.
///
/// Why: replaces the compiled-in `agent_assets` table (#9011) with the same
/// `(file name, body)` pairs, read from content.
/// What: top-level `*.md` under `agents/`, `BASE-*` first and then
/// alphabetical. [`AgentRoster::load`] never returns an empty roster or one
/// without [`FOUNDATION_FILE`].
/// Test: `the_repository_roster_carries_every_agent`, `an_empty_roster_is_an_error`.
#[derive(Debug, Clone)]
pub struct AgentRoster {
    source: ContentSource,
    files: Vec<(String, String)>,
}

impl AgentRoster {
    /// Lists and reads every top-level `agents/*.md` of `content`.
    ///
    /// # Errors
    /// [`AgentContentError::EmptyRoster`] on zero files,
    /// [`AgentContentError::Missing`] without [`FOUNDATION_FILE`], and
    /// [`AgentContentError::Content`] when a listed file cannot be read.
    pub fn load(content: &ResolvedContent) -> Result<Self, AgentContentError> {
        let prefix = format!("{AGENTS_CLASS}/");
        let mut files = Vec::new();
        for key in content.list(AGENTS_CLASS)? {
            let Some(name) = key.strip_prefix(&prefix) else {
                continue;
            };
            if name.contains('/') || !name.ends_with(".md") {
                continue;
            }
            files.push((name.to_string(), content.read_to_string(&key)?));
        }
        let origin = describe_source(content.source());
        if files.is_empty() {
            return Err(AgentContentError::EmptyRoster { origin });
        }
        if !files.iter().any(|(name, _)| name == FOUNDATION_FILE) {
            return Err(AgentContentError::Missing {
                origin,
                path: format!("{prefix}{FOUNDATION_FILE}"),
            });
        }
        files.sort_by(|(a, _), (b, _)| {
            (!a.starts_with("BASE-"), a.as_str()).cmp(&(!b.starts_with("BASE-"), b.as_str()))
        });
        Ok(Self {
            source: content.source().clone(),
            files,
        })
    }

    /// Where the roster came from.
    pub fn source(&self) -> &ContentSource {
        &self.source
    }

    /// The source named for an error message (see [`describe_source`]).
    pub fn origin(&self) -> String {
        describe_source(&self.source)
    }

    /// The body of `file_name` (`engineer.md`), if the roster has it.
    pub fn get(&self, file_name: &str) -> Option<&str> {
        self.files
            .iter()
            .find(|(name, _)| name == file_name)
            .map(|(_, body)| body.as_str())
    }

    /// The body of `file_name`, or [`AgentContentError::Missing`].
    pub fn require(&self, file_name: &str) -> Result<&str, AgentContentError> {
        self.get(file_name)
            .ok_or_else(|| AgentContentError::Missing {
                origin: self.origin(),
                path: format!("{AGENTS_CLASS}/{file_name}"),
            })
    }

    /// Every `(file_name, body)` pair, `BASE-*` first.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> + '_ {
        self.files
            .iter()
            .map(|(name, body)| (name.as_str(), body.as_str()))
    }

    /// The number of agent files; never zero for a loaded roster.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Always `false` for a roster [`AgentRoster::load`] returned.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Writes every file into `dir`, one atomic write per file, and returns
    /// the names written. Replaces `AGENT_ASSETS_DIR` for consumers that
    /// compose from a directory. Files already in `dir` that the roster lacks
    /// are left alone.
    ///
    /// Test: `materialize_writes_every_roster_file`.
    pub fn materialize(&self, dir: &Path) -> Result<Vec<String>, AgentContentError> {
        std::fs::create_dir_all(dir).map_err(|source| AgentContentError::Write {
            path: dir.to_path_buf(),
            source,
        })?;
        let mut written = Vec::with_capacity(self.files.len());
        for (name, body) in &self.files {
            let path = dir.join(name);
            trusty_common::atomic_file::write_atomic(&path, body.as_bytes())
                .map_err(|source| AgentContentError::Write { path, source })?;
            written.push(name.clone());
        }
        Ok(written)
    }
}

#[cfg(test)]
#[path = "agent_content_tests.rs"]
pub(crate) mod tests;
