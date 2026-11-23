//! trusty-summarizer: fast, low-inference summaries and classes for files.
//!
//! Why: sort and review many files before spending expensive inference on
//! them (see `docs/plans/2026-11-14-trusty-summarizer-plan.md`).
//!
//! What: [`Summarizer`] turns each file into a [`FileRecord`]: content hash,
//! sizes, class, summary, an outline with line and paragraph ranges, the
//! backends used and per-stage time, tokens and cost. Conversion goes
//! through [`Converters`]; summary and classification go through the
//! [`InferenceBackend`] and [`ClassifierBackend`] traits, chosen by name
//! from [`Config`] via [`BackendRegistry`]. Results are cached by content
//! hash plus output settings.
//!
//! ```no_run
//! use trusty_summarizer::{BackendRegistry, Config, Converters, Summarizer};
//!
//! let summarizer = Summarizer::new(
//!     Config::default(),
//!     &BackendRegistry::with_builtins(),
//!     Converters::with_builtins(),
//! )?;
//! summarizer.summarize_batch(&["docs".into()], |record| {
//!     println!("{}", serde_json::to_string(&record).expect("serializable"));
//!     Ok(())
//! })?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod backend;
pub mod cache;
pub mod config;
pub mod convert;
pub mod embed;
pub mod error;
pub mod excerpt;
pub mod extractive;
pub mod find;
pub mod labeler;
pub mod logfile;
pub mod openai;
pub mod output;
pub mod pipeline;
pub mod record;
pub mod segment;
pub mod server;
pub mod terms;
pub mod text;
pub mod trial;
pub mod usage;
pub mod walk;
pub mod worker;

pub use backend::{
    BackendRegistry, ClassifierBackend, ClassifyResponse, Cost, Credentials, InferenceBackend,
    InferenceRequest, InferenceResponse, KeySource, Usage,
};
pub use config::{Config, ConvertConfig, FindConfig, Label, OutlineConfig, default_labels};
pub use convert::{ConvertInput, Converter, Converters, DirectReader};
pub use embed::{Embedder, EmbeddingClassifier, LocalEmbedder, ModelSpec};
pub use error::{Error, Result, WorkerFailure};
pub use labeler::LlmClassifier;
pub use openai::OpenAiBackend;
pub use pipeline::{BatchOptions, BatchStats, Summarizer};
pub use record::{CacheStatus, ClassResult, FileRecord, LabelScore, SummaryRecord};
pub use text::{LineIndex, Span};
pub use worker::WorkerConverter;
