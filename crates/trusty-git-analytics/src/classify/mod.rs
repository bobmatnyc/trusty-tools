//! Stage 2 of the pipeline: classify each collected commit using a four-tier
//! cascade.
//!
//! ## Tiers
//!
//! 1. **Exact** — Aho-Corasick multi-keyword match (case-insensitive).
//! 2. **Regex** — pre-compiled regex patterns.
//! 3. **Fuzzy** — structural heuristics (merge/revert/ticket-prefix).
//! 4. **LLM** — optional async fallback via an OpenAI-compatible API.
//!
//! Tiers 1–3 are synchronous and run in parallel across commits via Rayon.
//! Tier 4 is async and serialized.

pub mod classifier;
pub mod errors;
pub mod pipeline;
pub(super) mod pipeline_db;
pub mod rules;
pub mod sources;
pub mod taxonomy;
pub mod tiers;

pub use classifier::{ClassificationEngine, ClassificationEngineConfig};
pub use errors::{ClassifyError, Result};
pub use pipeline::{ClassificationPipeline, ClassificationStats};
pub use rules::{Rule, RuleSet};
pub use taxonomy::{SubcategoryDef, TaxonomyRegistry, TopLevelCategory};
pub use tiers::ClassificationResult;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod pipeline_tests;
