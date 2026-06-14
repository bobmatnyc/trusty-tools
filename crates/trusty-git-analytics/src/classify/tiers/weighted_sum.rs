//! Tier 2.5: weighted-sum classifier.
//!
//! Why: tier 2 (regex) and tier 3 (fuzzy heuristics) leave a gap for commits
//! whose messages carry multiple weak signals — none strong enough to trigger a
//! rule on their own, but whose combination tips the balance toward one category.
//! A simple linear weighted sum over five cheap signals produces a calibrated
//! per-category score without requiring fuzzy-logic libraries (the Rust
//! fuzzy-logic ecosystem is pre-production, with the leading candidate at ~544
//! downloads and no inference engine as of 2026-05; research ref issue #270).
//!
//! Sits between the regex tier (Tier 2) and the fuzzy tier (Tier 3). Emits a
//! verdict when the argmax score reaches `min_confidence` (default 0.55).
//! Confidence is clamped to `[min_confidence, 0.95]` to leave room for the
//! rule-based tiers that should still beat this one when they fire.

use crate::classify::taxonomy::TopLevelCategory;
use crate::classify::tiers::ClassificationResult;
use crate::core::models::ClassificationMethod;

// ─── signal identifiers ──────────────────────────────────────────────────────
// Five signals are computed per commit:
//   0. Keyword      — stepped density score per category keyword bag
//   1. TicketPrefix — uniform +0.05 nudge when JIRA-style prefix detected
//   2. MessageLength — dynamic per-call (short/medium/long buckets)
//   3. MergeIndicator — MERGE_WEIGHTS table; strong positive for Merge
//   4. FilePaths     — dynamic per-call (tests/, docs/, manifests)

// ─── category indices ─────────────────────────────────────────────────────────

/// Internal category indices used to index into the weight table.
///
/// Why: a small fixed enum avoids per-call string-key lookups and keeps the
/// weight table a simple 2D array.
/// What: maps the seven [`TopLevelCategory`] variants to contiguous indices.
/// Test: covered indirectly by every weighted-sum test that asserts a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cat {
    Feature = 0,
    Bugfix = 1,
    Ktlo = 2,
    Integrations = 3,
    PlatformWork = 4,
    Content = 5,
    Maintenance = 6,
    Merge = 7,
}

pub(crate) const NUM_CATS: usize = 8;

impl Cat {
    const ALL: [Cat; NUM_CATS] = [
        Cat::Feature,
        Cat::Bugfix,
        Cat::Ktlo,
        Cat::Integrations,
        Cat::PlatformWork,
        Cat::Content,
        Cat::Maintenance,
        Cat::Merge,
    ];

    pub(crate) fn index(self) -> usize {
        self as usize
    }

    /// Convert back to a (`category` string, `TopLevelCategory`) pair.
    ///
    /// Why: the cascade returns `ClassificationResult` whose `category` field
    /// is a subcategory string (e.g. `"feature"`, `"bugfix"`) not an enum;
    /// this converts the internal enum back to the public API shape.
    /// What: returns the canonical subcategory name and its top-level parent.
    /// Test: covered by every test that asserts the returned category string.
    fn to_verdict(self) -> (&'static str, TopLevelCategory) {
        match self {
            Cat::Feature => ("feature", TopLevelCategory::Feature),
            Cat::Bugfix => ("bugfix", TopLevelCategory::Bugfix),
            Cat::Ktlo => ("chore", TopLevelCategory::Ktlo),
            Cat::Integrations => ("integration", TopLevelCategory::Integrations),
            Cat::PlatformWork => ("platform", TopLevelCategory::PlatformWork),
            Cat::Content => ("docs", TopLevelCategory::Content),
            Cat::Maintenance => ("refactor", TopLevelCategory::Maintenance),
            Cat::Merge => ("merge", TopLevelCategory::Maintenance),
        }
    }
}

// ─── weight table ─────────────────────────────────────────────────────────────

/// Static weight for the MergeIndicator signal, per category.
///
/// Why: the merge indicator is computed with a static contribution per
/// category; keeping it in a named array makes the weight rationale auditable.
/// What: W[cat.index()] = f32 contribution when a merge commit is detected.
///   - Merge: +0.65 — strong positive; a merge commit is very likely to be
///     labelled "merge".
///   - All others: −0.05 — mild negative; a merge commit is unlikely to be
///     a genuine bugfix or feature.
///
/// **Design note (1.3.0)**: the keyword signal uses a stepped scalar (0.40 /
/// 0.60 / 0.75) applied directly to the per-category accumulator. Only the
/// MergeIndicator and TicketPrefix signals use static tables because they are
/// the only ones whose per-category values differ from a simple +/- uniform
/// shift. MessageLength and FilePaths are computed dynamically in their
/// scorer functions.
static MERGE_WEIGHTS: &[f32; NUM_CATS] = &[
    // [Feature, Bugfix, Ktlo, Integrations, PlatformWork, Content, Maintenance, Merge]
    -0.05, -0.05, -0.05, -0.05, -0.05, -0.05, -0.05, 0.65,
];

