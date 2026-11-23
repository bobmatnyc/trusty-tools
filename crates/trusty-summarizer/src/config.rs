//! TOML configuration and where it is found.
//!
//! Why: backends are chosen by name from configuration (plan section 2), and
//! every setting that changes output also feeds the cache key.
//!
//! What: [`Config::load`] reads, in order, an explicit `--config` path, the
//! file named by `TRUSTY_SUMMARIZER_CONFIG`, then
//! `$XDG_CONFIG_HOME/trusty-summarizer/config.toml` or
//! `~/.config/trusty-summarizer/config.toml`. An explicit or env-named file
//! must exist; the default path is optional, and its absence means defaults.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Environment variable naming a config file.
pub const CONFIG_ENV: &str = "TRUSTY_SUMMARIZER_CONFIG";
/// Environment variable naming the cache directory.
pub const CACHE_DIR_ENV: &str = "TRUSTY_SUMMARIZER_CACHE_DIR";
/// The reserved classifier name meaning "no classifier configured".
pub const NO_CLASSIFIER: &str = "none";

/// The whole configuration. Every field has a default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Files processed at once; `None` means the CPU count, capped at 8.
    pub concurrency: Option<usize>,
    /// Emit line and paragraph ranges in the outline.
    pub references: bool,
    /// Files larger than this get a `too_large` error record.
    pub max_file_bytes: u64,
    /// Outline settings.
    pub outline: OutlineConfig,
    /// The `find` command.
    pub find: FindConfig,
    /// Summary settings.
    pub summary: SummaryConfig,
    /// Classifier settings.
    pub classifier: ClassifierConfig,
    /// Cache settings.
    pub cache: CacheConfig,
    /// Converter worker settings and limits.
    pub convert: ConvertConfig,
    /// The warm server.
    pub warm: WarmConfig,
    /// Free-form options per backend name, passed to that backend's factory:
    /// `[backends.extractive] max_sentences = 3`.
    pub backends: BTreeMap<String, toml::Table>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            concurrency: None,
            references: true,
            max_file_bytes: 64 * 1024 * 1024,
            outline: OutlineConfig::default(),
            find: FindConfig::default(),
            summary: SummaryConfig::default(),
            classifier: ClassifierConfig::default(),
            cache: CacheConfig::default(),
            convert: ConvertConfig::default(),
            warm: WarmConfig::default(),
            backends: BTreeMap::new(),
        }
    }
}

/// `[outline]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutlineConfig {
    /// A section is split at a paragraph once it spans this many lines;
    /// with `describe`, no entry spans more than this many lines.
    pub max_section_lines: usize,
    /// Step 7: each entry lists the names, numbers and terms that
    /// distinguish its range, and long sections are cut so no entry spans
    /// more than `max_section_lines` lines. Default `false`, the step 6
    /// (plain) outline: on the held-out questions the described outline
    /// added tokens and no accuracy (owner ruling Dnb, 2026-11-15).
    pub describe: bool,
    /// With `describe`: at most this many terms per entry.
    pub max_terms: usize,
    /// With `describe`: the terms of one entry, joined with `", "`, are at
    /// most this many characters.
    pub max_term_chars: usize,
}

impl Default for OutlineConfig {
    fn default() -> Self {
        OutlineConfig {
            max_section_lines: 60,
            // Ruling Dnb: the plain outline is the default.
            describe: false,
            max_terms: 6,
            max_term_chars: 80,
        }
    }
}

/// `[find]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FindConfig {
    /// `false` turns the `find` command off.
    pub enabled: bool,
    /// At most this many hits per request.
    pub max_hits: usize,
    /// Lines of context before and after each hit.
    pub context_lines: usize,
    /// Each returned line is cut to this many characters.
    pub max_line_chars: usize,
}

impl Default for FindConfig {
    fn default() -> Self {
        FindConfig {
            enabled: true,
            max_hits: 20,
            context_lines: 1,
            max_line_chars: 160,
        }
    }
}

