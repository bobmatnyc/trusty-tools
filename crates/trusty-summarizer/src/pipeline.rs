//! The per-file pipeline and the bounded-concurrency batch.
//!
//! Why: the one place that composes conversion, segmentation, the two
//! backends and the cache, so every caller (CLI, a later warm server, tests)
//! gets the same records.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Instant;

use crate::backend::{
    BackendRegistry, ClassifierBackend, Credentials, InferenceBackend, InferenceRequest,
};
use crate::backend::{llm_inference_name, type_of};
use crate::cache::{Cache, content_hash};
use crate::config::{Config, Label, NO_CLASSIFIER};
use crate::convert::{
    CODE, ConvertInput, Converters, MARKDOWN, TEXT, convert_cached, detect_format, read_limited,
};
use crate::error::{Error, Result};
use crate::excerpt::bounded_excerpt;
use crate::labeler::LLM_TYPE;
use crate::record::{
    BackendsUsed, CacheStatus, CachedResult, ClassResult, ErrorInfo, ErrorRecord, FileRecord,
    LabelScore, OutlineEntry, Stage, StageStats, estimate_tokens,
};
use crate::segment::{SegmentOptions, meaningful_title, segment};
use crate::terms::{TermLimits, section_terms};
use crate::text::LineIndex;
use crate::walk;

/// Bumped when the meaning of a cached result changes.
/// 3 (step 7): outline entries carry terms and a hard line bound, so no
/// entry stored by an earlier build is served.
/// 4 (trial): results carry `text_chars` and `tokens_est`.
/// 5 (B4): converted-document titles skip footer and boilerplate lines.
/// 6 (spec PR #2): converted PDFs carry `[table ...]` blocks.
pub(crate) const CACHE_SCHEMA: u32 = 6;

/// Counts for a finished batch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BatchStats {
    /// Records written.
    pub files: usize,
    /// Of those, error records.
    pub errors: usize,
    /// Of those, results served from cache.
    pub cache_hits: usize,
}

/// Per-batch inputs that are not configuration.
///
/// Why: a warm server runs batches for clients with other working
/// directories and other keys than its own.
/// What: `base` resolves relative roots (paths in records stay as given);
/// `credentials` is where API keys come from for this batch only.
#[derive(Debug, Clone, Default)]
pub struct BatchOptions {
    /// Directory relative roots are resolved against; `None` is the
    /// process's own working directory.
    pub base: Option<PathBuf>,
    /// API keys for this batch.
    pub credentials: Credentials,
}

/// A configured summarizer: backends built, cache opened.
///
/// Why: the library's entry point. Building it once resolves every backend
/// name, so a batch never fails half-way on configuration.
/// What: [`Summarizer::new`] validates the config, builds the named
/// inference and classifier backends through `backends`, and opens the cache
/// unless it is disabled. Thread-safe; one instance serves a whole batch.
/// Test: `test_backend_selected_by_name_without_caller_change`,
/// `second_run_is_served_from_cache`, `changed_file_or_setting_recomputes`
pub struct Summarizer {
    config: Config,
    converters: Converters,
    inference: Arc<dyn InferenceBackend>,
    classifier: Option<Arc<dyn ClassifierBackend>>,
    labels: Vec<Label>,
    cache: Option<Cache>,
    settings: String,
}

impl std::fmt::Debug for Summarizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Summarizer")
            .field("config", &self.config)
            .field("converters", &self.converters)
            .field("cache", &self.cache)
            .finish_non_exhaustive()
    }
}

impl Summarizer {
    /// Builds a summarizer.
    ///
    /// Errors: [`Error::Config`] for an invalid config,
    /// [`Error::UnknownBackend`] for an unregistered backend name, any error
    /// a backend factory returns, and [`Error::Cache`] when the cache
    /// directory cannot be created.
    pub fn new(config: Config, backends: &BackendRegistry, converters: Converters) -> Result<Self> {
        config.validate()?;
        let summary_name = &config.summary.backend;
        let inference = backends.inference(summary_name, &config.backend_options(summary_name))?;
        let classifier = backends.build_classifier(&config)?;
        let labels = config.labels();
        let cache = if config.cache.enabled {
            Some(Cache::open(&config.cache_dir()?)?)
        } else {
            None
        };
        let settings = settings_fingerprint(&config)?;
        Ok(Summarizer {
            config,
            converters,
            inference,
            classifier,
            labels,
            cache,
            settings,
        })
    }