// Keyword bags per category (used only for the Keyword signal).
// Each slice contains the keywords to check for that category.
// Score = (matched keywords) / (len of bag).

const FEATURE_KEYWORDS: &[&str] = &[
    "add",
    "implement",
    "feature",
    "support",
    "introduce",
    "create",
    "build",
    "new",
    "extend",
    "enable",
];

const BUGFIX_KEYWORDS: &[&str] = &[
    "fix",
    "bug",
    "issue",
    "broken",
    "regression",
    "hotfix",
    "patch",
    "resolve",
    "repair",
    "correct",
];

const KTLO_KEYWORDS: &[&str] = &[
    "chore", "ci", "build", "ops", "release", "version", "bump", "update", "upgrade", "automate",
];

const INTEGRATION_KEYWORDS: &[&str] = &[
    "integrate",
    "api",
    "webhook",
    "sdk",
    "plugin",
    "connector",
    "bridge",
    "endpoint",
    "external",
    "third-party",
];

const PLATFORM_KEYWORDS: &[&str] = &[
    "perf",
    "performance",
    "infra",
    "infrastructure",
    "architecture",
    "devops",
    "deploy",
    "scale",
    "optimize",
    "database",
];

const CONTENT_KEYWORDS: &[&str] = &[
    "docs",
    "readme",
    "documentation",
    "comment",
    "typo",
    "copy",
    "translation",
    "locale",
    "i18n",
    "asset",
];

const MAINTENANCE_KEYWORDS: &[&str] = &[
    "refactor",
    "cleanup",
    "rename",
    "deps",
    "dependency",
    "style",
    "lint",
    "format",
    "test",
    "remove",
];

/// Category-indexed keyword bags.
static KEYWORD_BAGS: &[&[&str]] = &[
    FEATURE_KEYWORDS,     // Cat::Feature
    BUGFIX_KEYWORDS,      // Cat::Bugfix
    KTLO_KEYWORDS,        // Cat::Ktlo
    INTEGRATION_KEYWORDS, // Cat::Integrations
    PLATFORM_KEYWORDS,    // Cat::PlatformWork
    CONTENT_KEYWORDS,     // Cat::Content
    MAINTENANCE_KEYWORDS, // Cat::Maintenance
    &[],                  // Cat::Merge — no keyword bag; driven by MergeIndicator
];

// ─── scorer helpers ───────────────────────────────────────────────────────────

/// Score the keyword signal for a commit message.
///
/// Why: the keyword signal is the dominant signal; checking a fixed bag of
/// ~10 words per category is O(message_len × total_keywords) but fast in
/// practice because messages are short.
/// What: returns per-category scores in [0.0, 0.75] using a stepped approach:
///   - 0 keyword matches → 0.0
///   - 1 match           → 0.40 (one strong keyword is meaningful signal)
///   - 2 matches         → 0.60
///   - 3+ matches        → 0.75
///
/// Stepped scoring instead of linear `matched/bag_len` is intentional: even
/// a single strong keyword (e.g. "fix" for bugfix) should produce a score
/// that exceeds `min_confidence` (0.55) when combined with even a small
/// contribution from other signals.
/// Test: covered by `keyword_score_*` unit tests.
pub(crate) fn score_keywords(lower: &str) -> [f32; NUM_CATS] {
    let mut out = [0.0f32; NUM_CATS];
    for cat in Cat::ALL {
        let bag = KEYWORD_BAGS[cat.index()];
        if bag.is_empty() {
            continue;
        }
        let matched = bag.iter().filter(|&&kw| lower.contains(kw)).count();
        let score = match matched {
            0 => 0.0,
            1 => 0.40,
            2 => 0.60,
            _ => 0.75,
        };
        out[cat.index()] = score;
    }
    out
}

