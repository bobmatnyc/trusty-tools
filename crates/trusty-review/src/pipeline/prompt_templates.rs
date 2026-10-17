//! Reviewer system prompts, read at runtime from instructional content
//! (ADR-0064, #9015).
//!
//! Why: the two reviewer prompts (stock and coverage-gating) are product
//! prompts, an ADR-0064 content class. A published crate cannot embed a file
//! outside its own directory, and a prompt fix must not need a crate release.
//! So the text lives under `content/instructions/trusty-review/prompts/` and
//! is read through the resolver every harness shares: a trusty-tools checkout
//! above the cwd, else the sha256-pinned bundle `tm content update` installed.
//! What: [`ReviewerPrompts::from_content`] reads both prompts from a resolved
//! source; [`ReviewerPrompts::load`] does it for this process. No content, an
//! absent prompt, or an empty one is a [`PromptContentError`] whose text names
//! the remedy; nothing here answers with a default or partial prompt.
//! Test: `prompt_templates_tests.rs` (one test per error arm), and the binary
//! refusal in `tests/stale_content_e2e.rs`.
//!
//! # Spec References
//! - ADR-0064 decision 5: `docs/adr/0064-instructional-content-tracked-separately-from-code.md`

use trusty_common::content::{
    ContentError, ContentSource, DevOverride, ResolveOptions, ResolvedContent, default_cache_dir,
    resolve,
};

/// Bundle directory of trusty-review's prompts, under the `instructions`
/// class (ADR-0064 amendment 2026-10-01, item 3).
pub const PROMPTS_DIR: &str = "instructions/trusty-review/prompts";

/// File name of the stock reviewer prompt under [`PROMPTS_DIR`].
pub const STOCK_FILE: &str = "system_prompt_stock.md";

/// File name of the coverage-gating reviewer prompt under [`PROMPTS_DIR`].
pub const COVERAGE_GATING_FILE: &str = "system_prompt_coverage_gating.md";

/// What a not-installed error tells the operator to run: the fetch first, then
/// the manual path that needs no API call. The same text as
/// `trusty_agents_common::agent_content::REMEDY` (#9396), restated so this
/// crate takes no dependency on the agent roster.
pub const REMEDY: &str = "run `tm content update`, or install by hand: `gh release download \
     <tag> --repo bobmatnyc/trusty-tools`, then `tm content install --from <dir>/<tag>.tar.gz`";

/// How [`describe_source`] names a dev checkout: this prefix, then the root.
const DEV_ORIGIN: &str = "dev checkout ";

/// The bundle path of prompt file `file` (`system_prompt_stock.md`).
pub fn prompt_path(file: &str) -> String {
    format!("{PROMPTS_DIR}/{file}")
}

/// Names a content source for an error message: `dev checkout <root>` or the
/// installed release tag (`content-v2.2.2`).
pub fn describe_source(source: &ContentSource) -> String {
    match source {
        ContentSource::DevCheckout { root } => format!("{DEV_ORIGIN}{}", root.display()),
        ContentSource::Installed { tag, .. } => tag.clone(),
        other => format!("{other:?}"),
    }
}

/// What clears an error about the content of `origin` (a [`describe_source`]
/// name): `git pull` in a dev checkout, `tm content update` otherwise.
fn source_remedy(origin: &str) -> String {
    match origin.strip_prefix(DEV_ORIGIN) {
        // #9015: a checkout serves its working tree; the cache cannot fix it.
        Some(root) => format!(
            "the trusty-tools checkout at {root} serves its working tree, not the installed \
             release: run `git pull` in {root}, or run trusty-review from outside that checkout"
        ),
        None => "run `tm content update`".to_owned(),
    }
}

/// Every way loading the reviewer prompts can fail.
///
/// Why: the prompts are runtime-only after #9015, so each failure means no
/// review can run. Every arm names what to run; none is answered by reviewing
/// with a missing or empty system prompt.
/// What: `#[non_exhaustive]`; built from a [`ContentError`] by `From`
/// (`NotInstalled` stays `NotInstalled`, anything else is `Content`). Unlike
/// the agent roster's error, there is no write arm: every variant is a content
/// problem, and every variant's text carries its remedy.
/// Test: `no_content_is_not_installed_naming_the_remedy`,
/// `an_unverifiable_bundle_is_a_content_error_naming_the_remedy`,
/// `a_release_without_the_prompts_is_missing_naming_the_path`,
/// `an_empty_prompt_is_invalid`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PromptContentError {
    /// Nothing serves content: no trusted checkout and no `content-lock.toml`.
    #[error("no instructional content is installed ({source}); {remedy}", remedy = REMEDY)]
    NotInstalled {
        /// The resolver's `NotInstalled` error.
        #[source]
        source: ContentError,
    },
    /// The home directory is unknown, so the cache cannot be located.
    #[error("cannot locate the content cache (no home directory); {remedy}", remedy = REMEDY)]
    NoCacheDir,
    /// Any other resolver failure (checksum, schema, corrupt bundle, I/O).
    #[error(
        "instructional content could not be loaded: {source}; run `tm content update` to \
         reinstall the pinned release, or upgrade trusty-review if that content is newer \
         than this binary"
    )]
    Content {
        /// The resolver error.
        #[source]
        source: ContentError,
    },
    /// A prompt file is absent: content released before #9015.
    #[error(
        "the content from {origin} has no `{path}`, which this trusty-review reads as its \
         reviewer prompt; {}",
        source_remedy(.origin)
    )]
    Missing {
        /// Where the content came from (see [`describe_source`]).
        origin: String,
        /// The bundle path that is absent.
        path: String,
    },
    /// A prompt file is present but unusable (empty).
    #[error("the content from {origin} has an unusable `{path}` ({reason}); {}", source_remedy(.origin))]
    Invalid {
        /// Where the content came from (see [`describe_source`]).
        origin: String,
        /// The bundle path of the unusable file.
        path: String,
        /// What is wrong with it.
        reason: String,
    },
}

