//! The tga → trusty-review DD-manifest adapter (DOC-67 §6, #5236).
//!
//! Why: `tga audit` and trusty-review are separate processes with no Cargo edge
//! between them (DOC-67 §5). One TOML file is the entire contract: tga names the
//! engagement and the repositories, trusty-review renders. Keeping the builder
//! pure — data in, structure out, the caller writes the file — is what makes the
//! field mapping provable in unit tests instead of only observable by running a
//! full audit.
//! What: [`DdManifest`] and its two sections, [`DdManifestOptions`] for the
//! run-scoped engagement metadata, and [`build_dd_manifest`], which maps a
//! resolved tga [`Config`] onto trusty-review's manifest schema per §6's
//! field table. [`DdManifest::to_toml`] serializes.
//! Test: `super::dd_manifest_tests`.
//!
//! ## Two properties a reviewer should check first
//!
//! **No credential reaches the file.** The manifest is handed to a third party.
//! Every string this module emits passes through
//! [`trusty_common::credentials::scrub_secrets`] with the credentials the
//! config holds as needles, so a token that reached a repository name, a CLI
//! title, or a stage's error message is removed rather than merely unlikely to
//! be there. It is a guarantee about the output, not a claim about the inputs.
//!
//! **The same input yields the same bytes** (DOC-67 §9). Nothing here reads the
//! clock, the environment, or the filesystem, and every collection is an
//! ordered `Vec` walked in config order. The one machine-dependent value is the
//! repository path, which is load-bearing — trusty-review scans that checkout —
//! and so is emitted as configured.

use std::path::{Path, PathBuf};

use serde::Serialize;
use trusty_common::credentials::scrub_secrets;

use crate::core::config::Config;

/// Failures the DD-manifest adapter can report.
///
/// Why: a library module, so a typed error rather than `anyhow` — the caller
/// (`tga audit`) turns these into operator-facing text.
/// What: an empty repository set (the manifest schema requires at least one
/// entry, so producing it would only move the failure into trusty-review with a
/// less actionable message), and TOML serialization failure.
/// Test: `super::dd_manifest_tests::empty_config_is_an_actionable_error`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DdManifestError {
    /// The resolved config names no repositories.
    #[error(
        "no repositories to audit: add entries under `repositories:` in config.yaml \
         (or let `tga install` discover them) before running `tga audit`"
    )]
    NoRepositories,

    /// The manifest could not be serialized as TOML.
    #[error("failed to serialize the DD manifest as TOML: {0}")]
    Serialize(#[from] toml::ser::Error),
}

/// Engagement metadata for one audit run.
///
/// Why: DOC-67 §6 maps the report's title/analyst/client from CLI flags, and §2
/// forbids obtaining any of them interactively — so each is simply absent when
/// not supplied and the template renders its own fallback. `gaps` is the channel
/// §9 requires: areas the sweep could not assess, carried into the report rather
/// than left in the orchestrator's stderr.
/// What: the four run-scoped values; everything else comes from [`Config`].
/// Test: `super::dd_manifest_tests::maps_engagement_metadata`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct DdManifestOptions {
    /// Report title, e.g. `"Acme — Technical Due Diligence"`.
    pub title: String,
    /// Analyst producing the report; `None` renders the template's fallback.
    pub analyst: Option<String>,
    /// Client the report is produced for; `None` renders the fallback.
    pub client: Option<String>,
    /// Gaps & Caveats lines for areas this run could not assess.
    pub gaps: Vec<String>,
    /// Directory a relative `RepositoryConfig.path` is relative to.
    ///
    /// Why: tga resolves a relative repository path against the process's
    /// working directory, and trusty-review resolves one against the MANIFEST's
    /// directory — and the manifest is written into the audit's output
    /// directory, which is a different place. Copying the path through verbatim
    /// therefore points the renderer at a checkout that does not exist, and its
    /// only symptom is a report with no analysis and no stated reason. The
    /// caller supplies its working directory; the builder does the join, so it
    /// still performs no I/O.
    /// What: prefixed onto every relative repository path. Absolute paths pass
    /// through untouched. Empty (the default) leaves paths as configured.
    /// Test: `super::dd_manifest_tests::relative_paths_are_anchored_to_base_dir`.
    pub base_dir: PathBuf,
}

/// A trusty-review report manifest, as tga produces it.
///
/// Why: mirrors `trusty_review::report::manifest`'s TOML shape without a Cargo
/// dependency on that crate (DOC-67 §5 — the file is the seam). Only the four
/// fields §6's table maps are declared; every other key trusty-review accepts is
/// deliberately absent so its own defaults apply.
/// What: the `[report]` section plus one `[[repositories]]` entry per configured
/// repository, in config order.
/// Test: `super::dd_manifest_tests::round_trips_through_the_review_schema`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DdManifest {
    /// The `[report]` metadata section.
    pub report: DdReportSection,
    /// One entry per audited repository, in config order.
    pub repositories: Vec<DdRepositoryEntry>,
}

/// The `[report]` section of a DD manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DdReportSection {
    /// Report title (also the output slug seed).
    pub title: String,
    /// Analyst name; omitted from the TOML when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub analyst: Option<String>,
    /// Client name; omitted from the TOML when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    /// Named unassessed areas; omitted from the TOML when empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<String>,
}

