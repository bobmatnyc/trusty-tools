//! Repo-named review skills, read at the PR base commit (#11899, DOC-93).
//!
//! Why: a repository names the review skills trusty-review applies to its own
//! pull requests. A skill comes only from the repository under review
//! (`.claude/skills/<name>/SKILL.md`), is read with its `[[review.skills]]`
//! entry at the base commit (a PR can edit both), is never read for a fork
//! PR, and never fails a review.
//! What: [`RepoSkillsInput`] is what a caller passes through
//! `ReviewOptions::with_repo_skills`: read the entries from the repo's
//! `.trusty-review.toml` at the base commit ([`RepoSkillsInput::from_repo_config`],
//! what CLI `run` and MCP `review_pr` use), or take an explicit request list.
//! [`SkillsCall`] finds the base commit from the diff source and PR head, and
//! `load::load_skills` applies the rules. CLI `run` and MCP `review_pr` read
//! a local checkout's git object store; the hosted webhook path reads the
//! same files through the Contents API at the base SHA (`contents_store`,
//! `SkillsReader::ContentsAtRef`).
//! Test: `tests.rs` and `hosted_tests.rs` in this directory.

mod contents_store; // #11899 step 2: the hosted reader
mod git_store;
mod load;
mod render;
mod skill_format;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::repo_config::REPO_CONFIG_FILENAME;
use crate::config::review_skills::{SkillEntry, parse_skill_entries};
use crate::integrations::context::contents_at_ref::DocFetcher;
use crate::models::{ReviewResult, SkipKind, SkippedSkill};
use crate::pipeline::diff::{DiffSource, extract_changed_files};
use crate::pipeline::optional_context::{ReviewOptions, seams::PrHead};
use crate::voice::VoiceConfig;
pub(crate) use git_store::{BaseTree, BlobRead, GitObjectStore};
pub(crate) use load::{SkillOrigin, load_skills};
pub(crate) use render::body_notice;

/// Largest `SKILL.md` loaded, in bytes (about 2k tokens) — DOC-93 RS-23.
pub const SKILL_CAP_BYTES: usize = 8 * 1024;
/// Largest total of loaded `SKILL.md` bytes per review (about 6k tokens) — RS-24.
pub const TOTAL_CAP_BYTES: usize = 24 * 1024;
/// Largest `.trusty-review.toml` read at the base commit.
const CONFIG_CAP_BYTES: usize = 64 * 1024;
/// Most `[[review.skills]]` entries processed per review; the rest are skipped
/// with one summary entry (#11899).
pub const MAX_SKILL_ENTRIES: usize = 32;

/// One requested skill: one `[[review.skills]]` entry (#11899, DOC-93 §2).
///
/// What: `name` is the `.claude/skills/<name>` directory; `apply_to`, when
/// set and non-empty, is a list of path globs, and the skill applies only
/// when a changed path matches one.
/// Test: `apply_to_gates_the_skill_on_changed_paths`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRequest {
    /// The skill directory name; must be a bare identifier.
    pub name: String,
    /// Path globs (`*` stays within one segment, `**` crosses segments).
    pub apply_to: Option<Vec<String>>,
}

/// Where a review's skill requests come from (#11899).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillsSource {
    /// Read `[[review.skills]]` from `.trusty-review.toml` at the base commit.
    RepoConfig,
    /// Use these requests as given.
    Requests(Vec<SkillRequest>),
}

/// The repo skills one review asks for, and where to read them (#11899).
///
/// What: `checkout` is the local repository a GitHub PR's base commit is read
/// from; a `--base` git-range review reads its own repository root instead.
/// Test: `runner_loads_base_skill_into_the_system_prompt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSkillsInput {
    /// Where the requests come from.
    pub source: SkillsSource,
    /// The local checkout for a GitHub-PR review; `None` loads nothing there.
    pub checkout: Option<PathBuf>,
}

impl RepoSkillsInput {
    /// Read the entries from the repo config at the base commit of `checkout`.
    pub fn from_repo_config(checkout: Option<PathBuf>) -> Self {
        Self {
            source: SkillsSource::RepoConfig,
            checkout,
        }
    }

    /// The CLI and local-MCP input: the git root of the working directory.
    ///
    /// Why: DOC-93 RS-37 — the configuration is found at the git root of the
    /// working directory; the same root `.trusty-review.toml` discovery uses.
    pub fn from_cwd() -> Self {
        let root = std::env::current_dir()
            .ok()
            .map(|cwd| crate::config::find_git_root(&cwd));
        Self::from_repo_config(root)
    }
}