/// `[summary]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SummaryConfig {
    /// Inference backend name: `extractive-tf` (no model), `extractive`
    /// (local embedding model, no LLM), or a configured endpoint
    /// (`type = "openai"`), which is the `llm` mode.
    pub backend: String,
    /// The instruction sent with the excerpt.
    pub instruction: String,
    /// Most characters of document text sent to a backend: the opening plus
    /// the most central sections (see [`crate::excerpt::bounded_excerpt`]).
    pub max_input_chars: usize,
    /// Output budget passed to the backend.
    pub max_output_tokens: u32,
}

impl Default for SummaryConfig {
    fn default() -> Self {
        SummaryConfig {
            backend: "extractive-tf".into(),
            instruction: "Summarize this document in two or three sentences. \
                          Answer with the summary only."
                .into(),
            max_input_chars: 32_000,
            max_output_tokens: 200,
        }
    }
}

/// `[classifier]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClassifierConfig {
    /// Classifier backend name; `none` means no classifier.
    pub backend: String,
    /// The labels to score against; empty means [`default_labels`].
    pub labels: Vec<Label>,
}

impl Default for ClassifierConfig {
    fn default() -> Self {
        ClassifierConfig {
            backend: NO_CLASSIFIER.into(),
            labels: Vec::new(),
        }
    }
}

/// One classification label. The description is the prototype text an
/// embedding classifier compares against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Label {
    /// The label written to the record.
    pub name: String,
    /// What a document with this label is about.
    #[serde(default)]
    pub description: String,
}

/// The label set used when `[[classifier.labels]]` is absent: common
/// kinds of general documents.
pub fn default_labels() -> Vec<Label> {
    [
        (
            "specification",
            "a specification, design document, plan or requirements",
        ),
        (
            "report",
            "a report of findings, a survey, study or analysis",
        ),
        (
            "contract or legal",
            "a contract, agreement, policy, terms or legal text",
        ),
        (
            "financial",
            "an invoice, budget, financial statement or accounts",
        ),
        (
            "correspondence",
            "a letter, email or message between people",
        ),
        (
            "meeting notes",
            "notes or minutes from a meeting, with attendees and actions",
        ),
        ("source code", "program source code or a script"),
        (
            "data",
            "a table, spreadsheet, data export or configuration data",
        ),
        ("presentation", "slides from a presentation or a slide deck"),
        ("other", "anything else"),
    ]
    .into_iter()
    .map(|(name, description)| Label {
        name: name.into(),
        description: description.into(),
    })
    .collect()
}

/// `[cache]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CacheConfig {
    /// Read and write the result cache.
    pub enabled: bool,
    /// Cache directory; see [`Config::cache_dir`] for the fallback order.
    pub dir: Option<PathBuf>,
}

impl Default for CacheConfig {
    fn default() -> Self {
        CacheConfig {
            enabled: true,
            dir: None,
        }
    }
}

/// `[convert]`: the converter worker and its per-file limits.
///
/// None of these change a successful result, so none is in the cache key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConvertConfig {
    /// The worker binary; default: found beside the executable or on PATH
    /// (see [`crate::worker::locate_worker`]).
    pub worker: Option<PathBuf>,
    /// Worker processes kept at most; each serves many files. Default 2.
    pub workers: usize,
    /// Time allowed for one file, in milliseconds. Default 60000.
    pub timeout_ms: u64,
    /// Time allowed for a worker to start and say hello, in milliseconds.
    /// Default 10000 (macOS scans a new binary on its first launch).
    pub start_timeout_ms: u64,
    /// Largest worker answer for one file, in bytes (the JSON line holding
    /// the text). Default 64 MiB.
    pub max_output_bytes: u64,
}

impl Default for ConvertConfig {
    fn default() -> Self {
        ConvertConfig {
            worker: None,
            workers: 2,
            timeout_ms: 60_000,
            start_timeout_ms: 10_000,
            max_output_bytes: 64 * 1024 * 1024,
        }
    }
}

/// `[warm]`: the on-demand background server (plan section 4).
///
/// Neither key changes a result, so neither is in the cache key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WarmConfig {
    /// Run `summarize` through the warm server; `false` is `--no-warm`.
    pub enabled: bool,
    /// The server exits after this many seconds with no request. Default
    /// 600. Read when a call starts a server; a running server keeps the
    /// value it was started with.
    pub idle_timeout_secs: u64,
}

impl Default for WarmConfig {
    fn default() -> Self {
        WarmConfig {
            enabled: true,
            idle_timeout_secs: 600,
        }
    }
}