/// One audited repository.
///
/// Why: §6 fixes the mapping — every AUDIT repo is a local checkout by
/// construction, `slug` is trusty-review's to derive, `git_ref` is whatever HEAD
/// is at collection time, and `metrics` must stay unset so the live `--analyze`
/// fetch is not blocked by a declared file.
/// What: the name and the checkout path, and nothing else.
/// Test: `super::dd_manifest_tests::names_fall_back_to_the_directory_basename`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DdRepositoryEntry {
    /// Display name for the application section.
    pub name: String,
    /// Local checkout path trusty-review scans.
    pub path: PathBuf,
}

impl DdManifest {
    /// Serialize to the TOML text trusty-review's `load_manifest` reads.
    ///
    /// Why: the caller writes the file; keeping serialization here means the
    /// determinism property is testable without touching disk.
    /// What: `toml::to_string_pretty` over the declared field order.
    /// Test: `super::dd_manifest_tests::two_builds_are_byte_identical`.
    ///
    /// # Errors
    ///
    /// [`DdManifestError::Serialize`] if a value cannot be represented in TOML
    /// (a non-UTF-8 repository path is the only realistic case).
    pub fn to_toml(&self) -> Result<String, DdManifestError> {
        Ok(toml::to_string_pretty(self)?)
    }
}

/// Build the DD manifest for one audit run.
///
/// Why: this is the whole tga→trusty-review seam (DOC-67 §6). It exists as a
/// pure function so the field mapping, the determinism property, and the
/// no-credential property are provable from unit tests rather than from a live
/// audit — none of which would be true if it wrote the file itself.
///
/// What: maps `cfg.repositories` onto `[[repositories]]` in config order, taking
/// each entry's `name` or falling back to its directory basename, and fills
/// `[report]` from `opts`. Every emitted string is scrubbed of the credentials
/// `cfg` holds, so a token that leaked into a name, a title, or a gap line is
/// removed before it can reach an artifact. No I/O, no clock, no environment
/// read: two calls on the same input produce equal values.
///
/// Test: `super::dd_manifest_tests` — the field mapping, the basename fallback,
/// `two_builds_are_byte_identical`, and `configured_token_never_reaches_the_manifest`.
///
/// # Errors
///
/// [`DdManifestError::NoRepositories`] when the config names none.
pub fn build_dd_manifest(
    cfg: &Config,
    opts: &DdManifestOptions,
) -> Result<DdManifest, DdManifestError> {
    if cfg.repositories.is_empty() {
        return Err(DdManifestError::NoRepositories);
    }

    // #5236: the needle set is derived once, then applied to every string that
    // leaves this function — the guarantee is about the output, not about which
    // input fields we happened to remember are sensitive.
    let secrets = configured_secrets(cfg);
    let clean = |s: &str| scrub_secrets(s, &secrets);

    let repositories = cfg
        .repositories
        .iter()
        .map(|repo| DdRepositoryEntry {
            name: clean(&repo_name(repo.name.as_deref(), &repo.path)),
            path: anchor(&opts.base_dir, &repo.path),
        })
        .collect();

    Ok(DdManifest {
        report: DdReportSection {
            title: clean(&opts.title),
            analyst: opts.analyst.as_deref().map(&clean),
            client: opts.client.as_deref().map(&clean),
            gaps: opts.gaps.iter().map(|g| clean(g)).collect(),
        },
        repositories,
    })
}

/// Anchor a possibly-relative repository path to `base`.
///
/// Why/What: see [`DdManifestOptions::base_dir`]. An absolute path, or an empty
/// `base`, is returned unchanged; a pure join otherwise, with no filesystem
/// access and therefore no canonicalization.
/// Test: `super::dd_manifest_tests::relative_paths_are_anchored_to_base_dir`.
fn anchor(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() || base.as_os_str().is_empty() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

/// The display name for a repository: its configured name, else the directory
/// basename (`config/mod.rs`'s own documented fallback).
fn repo_name(configured: Option<&str>, path: &Path) -> String {
    match configured.map(str::trim).filter(|n| !n.is_empty()) {
        Some(name) => name.to_string(),
        None => path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string()),
    }
}

/// Every credential the resolved config holds, as scrub needles.
///
/// Why: `scrub_secrets` can only remove values the caller already knows, so the
/// needle set decides how much the guarantee is worth. These are the fields tga
/// itself reads to authenticate — the ones an error message or an expanded
/// `${GITHUB_TOKEN}` can carry into text this module emits.
/// What: the GitHub / Bitbucket / JIRA / Linear / Azure-DevOps / OpenRouter
/// credentials, skipping absent and empty values. Never logged or serialized —
/// the return value's only use is as a needle set.
/// Test: `super::dd_manifest_tests::configured_token_never_reaches_the_manifest`.
fn configured_secrets(cfg: &Config) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |v: Option<&String>| {
        if let Some(v) = v.filter(|v| !v.is_empty()) {
            out.push(v.clone());
        }
    };

    push(cfg.github.as_ref().and_then(|g| g.token.as_ref()));
    push(cfg.bitbucket.as_ref().and_then(|b| b.token.as_ref()));
    push(cfg.bitbucket.as_ref().and_then(|b| b.app_password.as_ref()));
    push(cfg.jira.as_ref().and_then(|j| j.token.as_ref()));
    push(cfg.linear.as_ref().and_then(|l| l.api_key.as_ref()));
    push(
        cfg.classification
            .as_ref()
            .and_then(|c| c.openrouter_api_key.as_ref()),
    );
    if let Some(azdo) = cfg.pm.as_ref().and_then(|p| p.azure_devops.as_ref()) {
        if !azdo.pat.is_empty() {
            out.push(azdo.pat.clone());
        }
    }
    out
}

#[cfg(test)]
#[path = "dd_manifest_tests.rs"]
mod dd_manifest_tests;