/// Score the ticket-prefix signal.
///
/// Why: a PROJ-123 prefix is a weak universal confidence nudge; it does not
/// discriminate between categories, it just raises the floor so the argmax
/// has more room.
/// What: returns a flat array where every category gets +0.05 if a ticket
/// prefix is detected, 0.0 otherwise. The small uniform boost means that
/// any single keyword match (0.40 base score) + ticket prefix reaches 0.45,
/// leaving the argmax well below 0.55 until at least two signals fire.
/// Test: covered by `ticket_prefix_signal_*` unit tests.
pub(crate) fn score_ticket_prefix(message: &str) -> [f32; NUM_CATS] {
    // +0.05 uniform nudge when a JIRA-style ticket prefix is present.
    const TICKET_WEIGHT: f32 = 0.05;
    if has_jira_style_prefix(message) {
        [TICKET_WEIGHT; NUM_CATS]
    } else {
        [0.0; NUM_CATS]
    }
}

/// Score the message-length signal.
///
/// Why: message length is a soft proxy for commit complexity. Very short
/// messages (<12 chars) are often chores or merges; very long ones (>80 chars)
/// tend to be substantive features or refactors.
/// What: returns per-category adjustments keyed to three length buckets:
///   - Short (<12 chars): +0.10 for KTLO/Merge/Maintenance, −0.05 for Feature
///   - Medium (12–80): neutral (0.0)
///   - Long (>80): +0.10 for Feature/PlatformWork/Maintenance, neutral elsewhere
///
/// Test: covered by `length_signal_*` unit tests.
pub(crate) fn score_message_length(trimmed: &str) -> [f32; NUM_CATS] {
    let len = trimmed.len();
    let mut out = [0.0f32; NUM_CATS];
    if len < 12 {
        // Short → nudge toward KTLO, Merge, Maintenance; away from Feature
        out[Cat::Ktlo.index()] = 0.10;
        out[Cat::Merge.index()] = 0.10;
        out[Cat::Maintenance.index()] = 0.05;
        out[Cat::Feature.index()] = -0.05;
        out[Cat::Bugfix.index()] = -0.03;
    } else if len > 80 {
        // Long → nudge toward Feature and PlatformWork (complex changes)
        out[Cat::Feature.index()] = 0.10;
        out[Cat::PlatformWork.index()] = 0.10;
        out[Cat::Maintenance.index()] = 0.05;
        out[Cat::Bugfix.index()] = 0.05;
    }
    out
}

/// Score the merge indicator signal.
///
/// Why: the `is_merge` git flag and "Merge " prefix are strong structural
/// signals. When present, all score mass flows to the Merge category.
/// What: returns `MERGE_WEIGHTS` (large positive for Merge, mild negative
/// for all other categories) when the commit looks like a merge; all-zero
/// otherwise.
/// Test: covered by `merge_indicator_signal_*` unit tests.
pub(crate) fn score_merge_indicator(is_merge: bool, lower: &str) -> [f32; NUM_CATS] {
    let is_merge_commit = is_merge
        || lower.starts_with("merge pull request")
        || lower.starts_with("merge branch")
        || lower.starts_with("merge remote-tracking")
        || lower.starts_with("merge ");

    if !is_merge_commit {
        return [0.0; NUM_CATS];
    }

    *MERGE_WEIGHTS
}

/// Score the file-paths signal.
///
/// Why: the set of modified files provides orthogonal evidence to the commit
/// message. A commit touching mostly `tests/` is likely maintenance/QA; one
/// touching only `Cargo.toml` or `package.json` is likely KTLO (dependency
/// update).
/// What: buckets the changed paths into three categories and returns per-
/// category weights. When `paths` is empty (unavailable at classify-time),
/// returns all zeros so this signal does not penalise commits where paths
/// were not collected.
/// Test: covered by `file_paths_signal_*` unit tests.
pub(crate) fn score_file_paths(paths: &[String]) -> [f32; NUM_CATS] {
    if paths.is_empty() {
        return [0.0; NUM_CATS];
    }

    let total = paths.len() as f32;
    let test_count = paths
        .iter()
        .filter(|p| {
            p.contains("tests/")
                || p.contains("test/")
                || p.contains("spec/")
                || p.ends_with("_test.rs")
                || p.ends_with("_spec.rb")
                || p.ends_with(".test.ts")
                || p.ends_with(".spec.ts")
        })
        .count() as f32;
    let docs_count = paths
        .iter()
        .filter(|p| {
            p.contains("docs/")
                || p.contains("doc/")
                || p.ends_with(".md")
                || p.ends_with(".rst")
                || p.ends_with(".txt")
        })
        .count() as f32;
    let manifest_count = paths
        .iter()
        .filter(|p| {
            let name = p.split('/').next_back().unwrap_or(p.as_str());
            matches!(
                name,
                "Cargo.toml"
                    | "package.json"
                    | "pyproject.toml"
                    | "requirements.txt"
                    | "Gemfile"
                    | "pom.xml"
                    | "build.gradle"
                    | "go.mod"
                    | "Pipfile"
                    | "setup.py"
                    | "composer.json"
            )
        })
        .count() as f32;

    let mut out = [0.0f32; NUM_CATS];

    // tests-heavy → Maintenance (test refactors) or Bugfix (test-driven fix)
    let test_ratio = test_count / total;
    if test_ratio >= 0.5 {
        out[Cat::Maintenance.index()] += test_ratio * 0.20;
        out[Cat::Bugfix.index()] += test_ratio * 0.10;
    }

    // docs-heavy → Content
    let docs_ratio = docs_count / total;
    if docs_ratio >= 0.5 {
        out[Cat::Content.index()] += docs_ratio * 0.20;
    }

    // manifest-heavy → KTLO (dependency/build management)
    let manifest_ratio = manifest_count / total;
    if manifest_ratio >= 0.5 {
        out[Cat::Ktlo.index()] += manifest_ratio * 0.20;
        out[Cat::Maintenance.index()] += manifest_ratio * 0.10;
    }

    out
}