impl From<ContentError> for PromptContentError {
    fn from(source: ContentError) -> Self {
        match source {
            ContentError::NotInstalled { .. } => Self::NotInstalled { source },
            other => Self::Content { source: other },
        }
    }
}

/// The two reviewer system prompts, read from one content source.
///
/// Why: replaces the two `include_str!` constants (#9015) with the same text,
/// read from content; a caller loads once and builds any number of prompts.
/// What: built only by [`ReviewerPrompts::from_content`], which never returns
/// an empty prompt.
/// Test: `the_repository_prompts_load_byte_identical`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewerPrompts {
    stock: String,
    coverage_gating: String,
}

impl ReviewerPrompts {
    /// Reads both prompts from `content`.
    ///
    /// # Errors
    /// [`PromptContentError::Missing`] naming the first absent prompt,
    /// [`PromptContentError::Invalid`] for an empty one, and the resolver's own
    /// error otherwise.
    /// Test: `a_release_without_the_prompts_is_missing_naming_the_path`,
    /// `an_empty_prompt_is_invalid`.
    pub fn from_content(content: &ResolvedContent) -> Result<Self, PromptContentError> {
        Ok(Self {
            stock: require_prompt(content, STOCK_FILE)?,
            coverage_gating: require_prompt(content, COVERAGE_GATING_FILE)?,
        })
    }

    /// [`ReviewerPrompts::from_content`] over the content resolved for this
    /// process: the checkout above the cwd, else the installed bundle. Never
    /// fetches.
    ///
    /// Test: `no_content_is_not_installed_naming_the_remedy`; end to end in
    /// `tests/stale_content_e2e.rs`.
    pub fn load() -> Result<Self, PromptContentError> {
        // #9015: unit tests read a staged copy of the checkout's prompts, or a
        // source the test names, so they need no `.git` (EVO syncs none, #9619).
        #[cfg(test)]
        if let Some(loaded) = test_support::load_override() {
            return loaded;
        }
        Self::from_content(&resolve_for_process()?)
    }

    /// The stock prompt: no coverage-gating language.
    pub fn stock(&self) -> &str {
        &self.stock
    }

    /// The coverage-gating variant (#1014): the "do not block on coverage"
    /// advisory is replaced with a note that the runner applies a floor.
    pub fn coverage_gating(&self) -> &str {
        &self.coverage_gating
    }

    /// [`Self::coverage_gating`] when `coverage_gating_enabled`, else
    /// [`Self::stock`].
    pub fn for_coverage(&self, coverage_gating_enabled: bool) -> &str {
        if coverage_gating_enabled {
            &self.coverage_gating
        } else {
            &self.stock
        }
    }
}

/// Reads prompt `file`, which must exist and hold text.
fn require_prompt(content: &ResolvedContent, file: &str) -> Result<String, PromptContentError> {
    let path = prompt_path(file);
    let origin = || describe_source(content.source());
    match content.read_to_string(&path) {
        Ok(body) if body.trim().is_empty() => Err(PromptContentError::Invalid {
            origin: origin(),
            path,
            reason: "the file is empty".to_owned(),
        }),
        Ok(body) => Ok(body),
        Err(ContentError::NotFound { .. }) => Err(PromptContentError::Missing {
            origin: origin(),
            path,
        }),
        Err(e) => Err(e.into()),
    }
}

/// Resolves content for this process: the checkout above the cwd (ADR-0064
/// dev override), else the bundle pinned in `~/.trusty-mpm/content`.
fn resolve_for_process() -> Result<ResolvedContent, PromptContentError> {
    let dev = std::env::current_dir().map_or(DevOverride::Off, DevOverride::DetectFrom);
    let cache = default_cache_dir().ok_or(PromptContentError::NoCacheDir)?;
    Ok(resolve(&ResolveOptions::new(cache, dev))?)
}

#[cfg(test)]
#[path = "prompt_templates_support.rs"]
pub(crate) mod test_support;

#[cfg(test)]
#[path = "prompt_templates_tests.rs"]
mod tests;
