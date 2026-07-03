//! Generic content-filter strategies (ported from RTK, MIT).
//!
//! Why: Lets callers swap filtering policy (None / Minimal / Aggressive)
//! without branching at each call site, and makes comment stripping
//! language-aware so we don't strip `//` from Python or `#` from Rust.
//! What: `FilterLevel`, `Language`, the `FilterStrategy` trait + three impls,
//! and the `get_filter` factory.
//! Test: `filter_strategy_*` / `language_*` in `tool_output::tests`.

/// Filter aggressiveness level.
///
/// Why: Different content tolerates different filtering. Code may want
/// `Minimal` (preserve comments); large logs may want `Aggressive`.
/// What: Three levels — None passes through, Minimal removes blank/whitespace,
/// Aggressive also strips line comments by language.
/// Test: `filter_strategy_*` tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterLevel {
    None,
    Minimal,
    Aggressive,
}

/// Source language for comment-aware filtering.
///
/// Why: Stripping `//` comments from Python or `#` comments from Rust is wrong.
/// Aggressive filtering needs to know the language to use the right syntax.
/// What: Enum of supported languages with `from_extension` and
/// `comment_prefix` / `block_comment` helpers.
/// Test: `language_from_extension_known`, `language_comment_prefix_rust`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Go,
    Shell,
    Data,
    Unknown,
}

impl Language {
    /// Map a file extension (without leading dot) to a `Language`.
    pub fn from_extension(ext: &str) -> Self {
        match ext.trim_start_matches('.').to_ascii_lowercase().as_str() {
            "rs" => Self::Rust,
            "py" | "pyi" => Self::Python,
            "js" | "mjs" | "cjs" | "jsx" => Self::JavaScript,
            "ts" | "tsx" => Self::TypeScript,
            "go" => Self::Go,
            "sh" | "bash" | "zsh" => Self::Shell,
            "json" | "yaml" | "yml" | "toml" | "csv" => Self::Data,
            _ => Self::Unknown,
        }
    }

    /// Line-comment prefix for this language, if any.
    pub fn comment_prefix(&self) -> Option<&'static str> {
        match self {
            Self::Rust | Self::JavaScript | Self::TypeScript | Self::Go => Some("//"),
            Self::Python | Self::Shell => Some("#"),
            Self::Data | Self::Unknown => None,
        }
    }

    /// Block-comment open/close delimiters for this language, if any.
    pub fn block_comment(&self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Rust | Self::JavaScript | Self::TypeScript | Self::Go => Some(("/*", "*/")),
            Self::Python => Some(("\"\"\"", "\"\"\"")),
            _ => None,
        }
    }
}

/// Strategy trait for content filtering.
///
/// Why: Lets callers swap filtering policy (None / Minimal / Aggressive)
/// without branching at each call site.
/// What: One method, `filter(&self, content, lang) -> String`. Implementors
/// are stateless and `Send + Sync` so they can be cached/shared.
/// Test: `filter_strategy_no_filter_identity`, `filter_strategy_minimal_drops_blanks`,
/// `filter_strategy_aggressive_strips_rust_line_comments`.
pub trait FilterStrategy: Send + Sync {
    fn filter(&self, content: &str, lang: Language) -> String;
}

/// Pass-through filter — returns content unchanged.
pub struct NoFilter;

impl FilterStrategy for NoFilter {
    fn filter(&self, content: &str, _lang: Language) -> String {
        content.to_string()
    }
}

/// Minimal filter — removes blank lines and trailing whitespace.
pub struct MinimalFilter;

impl FilterStrategy for MinimalFilter {
    fn filter(&self, content: &str, _lang: Language) -> String {
        content
            .lines()
            .map(|l| l.trim_end())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Aggressive filter — minimal + strips line comments by language.
pub struct AggressiveFilter;

impl FilterStrategy for AggressiveFilter {
    fn filter(&self, content: &str, lang: Language) -> String {
        let prefix = lang.comment_prefix();
        let mut out: Vec<&str> = Vec::new();
        for line in content.lines() {
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(p) = prefix {
                let leading = trimmed.trim_start();
                if leading.starts_with(p) {
                    continue;
                }
            }
            out.push(trimmed);
        }
        // Collapse runs of equivalent lines? Aggressive just drops; keep simple.
        out.join("\n")
    }
}

/// Get a `FilterStrategy` impl for the requested level.
///
/// Why: Lets callers obtain a strategy without naming the concrete type.
/// What: Boxed trait object, one allocation per call (cheap, infrequent).
/// Test: `get_filter_returns_expected_type`.
pub fn get_filter(level: FilterLevel) -> Box<dyn FilterStrategy> {
    match level {
        FilterLevel::None => Box::new(NoFilter),
        FilterLevel::Minimal => Box::new(MinimalFilter),
        FilterLevel::Aggressive => Box::new(AggressiveFilter),
    }
}