/// The runner's call into the repo-skills loader (#11899).
///
/// Why: keeps the runner to two lines; mirrors `DocsCall`/`FilesCall`.
/// What: [`Self::attach`] reads the entries and skills at the base commit,
/// sets `VoiceConfig::repo_skills` and `ReviewResult::repo_skills`. With no
/// input, or no configured skill, it changes nothing (DOC-93 RS-32, RS-36).
/// Test: `runner_loads_base_skill_into_the_system_prompt`,
/// `runner_without_skills_leaves_prompt_and_record_unchanged`.
pub(crate) struct SkillsCall<'a> {
    input: Option<&'a RepoSkillsInput>,
    reader: SkillsReader,
    fetcher: Option<&'a Arc<dyn DocFetcher>>,
    source: &'a DiffSource,
    head: &'a PrHead,
    raw_diff: &'a str,
}

/// How a review reads the base commit (#11899).
///
/// What: `Checkout` is step 1's local git object store; `ContentsAtRef` is
/// the hosted webhook path's Contents API reader (DOC-93 RS-38).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum SkillsReader {
    /// Read `RepoSkillsInput::checkout` through git.
    #[default]
    Checkout,
    /// Read the base commit of the GitHub PR through the Contents API.
    ContentsAtRef,
}

impl<'a> SkillsCall<'a> {
    /// Capture what the loader needs from the running review.
    pub(crate) fn new(
        options: &'a ReviewOptions,
        source: &'a DiffSource,
        head: &'a PrHead,
        raw_diff: &'a str,
    ) -> Self {
        Self {
            input: options.repo_skills.as_ref(),
            reader: options.repo_skills_reader,
            fetcher: options.doc_fetcher.as_ref(),
            source,
            head,
            raw_diff,
        }
    }

    /// Load the skills through the review's reader (#11899).
    ///
    /// Why: the hosted path has no checkout; its reads are network calls.
    /// What: `ContentsAtRef` reads the base config and skills through the
    /// Contents API first ([`contents_store::find_base`]), then applies the
    /// same rules as [`Self::attach`]; `Checkout` is [`Self::attach`].
    /// Test: `hosted_skill_loads_at_base_and_reaches_the_prompt`,
    /// `runner_hosted_review_loads_base_skill_into_the_system_prompt`.
    pub(crate) async fn resolve(
        self,
        voice: VoiceConfig,
        result: &mut ReviewResult,
    ) -> VoiceConfig {
        let Some(input) = self.input else {
            return voice;
        };
        if self.reader == SkillsReader::Checkout {
            return self.attach(voice, result);
        }
        let base =
            contents_store::find_base(self.source, self.head, self.fetcher, &input.source).await;
        self.finish(input, base, voice, result)
    }

    /// Load the skills into `voice` and record them on `result`.
    pub(crate) fn attach(self, voice: VoiceConfig, result: &mut ReviewResult) -> VoiceConfig {
        let Some(input) = self.input else {
            return voice;
        };
        let base = find_base(self.source, self.head, input.checkout.as_deref());
        self.finish(input, base, voice, result)
    }

    /// Apply the rules to what `base` holds; shared by both readers.
    fn finish(
        &self,
        input: &RepoSkillsInput,
        base: Base,
        mut voice: VoiceConfig,
        result: &mut ReviewResult,
    ) -> VoiceConfig {
        let entries: Vec<SkillEntry> = match &input.source {
            SkillsSource::Requests(requests) => requests.iter().cloned().map(Ok).collect(),
            SkillsSource::RepoConfig => match &base.store {
                Some(store) => config_entries(store.as_ref()),
                None => Vec::new(), // no base commit: the entries are unknowable
            },
        };
        // #11899: no configured skill is silent, fork or not. A fork whose
        // base config cannot be read here still records and says so.
        let config_unknown = input.source == SkillsSource::RepoConfig && base.store.is_none();
        if entries.is_empty() && !(base.fork && config_unknown) {
            return voice;
        }
        let origin = match (&base.store, base.fork) {
            (_, true) => SkillOrigin::Fork {
                base_sha: base.sha.clone(),
            },
            (Some(store), false) => SkillOrigin::Base {
                base_sha: base.sha.clone(),
                tree: store.as_ref(),
            },
            (None, false) => SkillOrigin::NoBase(base.why.clone()),
        };
        let changed = extract_changed_files(self.raw_diff);
        let (block, record) = load_skills(&entries, &origin, &changed);
        tracing::info!(
            loaded = record.loaded.len(),
            skipped = record.skipped.len(),
            fork = record.fork,
            "repo review skills resolved (#11899)"
        );
        voice.repo_skills = (!block.is_empty()).then_some(block);
        result.repo_skills = Some(record);
        voice
    }
}

