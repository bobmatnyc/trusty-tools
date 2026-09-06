//! The one model-pricing table for the workspace (#6875).
//!
//! Why: three crates priced the same Anthropic models from three independent
//! tables. `trusty-agents` still charged Sonnet at 4.x-generation rates
//! ($3/$15 per MTok) and defaulted every unrecognised id to those same rates,
//! so a Sonnet 5 turn was billed 50% high and an unknown model was billed as if
//! it were Sonnet; `trusty-mpm`'s SM table priced Opus at $15/$75 and Haiku at
//! $0.80/$4. CLAUDE.md's "Common entry point, clean domain demarcation" gives a
//! cross-crate capability exactly one implementation, and #6872's cost ledger
//! would have added a fourth table. This module is that one implementation.
//!
//! What: [`Pricing`] loads rows from `crates/trusty-common/pricing.toml`, which
//! is embedded with `include_str!` so the crate has no runtime file dependency.
//! [`Pricing::rate_for`] resolves a model id to [`Rates`] for a given day,
//! picking the row with the latest `effective_from` at or before that day;
//! [`Rates::cost_usd`] turns a [`Usage`] into dollars. An operator replaces rows
//! by writing the same schema to [`default_override_path`]
//! (`~/.trusty-tools/pricing.toml`); [`shared`] is the process-wide instance
//! every consumer reads, bundled table plus that override.
//!
//! **An unknown model resolves to `None`, never to a fallback rate.** The
//! stale-default arm this replaces meant an unpriced model produced a
//! confident, wrong number. Callers turn `None` into `0.0` and report it
//! through [`warn_unknown_model_once`], so the gap is visible in the log rather
//! than absorbed into a total.
//!
//! Test: `pricing_tests.rs` — published-rate rows, `effective_from` selection,
//! alias and Bedrock-id resolution, operator override, and the unknown-model
//! contract.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use chrono::NaiveDate;
use serde::Deserialize;

/// The bundled table, embedded at compile time.
///
/// Why: a daemon must price a turn without reading a file that may not exist.
/// What: the verbatim text of `crates/trusty-common/pricing.toml`.
/// Test: `bundled_table_parses`.
pub const BUNDLED_PRICING_TOML: &str = include_str!("../pricing.toml");

/// Filename an operator drops under `~/.trusty-tools/` to override rows.
///
/// Why: naming the file once keeps the docs and the resolver in agreement.
/// What: `"pricing.toml"`.
/// Test: `default_override_path_layout`.
pub const OVERRIDE_FILE: &str = "pricing.toml";

/// Directory under `$HOME` holding the trusty-* operator configuration tree.
///
/// Why: this module is unconditional and `crate_config` is feature-gated, so
/// the constant lives at the crate root and both re-export it rather than
/// declaring a second copy of the literal.
/// What: re-export of [`crate::TRUSTY_TOOLS_DIR`].
/// Test: `default_override_path_layout`.
pub use crate::TRUSTY_TOOLS_DIR;

/// Token counts for one priced call.
///
/// Why: the four buckets bill at four different rates, and summing them before
/// pricing is the class of bug that made a cached turn look like a fresh one.
/// What: raw counts; `u64` because aggregate callers sum a whole corpus.
/// Test: `rates_cost_usd_prices_each_bucket_separately`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Usage {
    /// Fresh (uncached) input tokens.
    pub input: u64,
    /// Generated output tokens.
    pub output: u64,
    /// Tokens written into the prompt cache.
    pub cache_creation: u64,
    /// Tokens served from the prompt cache.
    pub cache_read: u64,
}

impl Usage {
    /// Build a usage record from the four buckets.
    pub fn new(input: u64, output: u64, cache_creation: u64, cache_read: u64) -> Self {
        Self {
            input,
            output,
            cache_creation,
            cache_read,
        }
    }
}

/// USD per 1,000,000 tokens for one model on one day.
///
/// Why: every consumer needs the same four numbers; handing back the rates
/// rather than a dollar figure lets a caller that only bills input and output
/// (the SM providers) read them directly.
/// What: the four per-MTok rates from the resolved row.
/// Test: `published_anthropic_rates_match_the_claude_api_skill`.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct Rates {
    /// Fresh input, USD per MTok.
    pub input: f64,
    /// Output, USD per MTok.
    pub output: f64,
    /// 5-minute-TTL cache write, USD per MTok. `0.0` records "no published rate".
    pub cache_write: f64,
    /// Cache read, USD per MTok. `0.0` records "no published rate".
    pub cache_read: f64,
}

