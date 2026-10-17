//! trusty-review's runtime instructional content (ADR-0064, #9015).
//!
//! Why: the reviewer prompts, the report templates, the review templates and
//! the bundled voice package are product content. A published crate cannot
//! embed a file outside its own directory, and a content fix must not need a
//! crate release, so all of it lives under `content/instructions/trusty-review/`
//! and is read through the resolver every harness shares. The code under
//! review must never supply that content: a PR checkout is a user-owned git
//! repo, which the resolver's checkout trust check accepts, so the cwd is never
//! consulted. A checkout serves content only when [`CHECKOUT_ENV`] names it.
//! What: [`resolve_for_process`] resolves the content for this process — the
//! checkout [`CHECKOUT_ENV`] names, else the sha256-pinned bundle `tm content
//! update` installed. [`require_text`] reads one file and refuses an absent or
//! empty one. Every failure is a [`PromptContentError`] whose text names the
//! remedy; nothing here answers with a default.
//! Test: `runtime_content_tests.rs`, and the binary refusals in
//! `tests/stale_content_e2e.rs`.
//!
//! # Spec References
//! - ADR-0064 decision 5: `docs/adr/0064-instructional-content-tracked-separately-from-code.md`

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use trusty_common::content::{
    ContentError, ContentSource, DevOverride, ResolveOptions, ResolvedContent, default_cache_dir,
    resolve,
};

/// Bundle directory of every trusty-review content file, under the
/// `instructions` class (ADR-0064 amendment 2026-10-01, item 3).
pub const CONTENT_DIR: &str = "instructions/trusty-review";

/// The environment variable that opts a trusty-tools checkout in as the
/// content source, naming its root as an absolute path (#9015).
pub const CHECKOUT_ENV: &str = "TRUSTY_REVIEW_CONTENT_CHECKOUT";

/// What a not-installed error tells the operator to run: the fetch first, then
/// the manual path that needs no API call. The same text as
/// `trusty_agents_common::agent_content::REMEDY` (#9396), restated so this
/// crate takes no dependency on the agent roster.
pub const REMEDY: &str = "run `tm content update`, or install by hand: `gh release download \
     <tag> --repo bobmatnyc/trusty-tools`, then `tm content install --from <dir>/<tag>.tar.gz`";

/// How [`describe_source`] names a dev checkout: this prefix, then the root.
const DEV_ORIGIN: &str = "dev checkout ";

/// The bundle path of `rel` under [`CONTENT_DIR`] (`voices/duetto/voice.toml`).
pub fn content_path(rel: &str) -> String {
    format!("{CONTENT_DIR}/{rel}")
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
            "the trusty-tools checkout at {root}, named by {CHECKOUT_ENV}, serves its working \
             tree, not the installed release: run `git pull` in {root}, or unset {CHECKOUT_ENV}"
        ),
        None => "run `tm content update`".to_owned(),
    }
}