/// The `[[review.skills]]` entries of the config at the base commit.
///
/// What: an absent file, or one with no or an empty `[[review.skills]]`
/// list, is no entries (silent). A file that cannot be read (git error or
/// timeout, over the cap, not a regular file) or is not UTF-8 TOML is one
/// `config_unreadable` skip naming the reason (#11899: never a silent drop).
/// Test: `config_absent_is_silent_fork_or_not`, `config_empty_skill_list_is_silent_fork_or_not`,
/// `unreadable_base_config_is_a_named_skip`.
fn config_entries(store: &dyn BaseTree) -> Vec<SkillEntry> {
    let unreadable = |reason: String| {
        vec![Err(SkippedSkill {
            name: REPO_CONFIG_FILENAME.to_string(),
            kind: SkipKind::ConfigUnreadable,
            reason,
        })]
    };
    let bytes = match store.read(REPO_CONFIG_FILENAME, CONFIG_CAP_BYTES) {
        BlobRead::Bytes(bytes) => bytes,
        BlobRead::Missing => return Vec::new(),
        BlobRead::TooLarge(n) => {
            return unreadable(format!("{n} bytes is over the {CONFIG_CAP_BYTES}-byte cap"));
        }
        BlobRead::NotRegular(mode) => {
            return unreadable(format!("not a regular file (mode {mode})"));
        }
        BlobRead::Error(e) => return unreadable(e), // #11899: the store names its source
    };
    let parsed = std::str::from_utf8(&bytes)
        .map_err(|e| e.to_string())
        .and_then(parse_skill_entries);
    parsed.unwrap_or_else(|e| unreadable(format!("not valid TOML: {e}")))
}

/// The base commit of this review and the object store holding it.
struct Base {
    store: Option<Box<dyn BaseTree + Send + Sync>>,
    sha: String,
    fork: bool,
    /// Why `store` is `None`.
    why: String,
}

/// Find the base commit: a `--base` range resolves its ref in its own
/// repository (the local path cannot tell a fork; the operator named the
/// base); a GitHub PR uses its base SHA from the API, read in `checkout`, and
/// its fork flag (`head_is_fork`); a diff file or stdin has no base commit.
fn find_base(source: &DiffSource, head: &PrHead, checkout: Option<&Path>) -> Base {
    let none = |why: &str, sha: &str, fork: bool| Base {
        store: None,
        sha: sha.to_string(),
        fork,
        why: why.to_string(),
    };
    match source {
        DiffSource::GitRange {
            repo_root, base, ..
        } => match git_store::resolve_base_commit(repo_root, base) {
            Ok(sha) => Base {
                store: Some(Box::new(GitObjectStore::new(repo_root, &sha))),
                sha,
                fork: false,
                why: String::new(),
            },
            Err(why) => none(&why, "", false),
        },
        DiffSource::Github { .. } => {
            let (sha, fork) = (head.base_sha.as_str(), head.fork);
            if !git_store::is_hex_sha(sha) {
                return none("the pull request base commit is unknown", "", fork);
            }
            let Some(root) = checkout else {
                return none("no local checkout to read the base commit from", sha, fork);
            };
            if !git_store::has_commit(root, sha) {
                let why = "the base commit is not in the local checkout; fetch it first";
                return none(why, sha, fork);
            }
            Base {
                store: Some(Box::new(GitObjectStore::new(root, sha))),
                sha: sha.to_string(),
                fork,
                why: String::new(),
            }
        }
        DiffSource::LocalFile { .. } | DiffSource::Stdin => {
            none("a diff file or stdin carries no base commit", "", false)
        }
    }
}

#[cfg(test)]
mod tests;

// #11899 step 2: the hosted Contents API reader against a fake.
#[cfg(test)]
pub(crate) mod hosted_tests;