impl Rates {
    /// Price a usage record.
    ///
    /// Why: each bucket must be multiplied by its OWN rate — pricing summed
    /// tokens once is what made cached turns cost as much as fresh ones.
    /// What: sums `bucket * rate / 1e6` over the four buckets.
    /// Test: `rates_cost_usd_prices_each_bucket_separately`.
    pub fn cost_usd(&self, usage: &Usage) -> f64 {
        let per = |tokens: u64, rate: f64| tokens as f64 * rate / 1_000_000.0;
        per(usage.input, self.input)
            + per(usage.output, self.output)
            + per(usage.cache_creation, self.cache_write)
            + per(usage.cache_read, self.cache_read)
    }
}

/// One row of the pricing table.
///
/// Why: `effective_from` is what lets a ledger price yesterday's usage at
/// yesterday's rate after a vendor changes a price.
/// What: the deserialised `[[model]]` entry. `cache_write`/`cache_read` default
/// to `0.0`, which records that no published cache rate exists for the id.
/// Test: `bundled_table_parses`.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct ModelRow {
    /// Canonical model id.
    pub id: String,
    /// Additional ids resolving to this row (Bedrock ids, punctuation variants).
    #[serde(default)]
    pub aliases: Vec<String>,
    /// `anthropic` | `bedrock` | `openrouter`, when known.
    #[serde(default)]
    pub provider: Option<String>,
    /// Provenance; `legacy-table` marks a rate carried over verbatim.
    #[serde(default)]
    pub source: Option<String>,
    /// First day this rate applies.
    pub effective_from: NaiveDate,
    /// Fresh input, USD per MTok.
    pub input: f64,
    /// Output, USD per MTok.
    pub output: f64,
    /// Cache write, USD per MTok.
    #[serde(default)]
    pub cache_write: f64,
    /// Cache read, USD per MTok.
    #[serde(default)]
    pub cache_read: f64,
}

impl ModelRow {
    fn rates(&self) -> Rates {
        Rates {
            input: self.input,
            output: self.output,
            cache_write: self.cache_write,
            cache_read: self.cache_read,
        }
    }

    /// Every lookup key this row claims, normalised and de-duplicated.
    fn keys(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        std::iter::once(&self.id)
            .chain(self.aliases.iter())
            .map(|k| normalize(k))
            .filter(|k| !k.is_empty() && seen.insert(k.clone()))
            .collect()
    }
}

#[derive(Debug, Deserialize)]
struct PricingFile {
    #[serde(default)]
    model: Vec<ModelRow>,
}

/// Loading or parsing a pricing table failed.
///
/// Why: an operator override that does not parse must be reported, not
/// silently ignored — a swallowed parse error leaves the operator believing a
/// rate they wrote is in force.
/// What: hand-written rather than `thiserror`-derived because this module is
/// unconditional and `thiserror` is an optional dependency of this crate.
/// Test: `override_with_malformed_toml_is_an_error`.
#[derive(Debug)]
#[non_exhaustive]
pub enum PricingError {
    /// The override file could not be read.
    Read {
        /// Path the read targeted.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// The TOML could not be parsed into rows.
    Parse {
        /// Human-readable origin of the text (a path, or `<bundled>`).
        origin: String,
        /// The `toml` crate's message.
        message: String,
    },
}

impl std::fmt::Display for PricingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read { path, source } => {
                write!(f, "pricing table I/O error at {}: {source}", path.display())
            }
            Self::Parse { origin, message } => {
                write!(f, "pricing table parse error in {origin}: {message}")
            }
        }
    }
}

impl std::error::Error for PricingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::Parse { .. } => None,
        }
    }
}

/// A resolved pricing table: normalised lookup key → rows for that key.
///
/// Why: consumers ask "what does this model cost", not "which row wins" — the
/// alias fan-out and the `effective_from` choice belong here, once.
/// What: rows are indexed under every key they claim, so an alias lookup is the
/// same map hit as a canonical-id lookup.
/// Test: `alias_resolves_a_bedrock_id`, `effective_from_picks_the_dated_row`.
#[derive(Debug, Clone, Default)]
pub struct Pricing {
    by_key: HashMap<String, Vec<ModelRow>>,
}