impl Config {
    /// Makes every path the configuration names absolute, against `cwd`:
    /// `cache.dir` (resolved through [`Config::cache_dir`] when unset),
    /// `convert.worker`, and each backend's `cache_dir` option.
    ///
    /// Why: a warm server has its own working directory and environment;
    /// a configuration that still depended on the caller's would be
    /// computed under different settings there.
    /// Errors: as [`Config::cache_dir`].
    /// Test: `relative_paths_are_resolved_against_the_caller`
    pub fn resolve_paths(&mut self, cwd: &Path) -> Result<()> {
        let abs = |p: &Path| {
            if p.is_absolute() || p.starts_with("~") {
                p.to_path_buf()
            } else {
                cwd.join(p)
            }
        };
        self.cache.dir = Some(abs(&self.cache_dir()?));
        if let Some(w) = &self.convert.worker {
            self.convert.worker = Some(abs(w));
        }
        for options in self.backends.values_mut() {
            if let Some(toml::Value::String(dir)) = options.get_mut("cache_dir") {
                *dir = abs(Path::new(dir.as_str())).display().to_string();
            }
        }
        Ok(())
    }

    /// Parses a TOML document and validates it.
    pub fn from_toml(text: &str) -> Result<Self> {
        let config: Config = toml::from_str(text).map_err(|e| Error::Config(e.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    /// Loads the configuration; returns it with the path it came from.
    ///
    /// Why: one documented search order, so a later warm server and the
    /// one-shot CLI read the same file.
    /// What: `explicit`, then `TRUSTY_SUMMARIZER_CONFIG`, then the default
    /// path; defaults when none applies. A named file that is missing or
    /// invalid is an [`Error::Config`].
    /// Test: `explicit_config_must_exist`, `config_parses_and_validates`
    pub fn load(explicit: Option<&Path>) -> Result<(Self, Option<PathBuf>)> {
        let named = explicit
            .map(Path::to_path_buf)
            .or_else(|| std::env::var_os(CONFIG_ENV).map(PathBuf::from));
        let path = match named {
            Some(p) => p,
            None => match default_config_path() {
                Some(p) if p.is_file() => p,
                _ => return Ok((Config::default(), None)),
            },
        };
        let text = std::fs::read_to_string(&path)
            .map_err(|e| Error::Config(format!("cannot read {}: {e}", path.display())))?;
        let config = Config::from_toml(&text)
            .map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
        Ok((config, Some(path)))
    }

    /// Checks the cross-field rules TOML parsing cannot express.
    pub fn validate(&self) -> Result<()> {
        if self.concurrency == Some(0) {
            return Err(Error::Config("concurrency must be at least 1".into()));
        }
        if self.find.max_hits == 0 || self.find.max_line_chars == 0 {
            return Err(Error::Config(
                "find.max_hits and find.max_line_chars must be at least 1".into(),
            ));
        }
        if self.outline.max_section_lines == 0 {
            return Err(Error::Config(
                "outline.max_section_lines must be at least 1".into(),
            ));
        }
        let c = &self.convert;
        if [
            c.workers as u64,
            c.timeout_ms,
            c.start_timeout_ms,
            c.max_output_bytes,
        ]
        .contains(&0)
        {
            return Err(Error::Config(
                "convert.workers, timeout_ms, start_timeout_ms and max_output_bytes must be at least 1"
                    .into(),
            ));
        }
        if self.warm.idle_timeout_secs == 0 {
            return Err(Error::Config(
                "warm.idle_timeout_secs must be at least 1".into(),
            ));
        }
        if self.summary.backend.trim().is_empty() {
            return Err(Error::Config("summary.backend must not be empty".into()));
        }
        if self.summary.max_input_chars == 0 {
            return Err(Error::Config(
                "summary.max_input_chars must be at least 1".into(),
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for l in &self.classifier.labels {
            if l.name.trim().is_empty() || !seen.insert(l.name.to_lowercase()) {
                return Err(Error::Config(format!(
                    "classifier label names must be non-empty and unique (`{}`)",
                    l.name
                )));
            }
        }
        Ok(())
    }

    /// The labels in use: `[[classifier.labels]]`, else [`default_labels`].
    pub fn labels(&self) -> Vec<Label> {
        if self.classifier.labels.is_empty() {
            default_labels()
        } else {
            self.classifier.labels.clone()
        }
    }

    /// The effective worker count.
    pub fn effective_concurrency(&self) -> usize {
        self.concurrency.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get().min(8))
                .unwrap_or(4)
        })
    }

    /// The cache directory: `cache.dir`, then `TRUSTY_SUMMARIZER_CACHE_DIR`,
    /// then `$XDG_CACHE_HOME/trusty-summarizer`, then
    /// `~/.cache/trusty-summarizer`.
    pub fn cache_dir(&self) -> Result<PathBuf> {
        if let Some(dir) = &self.cache.dir {
            return Ok(dir.clone());
        }
        if let Some(dir) = std::env::var_os(CACHE_DIR_ENV) {
            return Ok(PathBuf::from(dir));
        }
        if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
            return Ok(PathBuf::from(xdg).join("trusty-summarizer"));
        }
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(".cache").join("trusty-summarizer"))
            .ok_or_else(|| Error::Config("no cache directory: set cache.dir or HOME".into()))
    }

    /// The options table for a backend name; empty when absent.
    pub fn backend_options(&self, name: &str) -> toml::Table {
        self.backends.get(name).cloned().unwrap_or_default()
    }
}

fn default_config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("trusty-summarizer").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_parses_and_validates() {
        let c = Config::from_toml(
            r#"
            references = false
            [summary]
            backend = "extractive"
            [classifier]
            backend = "fastembed"
            [[classifier.labels]]
            name = "spec"
            description = "a specification"
            [backends.extractive]
            max_sentences = 2
            "#,
        )
        .unwrap();
        assert!(!c.references);
        assert_eq!(c.classifier.labels[0].name, "spec");
        assert_eq!(
            c.backend_options("extractive").get("max_sentences"),
            Some(&toml::Value::Integer(2))
        );
        let c = Config::from_toml("[classifier]\nbackend = \"x\"").unwrap();
        assert_eq!(c.labels().len(), 10);
        assert!(
            Config::from_toml(
                "[[classifier.labels]]\nname = \"a\"\n[[classifier.labels]]\nname = \"A\""
            )
            .is_err()
        );
        assert!(Config::from_toml("[summary]\nmax_input_chars = 0").is_err());
        assert!(Config::from_toml("concurrency = 0").is_err());
        assert!(Config::from_toml("[convert]\ntimeout_ms = 0").is_err());
        let c = Config::from_toml("[convert]\ntimeout_ms = 5\nworker = \"/w\"").unwrap();
        assert_eq!((c.convert.timeout_ms, c.convert.workers), (5, 2));
        assert!(Config::from_toml("typo = 1").is_err());
    }