    /// The configuration in use.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Processes one file. Never fails: a failure is an error record.
    pub fn summarize_file(&self, path: &Path) -> FileRecord {
        self.summarize_file_with(path, &BatchOptions::default())
    }

    fn summarize_file_with(&self, path: &Path, options: &BatchOptions) -> FileRecord {
        let mut stages = Vec::new();
        match self.run(path, options, &mut stages) {
            Ok(record) => record,
            Err(e) => error_record(path, &e, stages),
        }
    }

    /// Processes `roots` (files and directories) with at most
    /// `config.concurrency` files in flight, handing each record to `sink`
    /// in input order as soon as it and every earlier record are done.
    ///
    /// Why: batch is the first-class use; ordered streaming keeps output
    /// deterministic without holding the whole batch.
    /// What: per-file failures become error records and the batch goes on.
    /// The batch stops early only when `sink` returns an error (for
    /// example, a closed stdout); that error is returned.
    /// Test: `batch_continues_past_bad_files`
    pub fn summarize_batch<F>(&self, roots: &[PathBuf], sink: F) -> std::io::Result<BatchStats>
    where
        F: FnMut(FileRecord) -> std::io::Result<()>,
    {
        self.summarize_batch_with(roots, &BatchOptions::default(), sink)
    }

    /// [`Summarizer::summarize_batch`] with a working directory and keys
    /// for this batch; the records are the ones the in-process call gives.
    /// Test: `warm_output_matches_in_process_and_one_server_serves_both_calls`
    pub fn summarize_batch_with<F>(
        &self,
        roots: &[PathBuf],
        options: &BatchOptions,
        mut sink: F,
    ) -> std::io::Result<BatchStats>
    where
        F: FnMut(FileRecord) -> std::io::Result<()>,
    {
        let items = walk::expand_in(options.base.as_deref(), roots);
        let total = items.len();
        let workers = self.config.effective_concurrency().clamp(1, total.max(1));
        let next = AtomicUsize::new(0);
        let mut stats = BatchStats::default();
        let (tx, rx) = mpsc::channel::<(usize, FileRecord)>();

        std::thread::scope(|scope| {
            for _ in 0..workers {
                let tx = tx.clone();
                let (next, items) = (&next, &items);
                scope.spawn(move || {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(i) else { break };
                        let record = match item {
                            Ok(path) => self.summarize_file_with(path, options),
                            Err((path, e)) => error_record(path, e, Vec::new()),
                        };
                        if tx.send((i, record)).is_err() {
                            break;
                        }
                    }
                });
            }
            drop(tx);

            let mut pending = BTreeMap::new();
            let mut want = 0;
            for (i, record) in rx.iter() {
                pending.insert(i, record);
                while let Some(record) = pending.remove(&want) {
                    want += 1;
                    stats.files += 1;
                    match &record {
                        FileRecord::Error(_) => stats.errors += 1,
                        FileRecord::Ok(r) if r.cache == CacheStatus::Hit => stats.cache_hits += 1,
                        FileRecord::Ok(_) => {}
                    }
                    if let Err(e) = sink(record) {
                        // Workers finish their current file and take no new one.
                        next.store(total, Ordering::Relaxed);
                        return Err(e);
                    }
                }
            }
            Ok(())
        })?;
        Ok(stats)
    }

    fn run(
        &self,
        path: &Path,
        options: &BatchOptions,
        stages: &mut Vec<StageStats>,
    ) -> Result<FileRecord> {
        let display = path.display().to_string();
        let fs_path = match &options.base {
            Some(base) => base.join(path),
            None => path.to_path_buf(),
        };

        let t = Instant::now();
        let bytes = read_limited(&fs_path, self.config.max_file_bytes)
            .map_err(|e| walk::respell_error(e, &fs_path, path))?;
        let hash = content_hash(&bytes);
        stages.push(StageStats::timed(Stage::Read, t.elapsed()));

        let format = detect_format(path, &bytes);
        let converter = self.converters.get(&format)?;

        let t = Instant::now();
        let key = Cache::key(&hash, converter.name(), &self.settings);
        if let Some(cache) = &self.cache
            && let Some(hit) = cache.get(&key)
            // An empty summary cached by an earlier build is never served.
            && !hit.summary.trim().is_empty()
        {
            stages.push(StageStats::timed(Stage::Cache, t.elapsed()));
            let stages = std::mem::take(stages);
            return Ok(FileRecord::Ok(hit.into_record(
                display,
                CacheStatus::Hit,
                stages,
            )));
        }
        let lookup = t.elapsed();

        let t = Instant::now();
        let input = ConvertInput {
            path,
            bytes: &bytes,
            format: &format,
        };
        // The stage is recorded on failure too, so an error record shows the
        // time a crashed or timed-out worker took.
        let converted = convert_cached(converter.as_ref(), &input, self.cache.as_ref());
        stages.push(StageStats::timed(Stage::Convert, t.elapsed()));
        let text = converted?;

        let t = Instant::now();
        let sections = segment(
            &text,
            SegmentOptions {
                markdown: format == MARKDOWN,
                max_section_lines: self.config.outline.max_section_lines,
                hard_bound: self.config.outline.describe,
            },
        );
        // B4: a converted document (PDF, PPTX, ...) has no headings; its
        // titles skip footers, copyright lines and bare values.
        let mut sections = sections;
        if ![MARKDOWN, TEXT, CODE].contains(&format.as_str()) {
            let index = LineIndex::new(&text);
            for s in &mut sections {
                s.title = meaningful_title(&index, s.lines);
            }
        }
        let terms = if self.config.outline.describe {
            section_terms(
                &text,
                &sections,
                TermLimits {
                    max_terms: self.config.outline.max_terms,
                    max_chars: self.config.outline.max_term_chars,
                },
            )
        } else {
            vec![Vec::new(); sections.len()]
        };
        let outline = sections
            .iter()
            .zip(terms)
            .map(|(s, terms)| OutlineEntry {
                terms,
                title: s.title.clone(),
                lines: self.config.references.then_some(s.lines),
                paragraphs: if self.config.references {
                    s.paragraphs
                } else {
                    None
                },
            })
            .collect();
        let lines = LineIndex::new(&text).line_count();
        let text_chars = text.chars().count();
        stages.push(StageStats::timed(Stage::Segment, t.elapsed()));

        let excerpt = bounded_excerpt(&text, &sections, self.config.summary.max_input_chars);
        let sent = excerpt.chars().count();

        let class = match &self.classifier {
            None => ClassResult::NoClassifierConfigured,
            Some(classifier) => {
                let t = Instant::now();
                let response = classifier
                    .classify_with(&[excerpt.as_str()], &self.labels, &options.credentials)
                    .inspect_err(|e| {
                        // A failed call the provider still bills shows in the record.
                        if let Some(u) = e.billed_usage() {
                            stages.push(
                                StageStats::with_usage(Stage::Classify, t.elapsed(), u).sent(sent),
                            );
                        }
                    })?;
                stages.push(
                    StageStats::with_usage(Stage::Classify, t.elapsed(), response.usage).sent(sent),
                );
                best_label(
                    &self.config.classifier.backend,
                    &response.scores,
                    &self.labels,
                )?
            }
        };

        let t = Instant::now();
        let response = self.inference.complete(&InferenceRequest {
            instruction: self.config.summary.instruction.clone(),
            input: excerpt,
            max_output_tokens: self.config.summary.max_output_tokens,
            credentials: options.credentials.clone(),
        });
        let response = response.inspect_err(|e| {
            // A failed call the provider still bills shows in the record.
            if let Some(u) = e.billed_usage() {
                stages.push(StageStats::with_usage(Stage::Summarize, t.elapsed(), u).sent(sent));
            }
        })?;
        stages
            .push(StageStats::with_usage(Stage::Summarize, t.elapsed(), response.usage).sent(sent));
        let summary = response.text.trim().to_string();
        if summary.is_empty() {
            // No mode may record or cache an empty summary as a success.
            return Err(Error::EmptySummary {
                backend: self.config.summary.backend.clone(),
            });
        }

        let result = CachedResult {
            hash,
            bytes: bytes.len() as u64,
            lines,
            text_chars,
            tokens_est: estimate_tokens(text_chars),
            format,
            class,
            summary,
            outline,
            backends: BackendsUsed {
                converter: converter.name().to_string(),
                summary: self.config.summary.backend.clone(),
                classifier: self.config.classifier.backend.clone(),
            },
        };

        let status = match &self.cache {
            None => CacheStatus::Disabled,
            Some(cache) => {
                let t = Instant::now();
                if let Err(e) = cache.put(&key, &result) {
                    eprintln!("trusty-summarizer: warning: cache write failed: {e}");
                }
                stages.push(StageStats::timed(Stage::Cache, lookup + t.elapsed()));
                CacheStatus::Miss
            }
        };
        let stages = std::mem::take(stages);
        Ok(FileRecord::Ok(result.into_record(display, status, stages)))
    }
}