// ─── JIRA-style prefix detector ───────────────────────────────────────────────

/// Return `true` when the commit message begins with a `PROJ-123`-style token.
///
/// Why: shared with the fuzzy tier's `bare_ticket_prefix` logic but expressed
/// as a boolean here because the weighted-sum tier only needs a presence flag.
/// What: checks whether the first whitespace-separated token (after stripping
/// trailing `:` or `-`) matches `[A-Z][A-Z0-9]*-[0-9]+`.
/// Test: covered by `ticket_prefix_signal_*` unit tests.
fn has_jira_style_prefix(message: &str) -> bool {
    let first = match message.split_whitespace().next() {
        Some(s) => s,
        None => return false,
    };
    let candidate = first.trim_end_matches([':', '-', ',']);
    let mut parts = candidate.split('-');
    let project = match parts.next() {
        Some(s) => s,
        None => return false,
    };
    let number = match parts.next() {
        Some(s) => s,
        None => return false,
    };
    if parts.next().is_some() {
        return false;
    }
    if project.is_empty() || number.is_empty() {
        return false;
    }
    project
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_uppercase())
        && project
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && number.chars().all(|c| c.is_ascii_digit())
}

// ─── public API ───────────────────────────────────────────────────────────────

/// Configuration for the weighted-sum tier (Tier 2.5).
///
/// Why: operators need at minimum an on/off toggle and a confidence threshold
/// control so they can opt out of the tier or tune aggressiveness without
/// recompiling.
/// What: `enabled` gates the tier entirely; `min_confidence` is the argmax
/// floor below which the tier falls through to the fuzzy tier instead of
/// emitting a verdict.
/// Test: passing `WeightedSumConfig { enabled: false, .. }` to
/// `WeightedSumClassifier::classify` must always return `None`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeightedSumConfig {
    /// Whether the weighted-sum tier is active.
    ///
    /// Defaults to `true`. Set to `false` to revert Tier 2.5 behaviour to
    /// pre-1.3.0 fall-through (fuzzy tier handles everything after regex).
    #[serde(default = "default_weighted_sum_enabled")]
    pub enabled: bool,

    /// Minimum argmax score required to emit a verdict.
    ///
    /// If the best-scoring category's accumulated score is below this
    /// threshold, the tier returns `None` and the pipeline falls through to
    /// Tier 3 (fuzzy). Clamped to `[0.0, 1.0]` at construction time.
    ///
    /// The emitted confidence is clamped to `[min_confidence, 0.95]`:
    /// - Lower bound: `min_confidence` ensures we never emit below the
    ///   threshold (the caller checked this).
    /// - Upper bound: 0.95 preserves headroom for rule-based tiers that
    ///   should still outrank this one when they fire.
    ///
    /// Defaults to `0.55`.
    #[serde(default = "default_weighted_sum_min_confidence")]
    pub min_confidence: f32,
}

fn default_weighted_sum_enabled() -> bool {
    true
}

fn default_weighted_sum_min_confidence() -> f32 {
    0.55
}

impl Default for WeightedSumConfig {
    fn default() -> Self {
        Self {
            enabled: default_weighted_sum_enabled(),
            min_confidence: default_weighted_sum_min_confidence(),
        }
    }
}

