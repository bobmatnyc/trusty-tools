//! Voice package loader — filesystem discovery, else instructional content.
//!
//! Why: voice packages live at `~/.config/trusty-review/voices/<name>/voice.toml`
//! (XDG convention, matching existing trusty-review config placement). The
//! `duetto` package ships as product content (ADR-0064, #9015) at
//! `instructions/trusty-review/voices/duetto/voice.toml`, read at runtime when
//! the user-config file is absent; content that lacks it, empties it or holds
//! a voice that does not parse is a [`VoiceLoaderError::Content`], never a
//! fallback.
//!
//! What: `VoiceLoader` holds the base search directories and exposes:
//!   - `load(name)` — look up `<name>` in user-config dir first, then the
//!     content; returns `VoicePackage` or `VoiceLoaderError`.
//!   - `list()` — enumerate installed voice names from user-config, plus the
//!     shipped ones.
//!
//! Discovery order: user-config dir wins over the content.
//!
//! Test: `load_bundled_duetto_voice`, `load_missing_voice_errors`,
//! `load_from_custom_dir`, `list_includes_bundled_duetto` in voice/tests.rs;
//! `load_rejects_absolute_path_before_any_io`,
//! `load_rejects_parent_traversal_before_any_io` in this module.
//!
//! 🔴 Security (issue #2995): `name` may originate from an attacker-controlled
//! repo-scoped `.trusty-review.toml` `[voice] package` key (any PR author can
//! add one, and since #2995 it can outrank the operator's env var). `load`
//! validates `name` via `crate::identifier::is_valid_identifier` BEFORE any
//! path join or filesystem access — `Path::join` silently discards the base
//! directory for an absolute component (`base.join("/etc/passwd")` ==
//! `/etc/passwd`) and never collapses `..`, so an unsanitised name could read
//! an arbitrary local file whose contents are then injected into the LLM
//! system prompt. See `crate::identifier` for the full threat model.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::identifier::is_valid_identifier;
use crate::runtime_content::{
    PromptContentError, content_path, describe_source, optional_text, resolve_for_process,
};

use super::types::VoicePackage;

/// The voice packages that ship in the instructional content (#9015).
const SHIPPED: &[&str] = &["duetto"];