    /// Ruling Dnb: with no configuration the outline is plain and `find`
    /// is on; `outline.describe = true` still turns the described one on.
    #[test]
    fn defaults_are_plain_outline_with_find_on() {
        let c = Config::default();
        assert!(!c.outline.describe);
        assert!(c.find.enabled);
        assert_eq!(Config::from_toml("").unwrap(), c);
        let described = Config::from_toml("[outline]\ndescribe = true").unwrap();
        assert!(described.outline.describe);
    }

    #[test]
    fn relative_paths_are_resolved_against_the_caller() {
        let mut c = Config::from_toml(
            "[cache]\ndir = \"c\"\n[convert]\nworker = \"bin/w\"\n\
             [backends.local]\ncache_dir = \"models\"",
        )
        .unwrap();
        c.resolve_paths(Path::new("/work")).unwrap();
        assert_eq!(c.cache.dir.as_deref(), Some(Path::new("/work/c")));
        assert_eq!(c.convert.worker.as_deref(), Some(Path::new("/work/bin/w")));
        assert_eq!(
            c.backend_options("local").get("cache_dir"),
            Some(&toml::Value::String("/work/models".into()))
        );
        assert!(Config::from_toml("[warm]\nidle_timeout_secs = 0").is_err());
    }

    #[test]
    fn explicit_config_must_exist() {
        let err = Config::load(Some(Path::new("/nonexistent/trusty.toml"))).unwrap_err();
        assert_eq!(err.kind(), "config");
    }
}