fn error_record(path: &Path, e: &Error, stages: Vec<StageStats>) -> FileRecord {
    FileRecord::Error(ErrorRecord {
        path: path.display().to_string(),
        error: ErrorInfo::from(e),
        stages,
    })
}

/// Picks the best label and the runner-up, rejecting a response of the
/// wrong shape or with a non-finite score.
fn best_label(backend: &str, scores: &[Vec<f32>], labels: &[Label]) -> Result<ClassResult> {
    let bad = |m: String| Error::Backend {
        backend: backend.to_string(),
        message: m,
        usage: None,
    };
    let [row] = scores else {
        return Err(bad(format!("expected 1 score row, got {}", scores.len())));
    };
    if row.len() != labels.len() {
        return Err(bad(format!(
            "expected {} scores, got {}",
            labels.len(),
            row.len()
        )));
    }
    if row.iter().any(|s| !s.is_finite()) {
        return Err(bad("non-finite score".into()));
    }
    let mut order: Vec<usize> = (0..row.len()).collect();
    // Highest score first; ties go to the earlier label.
    order.sort_by(|&a, &b| row[b].total_cmp(&row[a]).then(a.cmp(&b)));
    let Some(&i) = order.first() else {
        return Err(bad("no scores".into()));
    };
    Ok(ClassResult::Classified {
        label: labels[i].name.clone(),
        score: row[i],
        runner_up: order.get(1).map(|&j| LabelScore {
            label: labels[j].name.clone(),
            score: row[j],
        }),
    })
}