/// Errors produced by the voice loader.
///
/// Why: structured errors let callers decide whether to degrade silently
/// (a missing voice package) or surface loudly (a corrupt TOML file).
/// `InvalidName` is distinct from `NotFound` so callers/logs can tell "typo"
/// apart from "rejected as a path-traversal attempt" (security fix, #2995).
/// What: `NotFound` for absent (but syntactically valid) packages;
/// `InvalidName` for a name that fails `is_valid_identifier` — returned
/// BEFORE any path join or filesystem access; `ParseError` for bad TOML in
/// a user-config file; `Io` for filesystem failures; `Content` (#9015) when
/// the instructional content cannot supply a usable voice — the review must
/// refuse.
/// Test: `load_missing_voice_errors` in voice/tests.rs;
/// `load_rejects_absolute_path_before_any_io` in this module;
/// `a_stale_release_without_the_shipped_voice_refuses` in voice/tests.rs.
#[derive(Debug, Error)]
pub enum VoiceLoaderError {
    /// No voice package with the given name exists in any search directory.
    #[error("voice package '{name}' not found in any search directory")]
    NotFound { name: String },
    /// `name` is not a bare alphanumeric/`-`/`_` identifier — rejected before
    /// any path join to prevent path traversal / absolute-path file reads
    /// (security fix, issue #2995).
    #[error(
        "voice package name '{name}' is invalid — only bare alphanumeric/-/_ identifiers are \
         allowed (no path separators, '..', or absolute paths); check [voice].package in your \
         config or .trusty-review.toml"
    )]
    InvalidName { name: String },
    /// The `voice.toml` file exists but could not be parsed.
    #[error("failed to parse voice.toml at {path}: {source}")]
    ParseError {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    /// A filesystem I/O error occurred while reading a voice file.
    #[error("I/O error reading voice file at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The instructional content could not supply the voice (#9015); the text
    /// names the remedy.
    #[error(transparent)]
    Content(#[from] PromptContentError),
}

/// Discovers and loads voice packages from user-config and the content.
///
/// Why: single entry point for all voice loading so callers (prompt builder,
/// CLI `voice list/show`) don't have to reason about search-directory order.
/// What: tries `~/.config/trusty-review/voices/<name>/voice.toml` first, then
/// the instructional content; `load()` returns the first match found.
/// `extra_dirs` lets tests inject a temp directory without touching the home dir.
/// `skip_xdg` suppresses the XDG user-config lookup so tests can assert on the
/// shipped voice exclusively (see `bundled_only()`).
/// Test: `load_bundled_duetto_voice`, `load_from_custom_dir`.
#[derive(Debug, Default)]
pub struct VoiceLoader {
    /// Extra directories prepended to the search path (highest priority).
    extra_dirs: Vec<PathBuf>,
    /// When `true`, skip the XDG user-config path (`~/.config/trusty-review/voices`).
    /// Intended exclusively for tests that must assert on the shipped voice.
    skip_xdg: bool,
}

impl VoiceLoader {
    /// Construct a loader that searches only the standard XDG config path and
    /// the content (no extra directories).
    ///
    /// Why: the production call site needs no custom directories; this is the
    /// typical path for the CLI and daemon.
    /// What: creates a `VoiceLoader` with an empty `extra_dirs` list and
    /// `skip_xdg = false` (XDG search enabled).
    /// Test: `load_bundled_duetto_voice`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a loader with additional search directories prepended.
    ///
    /// Why: tests inject a temp directory that holds a hand-crafted `voice.toml`
    /// so they can exercise loader behaviour without writing to `~/.config`.
    /// What: the extra directories are searched before the XDG user-config path.
    /// `skip_xdg` remains `false` so XDG is still searched after extra dirs.
    /// Test: `load_from_custom_dir`.
    pub fn with_extra_dirs(dirs: Vec<PathBuf>) -> Self {
        Self {
            extra_dirs: dirs,
            skip_xdg: false,
        }
    }

    /// Construct a loader that skips the XDG user-config path entirely.
    ///
    /// Why: tests that assert on the SHIPPED voice must not accidentally load
    /// an external `~/.config/trusty-review/voices/duetto/voice.toml` installed
    /// by a developer or CI machine — that would silently test the wrong thing.
    /// `bundled_only()` guarantees only the instructional content is reachable
    /// for any given voice name.
    /// What: returns a loader with no extra dirs and the XDG search path
    /// suppressed (achieved by pre-populating `extra_dirs` with an empty sentinel
    /// value and overriding `load` via the `skip_xdg` flag).  Internally this is
    /// equivalent to `with_extra_dirs(vec![])` PLUS setting `skip_xdg = true`.
    /// Test: `bundled_duetto_matches_external_when_present` in tests_integration.rs.
    pub fn bundled_only() -> Self {
        Self {
            extra_dirs: vec![],
            skip_xdg: true,
        }
    }

    /// Load a voice package by name, searching directories in priority order.
    ///
    /// Why: production callers (prompt builder, health endpoint) call this with
    /// the configured voice name; a shipped voice comes from the content. `name` may originate from an attacker-controlled repo
    /// `.trusty-review.toml` — see the module doc's security note.
    /// What: validates `name` via `is_valid_identifier` FIRST, returning
    /// [`VoiceLoaderError::InvalidName`] before any path join or filesystem
    /// access. Only then tries each search directory (extra dirs → XDG
    /// user-config dir) for `<name>/voice.toml`, then the content's
    /// `instructions/trusty-review/voices/<name>/voice.toml`. Returns
    /// `VoiceLoaderError::NotFound` when nothing has that name, and
    /// `VoiceLoaderError::Content` when the content cannot be resolved, or a
    /// shipped voice is absent, empty or does not parse (#9015).
    /// Test: `load_bundled_duetto_voice`, `load_missing_voice_errors`,
    /// `load_from_custom_dir`, `load_rejects_absolute_path_before_any_io`,
    /// `load_rejects_parent_traversal_before_any_io`.
    pub fn load(&self, name: &str) -> Result<VoicePackage, VoiceLoaderError> {
        if !is_valid_identifier(name) {
            return Err(VoiceLoaderError::InvalidName {
                name: name.to_string(),
            });
        }

        // 1. Search extra dirs (highest priority — test injection + local overrides).
        for base in &self.extra_dirs {
            let candidate = base.join(name).join("voice.toml");
            if candidate.exists() {
                return Self::parse_file(&candidate);
            }
        }

        // 2. XDG user-config path: ~/.config/trusty-review/voices/<name>/voice.toml
        //    Skipped when `skip_xdg = true` (see `bundled_only()`) so tests that
        //    assert on the shipped voice are not polluted by a developer's
        //    external file at this path.
        if !self.skip_xdg
            && let Some(config_dir) = dirs::config_dir()
        {
            let candidate = config_dir
                .join("trusty-review")
                .join("voices")
                .join(name)
                .join("voice.toml");
            if candidate.exists() {
                return Self::parse_file(&candidate);
            }
        }

        // 3. #9015: the instructional content, never a compiled-in default.
        let content = resolve_for_process()?;
        let path = content_path(&format!("voices/{name}/voice.toml"));
        let Some(text) = optional_text(&content, &path, SHIPPED.contains(&name))? else {
            return Err(VoiceLoaderError::NotFound {
                name: name.to_string(),
            });
        };
        // A voice in content that does not parse is a content defect: refuse.
        toml::from_str::<VoicePackage>(&text).map_err(|e| {
            VoiceLoaderError::Content(PromptContentError::Invalid {
                origin: describe_source(content.source()),
                path,
                reason: format!("it does not parse as a voice package: {e}"),
            })
        })
    }

    /// List all installed voice package names across all search directories.
    ///
    /// Why: the `voice list` CLI subcommand and health endpoint report available
    /// voices; this aggregates them without duplicates.
    /// What: scans each search directory for subdirectories containing
    /// `voice.toml`; always includes the shipped voices, which the content
    /// serves (#9015: listing reads no content).  Returns an alphabetically sorted, deduplicated list.
    /// Test: `list_includes_bundled_duetto`.
    pub fn list(&self) -> Vec<String> {
        let mut names = std::collections::BTreeSet::new();

        // Extra dirs.
        for base in &self.extra_dirs {
            collect_voice_names(base, &mut names);
        }

        // XDG user-config dir.
        if let Some(config_dir) = dirs::config_dir() {
            let voices_dir = config_dir.join("trusty-review").join("voices");
            collect_voice_names(&voices_dir, &mut names);
        }

        // The shipped voices are always listed.
        names.extend(SHIPPED.iter().map(|n| (*n).to_string()));

        names.into_iter().collect()
    }

    /// Parse a TOML file into a `VoicePackage`.
    ///
    /// Why: private helper to avoid duplicating read+parse logic.
    /// What: reads the file contents and calls `toml::from_str`.
    /// Test: exercised transitively by `load_bundled_duetto_voice`.
    fn parse_file(path: &Path) -> Result<VoicePackage, VoiceLoaderError> {
        let content = std::fs::read_to_string(path).map_err(|source| VoiceLoaderError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        toml::from_str::<VoicePackage>(&content).map_err(|source| VoiceLoaderError::ParseError {
            path: path.to_path_buf(),
            source,
        })
    }
}

/// Collect subdirectory names that contain a `voice.toml` file.
///
/// Why: the `list()` method needs to scan a directory for installed packages
/// without assuming a specific naming scheme.
/// What: reads `base` directory entries; for each subdir, checks for a
/// `voice.toml`; inserts the subdir name into `names`.  Any I/O error is
/// silently skipped (the directory may not exist).
/// Test: exercised transitively by `list_includes_bundled_duetto`.
fn collect_voice_names(base: &Path, names: &mut std::collections::BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir()
            && path.join("voice.toml").exists()
            && let Some(name) = path.file_name().and_then(|n| n.to_str())
        {
            names.insert(name.to_string());
        }
    }
}
