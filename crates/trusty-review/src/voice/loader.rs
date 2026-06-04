//! Voice package loader — filesystem discovery and bundled-fixture fallback.
//!
//! Why: voice packages live at `~/.config/trusty-review/voices/<name>/voice.toml`
//! (XDG convention, matching existing trusty-review config placement).  But for
//! production self-containment the crate also ships the `duetto` package as a
//! bundled fixture under `voices/duetto/voice.toml`; if the user-config file is
//! absent the fixture is used automatically.  This makes the feature immediately
//! usable after `cargo install` with no external file setup required.
//!
//! What: `VoiceLoader` holds the base search directories and exposes:
//!   - `load(name)` — look up `<name>` in user-config dir first, then bundled
//!     fixtures; returns `VoicePackage` or `VoiceLoaderError`.
//!   - `list()` — enumerate installed voice names from user-config and bundled.
//!
//! Discovery order: user-config dir wins over bundled fixtures.
//!
//! Test: `load_bundled_duetto_voice`, `load_missing_voice_errors`,
//! `load_from_custom_dir`, `list_includes_bundled_duetto` in voice/tests.rs.

use std::path::{Path, PathBuf};

use thiserror::Error;

use super::types::VoicePackage;

/// Errors produced by the voice loader.
///
/// Why: structured errors let callers decide whether to degrade silently
/// (a missing voice package) or surface loudly (a corrupt TOML file).
/// What: `NotFound` for absent packages; `ParseError` for bad TOML;
/// `Io` for filesystem failures.
/// Test: `load_missing_voice_errors` in voice/tests.rs.
#[derive(Debug, Error)]
pub enum VoiceLoaderError {
    /// No voice package with the given name exists in any search directory.
    #[error("voice package '{name}' not found in any search directory")]
    NotFound { name: String },
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
}

/// Bundled fixture voice packages compiled into the binary.
///
/// Why: shipping the `duetto` package as an embedded string literal means the
/// crate is production-usable out of the box without requiring external files.
/// An operator who wants to customise or override installs their file at the
/// XDG path; it takes precedence automatically.
/// What: the `include_str!` macro bakes the TOML at compile time.
/// Test: `load_bundled_duetto_voice`.
const BUNDLED_DUETTO_VOICE: &str = include_str!("../../voices/duetto/voice.toml");

/// Discovers and loads voice packages from user-config and bundled fixtures.
///
/// Why: single entry point for all voice loading so callers (prompt builder,
/// CLI `voice list/show`) don't have to reason about search-directory order.
/// What: tries `~/.config/trusty-review/voices/<name>/voice.toml` first, then
/// falls back to bundled fixtures; `load()` returns the first match found.
/// `extra_dirs` lets tests inject a temp directory without touching the home dir.
/// `skip_xdg` suppresses the XDG user-config lookup so tests can assert on the
/// bundled fixture exclusively (see `bundled_only()`).
/// Test: `load_bundled_duetto_voice`, `load_from_custom_dir`.
#[derive(Debug, Default)]
pub struct VoiceLoader {
    /// Extra directories prepended to the search path (highest priority).
    extra_dirs: Vec<PathBuf>,
    /// When `true`, skip the XDG user-config path (`~/.config/trusty-review/voices`).
    /// Intended exclusively for tests that must assert on the bundled fixture.
    skip_xdg: bool,
}

impl VoiceLoader {
    /// Construct a loader that searches only the standard XDG config path and
    /// bundled fixtures (no extra directories).
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
    /// Why: tests that assert on the BUNDLED fixture must not accidentally load
    /// an external `~/.config/trusty-review/voices/duetto/voice.toml` installed
    /// by a developer or CI machine — that would silently test the wrong thing.
    /// `bundled_only()` guarantees only the compile-time `include_str!` fixture
    /// is reachable for any given voice name.
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
    /// the configured voice name and rely on graceful fallback to bundled fixtures.
    /// What: tries each search directory (extra dirs → XDG user-config dir) for
    /// `<name>/voice.toml`, then checks bundled fixtures.  Returns the first hit.
    /// Returns `VoiceLoaderError::NotFound` when nothing matches.
    /// Test: `load_bundled_duetto_voice`, `load_missing_voice_errors`,
    /// `load_from_custom_dir`.
    pub fn load(&self, name: &str) -> Result<VoicePackage, VoiceLoaderError> {
        // 1. Search extra dirs (highest priority — test injection + local overrides).
        for base in &self.extra_dirs {
            let candidate = base.join(name).join("voice.toml");
            if candidate.exists() {
                return Self::parse_file(&candidate);
            }
        }

        // 2. XDG user-config path: ~/.config/trusty-review/voices/<name>/voice.toml
        //    Skipped when `skip_xdg = true` (see `bundled_only()`) so tests that
        //    assert on the bundled fixture are not polluted by a developer's
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

        // 3. Bundled fixture fallback.
        if name == "duetto" {
            return toml::from_str::<VoicePackage>(BUNDLED_DUETTO_VOICE).map_err(|source| {
                VoiceLoaderError::ParseError {
                    path: PathBuf::from("<bundled:duetto>"),
                    source,
                }
            });
        }

        Err(VoiceLoaderError::NotFound {
            name: name.to_string(),
        })
    }

    /// List all installed voice package names across all search directories.
    ///
    /// Why: the `voice list` CLI subcommand and health endpoint report available
    /// voices; this aggregates them without duplicates.
    /// What: scans each search directory for subdirectories containing
    /// `voice.toml`; always includes bundled voices that are absent from the
    /// user-config dir.  Returns an alphabetically sorted, deduplicated list.
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

        // Bundled fixtures always included.
        names.insert("duetto".to_string());

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