impl Pricing {
    /// The table embedded in this binary.
    ///
    /// Why: the common case — no operator override, no filesystem access.
    /// What: parses [`BUNDLED_PRICING_TOML`]. A parse failure here is a build
    /// defect, not a runtime condition, so it panics rather than propagating:
    /// `bundled_table_parses` fails first in CI.
    /// Test: `bundled_table_parses`.
    pub fn bundled() -> Self {
        Self::from_toml_str(BUNDLED_PRICING_TOML, "<bundled>")
            .expect("bundled pricing.toml must parse; see pricing_tests::bundled_table_parses")
    }

    /// Parse a table from TOML text.
    ///
    /// Why: the override reader and the tests both need this without a file.
    /// What: deserialises `[[model]]` rows and indexes them by every key they
    /// claim. `origin` only labels errors.
    /// Test: `bundled_table_parses`, `override_with_malformed_toml_is_an_error`.
    pub fn from_toml_str(text: &str, origin: &str) -> Result<Self, PricingError> {
        let parsed: PricingFile = toml::from_str(text).map_err(|e| PricingError::Parse {
            origin: origin.to_string(),
            message: e.to_string(),
        })?;
        let mut table = Self::default();
        table.insert_rows(parsed.model);
        Ok(table)
    }

    /// The bundled table with an operator file layered over it.
    ///
    /// Why: an operator must be able to correct a rate without a release.
    /// What: every key the override file claims is REPLACED wholesale — the
    /// override owns that id's whole rate history, so a partial edit cannot
    /// leave a bundled row shadowing the operator's. Keys the file does not
    /// mention keep their bundled rows. A missing file is not an error.
    /// Test: `override_replaces_one_row_and_leaves_the_rest`.
    pub fn with_override(path: &Path) -> Result<Self, PricingError> {
        let mut table = Self::bundled();
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(table),
            Err(source) => {
                return Err(PricingError::Read {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        let parsed: PricingFile = toml::from_str(&text).map_err(|e| PricingError::Parse {
            origin: path.display().to_string(),
            message: e.to_string(),
        })?;
        for row in &parsed.model {
            for key in row.keys() {
                table.by_key.remove(&key);
            }
        }
        table.insert_rows(parsed.model);
        Ok(table)
    }

    fn insert_rows(&mut self, rows: Vec<ModelRow>) {
        for row in rows {
            for key in row.keys() {
                self.by_key.entry(key).or_default().push(row.clone());
            }
        }
    }

    /// Rates for `model_id` as of `on`, or `None` when the model is not priced.
    ///
    /// Why: an unpriced model must be visible as unpriced. The table this
    /// replaced answered every unknown id with Sonnet rates, which is a
    /// confident wrong number rather than a gap.
    /// What: normalises the id (lower-cases, strips `<vendor>/` routing
    /// prefixes and `us.anthropic.`-style Bedrock prefixes), tries an exact key
    /// hit, then the LONGEST key that is a `-`/`.`/`:`-delimited prefix — so a
    /// dated or versioned snapshot (`claude-sonnet-4-5-20250929`,
    /// `claude-sonnet-4-5-v1:0`) prices as its base model. Among the matched
    /// rows it takes the latest `effective_from` at or before `on`.
    /// Test: `alias_resolves_a_bedrock_id`, `effective_from_picks_the_dated_row`,
    /// `unknown_model_is_none`.
    pub fn rate_for(&self, model_id: &str, on: NaiveDate) -> Option<Rates> {
        let key = normalize(model_id);
        if key.is_empty() {
            return None;
        }
        let rows = match self.by_key.get(&key) {
            Some(rows) => rows,
            None => self.by_key.get(&self.longest_prefix_key(&key)?)?,
        };
        rows.iter()
            .filter(|r| r.effective_from <= on)
            .max_by_key(|r| r.effective_from)
            .map(ModelRow::rates)
    }

    /// Rates for `model_id` as of today (UTC).
    ///
    /// Why: the live cost surfaces price the turn happening now; only the #6872
    /// ledger backfill needs an explicit date.
    /// What: [`Pricing::rate_for`] with `chrono::Utc::now().date_naive()`.
    /// Test: `published_anthropic_rates_match_the_claude_api_skill`.
    pub fn rate_today(&self, model_id: &str) -> Option<Rates> {
        self.rate_for(model_id, chrono::Utc::now().date_naive())
    }

    fn longest_prefix_key(&self, key: &str) -> Option<String> {
        self.by_key
            .keys()
            .filter(|k| {
                key.len() > k.len()
                    && key.starts_with(k.as_str())
                    && matches!(key.as_bytes()[k.len()], b'-' | b'.' | b':')
            })
            .max_by_key(|k| k.len())
            .cloned()
    }
}

/// Canonical form of a model id for lookup.
///
/// Why: the same model arrives as `claude-sonnet-4-6` (direct),
/// `anthropic/claude-sonnet-4-6` (OpenRouter),
/// `us.anthropic.claude-sonnet-4-6` (Bedrock) and
/// `bedrock/us.anthropic.claude-sonnet-4-6` (SM routing). Normalising once
/// means a row needs one alias per genuinely different id, not one per route.
/// What: lower-cases, drops every `<segment>/` routing prefix, then drops
/// leading dotted segments while what follows is an `anthropic.`/`claude…`
/// id — which leaves `gpt-5.4-mini-…` untouched, since its dot is a version.
/// Test: `normalize_strips_routing_and_region_prefixes`.
fn normalize(id: &str) -> String {
    let lowered = id.trim().to_ascii_lowercase();
    let mut s = lowered.as_str();
    while let Some((_, rest)) = s.split_once('/') {
        s = rest;
    }
    loop {
        match s.split_once('.') {
            Some((head, rest))
                if head == "anthropic"
                    || rest.starts_with("anthropic.")
                    || rest.starts_with("claude") =>
            {
                s = rest;
            }
            _ => break,
        }
    }
    s.to_string()
}

/// The process-wide table: bundled rows plus the operator override.
///
/// Why: every cost surface must read the SAME numbers, and re-parsing the TOML
/// per priced turn would put a parse in the hot path of a per-message ledger.
/// What: a `OnceLock` memo of an immutable value — the table is never mutated
/// after construction, so this is a cached constant rather than shared state.
/// An unreadable or malformed override is logged and the bundled table stands;
/// refusing to price anything because one operator file is broken would be
/// worse than pricing from the shipped rates.
/// Test: `shared_matches_bundled_for_a_published_model`.
pub fn shared() -> &'static Pricing {
    static SHARED: OnceLock<Pricing> = OnceLock::new();
    SHARED.get_or_init(|| match default_override_path() {
        Some(path) => Pricing::with_override(&path).unwrap_or_else(|e| {
            tracing::warn!("pricing override ignored: {e}");
            Pricing::bundled()
        }),
        None => Pricing::bundled(),
    })
}

/// `~/.trusty-tools/pricing.toml`, the operator override location.
///
/// Why: the `~/.trusty-tools/` tree is where every trusty-* crate already reads
/// operator configuration; pricing is workspace-wide rather than per-crate, so
/// it sits at the root of that tree rather than under a crate directory.
/// What: `None` only when the home directory cannot be resolved.
/// Test: `default_override_path_layout`.
pub fn default_override_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(TRUSTY_TOOLS_DIR).join(OVERRIDE_FILE))
}

/// Report an unpriced model once per process.
///
/// Why: [`Pricing::rate_for`] returns `None` so a caller cannot silently bill
/// zero — but a warning per token bucket, per turn, would bury the log. Once
/// per distinct id is enough to tell an operator a row is missing.
/// What: a de-duplicating set of ids already warned about. Global, and
/// deliberately so: "once per process" cannot be expressed per call site.
/// A poisoned lock degrades to warning every time rather than going silent.
/// Test: `warn_unknown_model_once_dedupes`.
pub fn warn_unknown_model_once(model: &str) {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let first = match SEEN.get_or_init(|| Mutex::new(HashSet::new())).lock() {
        Ok(mut seen) => seen.insert(model.to_string()),
        Err(_) => true,
    };
    if first {
        tracing::warn!(
            model = %model,
            "no pricing row for this model; its cost is reported as $0.00. \
             Add a row to crates/trusty-common/pricing.toml or to \
             ~/.trusty-tools/pricing.toml (#6875)."
        );
    }
}

#[cfg(test)]
#[path = "pricing_tests.rs"]
mod tests;