/// Every way loading trusty-review's content can fail.
///
/// Why: the prompts, templates and voice are runtime-only after #9015, so each
/// failure means the command that needs them cannot run. Every arm names what
/// to run; none is answered with a missing, empty or default file.
/// What: `#[non_exhaustive]`; built from a [`ContentError`] by `From`
/// (`NotInstalled` stays `NotInstalled`, anything else is `Content`). There is
/// no write arm: every variant is a content problem, and every variant's text
/// carries its remedy.
/// Test: `no_content_is_not_installed_naming_the_remedy`,
/// `an_unverifiable_bundle_is_a_content_error_naming_the_remedy`,
/// `a_release_without_the_prompts_is_missing_naming_the_path`,
/// `an_empty_prompt_is_invalid`, `every_prompt_content_error_names_its_remedy`,
/// `a_relative_checkout_override_is_refused`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PromptContentError {
    /// Nothing serves content: no `content-lock.toml` and no opted-in checkout.
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
    /// A content file is absent: content released before this binary needed it.
    #[error(
        "the content from {origin} has no `{path}`, which this trusty-review reads at runtime; \
         {}",
        source_remedy(.origin)
    )]
    Missing {
        /// Where the content came from (see [`describe_source`]).
        origin: String,
        /// The bundle path that is absent.
        path: String,
    },
    /// A content file is present but unusable (empty, or not a valid voice).
    #[error("the content from {origin} has an unusable `{path}` ({reason}); {}", source_remedy(.origin))]
    Invalid {
        /// Where the content came from (see [`describe_source`]).
        origin: String,
        /// The bundle path of the unusable file.
        path: String,
        /// What is wrong with it.
        reason: String,
    },
    /// [`CHECKOUT_ENV`] names something that cannot serve content (#9015).
    #[error(
        "{env}={} cannot serve content: {reason}; set {env} to the absolute root of a trusted \
         trusty-tools checkout, or unset it to read the content `tm content update` installed",
        value.display(),
        env = CHECKOUT_ENV
    )]
    CheckoutOverride {
        /// The value of [`CHECKOUT_ENV`].
        value: PathBuf,
        /// Why it was refused.
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

/// The dev-override policy a [`CHECKOUT_ENV`] value asks for (#9015).
///
/// Why: a checkout above the cwd may be the code under review (`gh pr
/// checkout N && trusty-review run`), so the cwd is never a source; only an
/// operator who names a checkout gets one.
/// What: unset or empty is [`DevOverride::Off`]; an absolute path is
/// [`DevOverride::At`], which the resolver refuses unless it is a trusted
/// checkout; a relative path is [`PromptContentError::CheckoutOverride`],
/// because it would name the cwd again.
/// Test: `the_checkout_override_is_opt_in`, `a_relative_checkout_override_is_refused`.
pub fn dev_override(value: Option<&OsStr>) -> Result<DevOverride, PromptContentError> {
    match value {
        None => Ok(DevOverride::Off),
        Some(v) if v.is_empty() => Ok(DevOverride::Off),
        Some(v) if Path::new(v).is_absolute() => Ok(DevOverride::At(PathBuf::from(v))),
        Some(v) => Err(PromptContentError::CheckoutOverride {
            value: PathBuf::from(v),
            reason: "it is not an absolute path".to_owned(),
        }),
    }
}

/// Resolves content under `checkout` (a [`CHECKOUT_ENV`] value) and `cache`.
///
/// Test: `the_checkout_override_is_opt_in`,
/// `an_untrusted_checkout_override_is_refused_naming_the_variable`.
pub fn resolve_with(
    checkout: Option<&OsStr>,
    cache: Option<PathBuf>,
) -> Result<ResolvedContent, PromptContentError> {
    let dev = dev_override(checkout)?;
    let cache = cache.ok_or(PromptContentError::NoCacheDir)?;
    let named = match &dev {
        DevOverride::At(root) => Some(root.clone()),
        _ => None,
    };
    resolve(&ResolveOptions::new(cache, dev)).map_err(|e| match (named, e) {
        (
            Some(value),
            e @ (ContentError::NotACheckout { .. } | ContentError::UntrustedCheckout { .. }),
        ) => PromptContentError::CheckoutOverride {
            value,
            reason: e.to_string(),
        },
        (_, e) => e.into(),
    })
}

/// The content for this process: the checkout [`CHECKOUT_ENV`] names, else
/// the bundle pinned in `~/.trusty-mpm/content`. Never the cwd; never fetches.
///
/// Test: `stale_or_absent_content_refuses_run_naming_the_remedy`,
/// `a_checkout_cwd_never_supplies_the_prompts` (tests/stale_content_e2e.rs).
pub fn resolve_for_process() -> Result<Arc<ResolvedContent>, PromptContentError> {
    // #9015: unit tests read a staged bundle of the checkout's content, or a
    // source the test names, so they need no `.git` (EVO syncs none, #9619).
    #[cfg(test)]
    if let Some(resolved) = test_support::resolve_override() {
        return resolved;
    }
    let checkout = std::env::var_os(CHECKOUT_ENV);
    resolve_with(checkout.as_deref(), default_cache_dir()).map(Arc::new)
}

/// Reads bundle path `path` from `content`; it must exist and hold text.
///
/// # Errors
/// [`PromptContentError::Missing`] for an absent file,
/// [`PromptContentError::Invalid`] for an empty one, and the resolver's own
/// error otherwise.
/// Test: `a_release_without_the_prompts_is_missing_naming_the_path`,
/// `an_empty_prompt_is_invalid`.
pub fn require_text(content: &ResolvedContent, path: &str) -> Result<String, PromptContentError> {
    let origin = || describe_source(content.source());
    match content.read_to_string(path) {
        Ok(body) if body.trim().is_empty() => Err(PromptContentError::Invalid {
            origin: origin(),
            path: path.to_owned(),
            reason: "the file is empty".to_owned(),
        }),
        Ok(body) => Ok(body),
        Err(ContentError::NotFound { .. }) => Err(PromptContentError::Missing {
            origin: origin(),
            path: path.to_owned(),
        }),
        Err(e) => Err(e.into()),
    }
}

/// [`require_text`] where an absent file of a name this binary does not ship
/// is `Ok(None)`: the caller's own not-found, not a content defect (#9015).
///
/// Why: a shipped name (`duetto`, `strict-security`) absent from content is
/// stale content and must refuse; any other name may be an operator's own
/// file that simply is not there, which each loader reports as before.
pub(crate) fn optional_text(
    content: &ResolvedContent,
    path: &str,
    shipped: bool,
) -> Result<Option<String>, PromptContentError> {
    match require_text(content, path) {
        Ok(text) => Ok(Some(text)),
        Err(PromptContentError::Missing { .. }) if !shipped => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
#[path = "runtime_content_support.rs"]
pub(crate) mod test_support;

#[cfg(test)]
#[path = "runtime_content_tests.rs"]
mod tests;
