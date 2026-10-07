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
//! [`AgentContentError`] is every failure, each naming `tm content update`
//! (and, when nothing is installed, the offline `tm content install --from`). [`crate::harness_doc::HarnessDoc`] reads the
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

/// What every not-installed error tells the operator to run (#9396): the
/// fetch first, then the manual path that needs no API call.
pub const REMEDY: &str = "run `tm content update`, or install by hand: `gh release download \
     <tag> --repo bobmatnyc/trusty-tools`, then `tm content install --from <dir>/<tag>.tar.gz`";

/// How [`describe_source`] names a dev checkout: this prefix, then the root.
const DEV_ORIGIN: &str = "dev checkout ";

/// What clears an error about the content of `origin` (a [`describe_source`] name).
///
/// Why: a dev checkout serves its working tree, so `tm content update`
/// cannot change what it serves (#9396).
/// What: for a checkout, `git pull` there or running tm outside it; for an
/// installed release, `tm content update`.
/// Test: `a_stale_checkout_names_git_pull_not_tm_content_update`.
fn source_remedy(origin: &str) -> String {
    match origin.strip_prefix(DEV_ORIGIN) {
        // #9396: a stale clone is cured in the clone, never by the cache.
        Some(root) => format!(
            "the trusty-tools checkout at {root} serves its working tree, not the installed \
             release: run `git pull` in {root}, or run tm from outside that checkout"
        ),
        None => "run `tm content update`".to_owned(),
    }
}

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
    // #9396: `tm content update` fetches; `--from` is the offline install.
    #[error("no instructional content is installed ({source}); {remedy}", remedy = REMEDY)]
    NotInstalled {
        /// The resolver's `NotInstalled` error.
        #[source]
        source: ContentError,
    },
    /// The home directory is unknown, so the cache cannot be located.
    #[error("cannot locate the content cache (no home directory); {remedy}", remedy = REMEDY)]
    NoCacheDir,
    /// Nothing was installed and the first-use fetch of the content release
    /// failed (#9396); nothing unverified was pinned or served.
    #[error(
        "no instructional content is installed, and fetching the content release \
         failed: {reason}; {remedy}",
        remedy = REMEDY
    )]
    FetchFailed {
        /// Why the fetch failed (unreachable, missing sidecar, checksum, ...).
        reason: String,
    },
    /// Any other resolver failure (checksum, schema, untrusted checkout, I/O).
    #[error("instructional content could not be loaded: {source}")]
    Content {
        /// The resolver error.
        #[source]
        source: ContentError,
    },
    /// The source resolved but holds no `agents/*.md`.
    #[error("the content from {origin} holds no agents; {}", source_remedy(.origin))]
    EmptyRoster {
        /// Where the content came from (see [`describe_source`]).
        origin: String,
    },
    /// A required file (`BASE-AGENT.md`, a harness doc, a named agent) is absent.
    #[error("the content from {origin} has no `{path}`; {}", source_remedy(.origin))]
    Missing {
        /// Where the content came from (see [`describe_source`]).
        origin: String,
        /// The bundle path or roster file name that is absent.
        path: String,
    },
    /// A required file is present but this binary cannot use it (#9012): a
    /// manifest that does not parse or validate, typically one a newer content
    /// release extended with a section or key this binary does not know.
    #[error(
        "the content from {origin} has an unusable `{path}` ({reason}); {}, or upgrade tm \
         if that content is newer than this binary",
        source_remedy(.origin)
    )]
    Invalid {
        /// Where the content came from (see [`describe_source`]).
        origin: String,
        /// The bundle path of the unusable file.
        path: String,
        /// The parse or validation failure.
        reason: String,
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

impl AgentContentError {
    /// Whether nothing is installed: no checkout, no lock, no home directory,
    /// or a first-use fetch that failed (#9396).
    pub fn is_not_installed(&self) -> bool {
        matches!(
            self,
            Self::NotInstalled { .. } | Self::NoCacheDir | Self::FetchFailed { .. }
        )
    }
}

// #9011: set once this process has surfaced the no-content root cause.
static NOT_INSTALLED_REPORTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Reports "no content is installed" as one ERROR line per process (#9011).
///
/// Why: with no content, every roster consumer in a process fails for the same
/// reason. One line naming `tm content install` tells the operator what to do;
/// a line per consumer, or per request, buries it.
/// What: `false` for any other error, which the caller reports as before. For
/// a not-installed `err`, the first report in this process (unless
/// [`mark_not_installed_reported`] ran) logs `"{what}: {err}"` at ERROR and a
/// later one logs at DEBUG; either way it returns `true`, so the caller adds
/// no line of its own. Never changes whether the caller refuses.
/// Test: `only_a_not_installed_error_is_claimed`, and the single-line binary
/// tests `no_content_logs_one_error_naming_tm_content_install` (trusty-code)
/// and `sessions_start_without_content_prints_one_line_naming_the_remedy`
/// (trusty-mpm).
pub fn report_not_installed(err: &AgentContentError, what: &str) -> bool {
    if !err.is_not_installed() {
        return false;
    }
    if NOT_INSTALLED_REPORTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        tracing::debug!("{what}: {err}");
    } else {
        tracing::error!("{what}: {err}");
    }
    true
}

/// Records that the caller surfaces a not-installed error through its own
/// channel (a printed provisioning gap), so a later [`report_not_installed`]
/// in this process stays at DEBUG (#9011).
pub fn mark_not_installed_reported() {
    NOT_INSTALLED_REPORTED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Names a content source for an error message: `dev checkout <root>` or the
/// installed release tag (`content-v0.2.0`).
pub fn describe_source(source: &ContentSource) -> String {
    match source {
        ContentSource::DevCheckout { root } => format!("{DEV_ORIGIN}{}", root.display()),
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