/// Tier 2.5 — weighted-sum classifier.
///
/// Why: deterministic rules (Tiers 1 & 2) have hard cut-offs and miss commits
/// whose messages combine multiple weak signals. The fuzzy tier (Tier 3) uses
/// hand-crafted single-condition rules with fixed confidence levels. This tier
/// fills the gap by composing five complementary signals into a richer, calibrated
/// score before falling back to the fuzzy tier.
/// What: stateless; all configuration is captured at construction time in the
/// embedded [`WeightedSumConfig`]. `classify` computes a per-category score
/// array and emits the argmax when it clears `min_confidence`.
/// Test: see `tests` module below — unit tests per signal plus integration
/// and fall-through scenarios.
pub struct WeightedSumClassifier {
    pub(crate) config: WeightedSumConfig,
}

impl WeightedSumClassifier {
    /// Construct a classifier with the given configuration.
    ///
    /// Why: callers (the engine builder) pass a config extracted from
    /// `ClassificationConfig.weighted_sum`; separating construction from
    /// classification keeps the hot path free of config loading.
    /// What: stores the config; no signal tables are allocated (they are
    /// static).
    /// Test: `WeightedSumClassifier::new(config).classify(…)`.
    pub fn new(config: WeightedSumConfig) -> Self {
        Self { config }
    }

    /// Classify a commit message, optionally using `paths` to boost
    /// the file-path signal.
    ///
    /// Why: the pipeline's `classify_batch` path does not currently surface
    /// file paths to the classifier. By accepting an empty `paths` slice the
    /// API stays forward-compatible: when paths are available (future work or
    /// test fixtures) they improve accuracy; when absent the signal contributes
    /// zero and the other four signals carry the verdict.
    /// What: computes a length-5 × length-8 weighted-sum, takes the argmax,
    /// and returns `Some(ClassificationResult)` when the score exceeds
    /// `min_confidence`. Returns `None` to fall through to the fuzzy tier.
    /// Test: `integration_fix_message_classifies_as_bugfix` and
    /// `fall_through_when_no_signal_dominates` in the test module below.
    pub fn classify(
        &self,
        message: &str,
        is_merge: bool,
        paths: &[String],
    ) -> Option<ClassificationResult> {
        if !self.config.enabled {
            return None;
        }

        let trimmed = message.trim();
        let lower = trimmed.to_lowercase();

        // Accumulate per-category scores from the five signals.
        let mut scores = [0.0f32; NUM_CATS];

        // Signal 0: keyword density
        let keyword_scores = score_keywords(&lower);
        for i in 0..NUM_CATS {
            scores[i] += keyword_scores[i];
        }

        // Signal 1: ticket prefix
        let ticket_scores = score_ticket_prefix(trimmed);
        for i in 0..NUM_CATS {
            scores[i] += ticket_scores[i];
        }

        // Signal 2: message length (computed dynamically, not from the static table)
        let length_scores = score_message_length(trimmed);
        for i in 0..NUM_CATS {
            scores[i] += length_scores[i];
        }

        // Signal 3: merge indicator
        let merge_scores = score_merge_indicator(is_merge, &lower);
        for i in 0..NUM_CATS {
            scores[i] += merge_scores[i];
        }

        // Signal 4: file paths (zero when paths is empty)
        let path_scores = score_file_paths(paths);
        for i in 0..NUM_CATS {
            scores[i] += path_scores[i];
        }

        // Argmax over categories.
        let (best_cat_idx, &best_score) = scores
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))?;

        // Tie-breaking: if two categories share the exact same top score,
        // return None — the tier does not emit a guess when confidence is
        // equally split. This also prevents emitting a verdict when all
        // signals are zero (scores == [0.0; N]).
        let tie_count = scores.iter().filter(|&&s| s == best_score).count();
        if tie_count > 1 || best_score <= 0.0 {
            return None;
        }

        if (best_score as f64) < self.config.min_confidence as f64 {
            return None;
        }

        let best_cat = Cat::ALL[best_cat_idx];
        let (category, top_level) = best_cat.to_verdict();

        // Clamp confidence to [min_confidence, 0.95].
        let confidence = (best_score as f64)
            .max(self.config.min_confidence as f64)
            .min(0.95);

        Some(ClassificationResult {
            category: category.to_string(),
            subcategory: None,
            top_level: Some(top_level),
            confidence,
            method: ClassificationMethod::WeightedSum,
            ticket_id: None,
            complexity: None,
        })
    }
}