/// Canonical JSON of every setting that changes a result; part of the
/// cache key. `serde_json` maps are sorted, so the text is stable.
fn settings_fingerprint(config: &Config) -> Result<String> {
    let summary = &config.summary.backend;
    let classifier = &config.classifier.backend;
    let (classifier_options, labeler_options) = if classifier == NO_CLASSIFIER {
        (toml::Table::new(), toml::Table::new())
    } else {
        let own = config.backend_options(classifier);
        let inner = match type_of(&own) {
            Some(LLM_TYPE) => llm_inference_name(classifier, &own)
                .map(|n| config.backend_options(n))
                .unwrap_or_default(),
            _ => toml::Table::new(),
        };
        (own, inner)
    };
    let value = serde_json::json!({
        "schema": CACHE_SCHEMA,
        "version": env!("CARGO_PKG_VERSION"),
        "references": config.references,
        "outline": config.outline,
        "summary": config.summary,
        "summary_options": config.backend_options(summary),
        "classifier": config.classifier.backend,
        "labels": if classifier == NO_CLASSIFIER { Vec::new() } else { config.labels() },
        "classifier_options": classifier_options,
        "labeler_inference_options": labeler_options,
    });
    serde_json::to_string(&value).map_err(|e| Error::Config(format!("settings fingerprint: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn best_label_checks_shape() {
        let labels: Vec<Label> = ["a", "b", "c"]
            .iter()
            .map(|n| Label {
                name: (*n).into(),
                description: String::new(),
            })
            .collect();
        let got = best_label("t", &[vec![0.1, 0.9, 0.5]], &labels).unwrap();
        assert_eq!(
            got,
            ClassResult::Classified {
                label: "b".into(),
                score: 0.9,
                runner_up: Some(LabelScore {
                    label: "c".into(),
                    score: 0.5
                }),
            }
        );
        assert!(best_label("t", &[vec![0.1, f32::NAN, 0.2]], &labels).is_err());
        assert!(best_label("t", &[vec![0.1]], &labels).is_err());
        assert!(best_label("t", &[], &labels).is_err());
    }
}
