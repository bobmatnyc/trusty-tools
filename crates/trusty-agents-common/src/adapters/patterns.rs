//! Pattern matching utilities for harness output analysis.
//!
//! Why: Adapters (Phase 2) need to detect idle/working/error states from raw
//! pane output. Compiling regexes lazily via `OnceLock` gives us the
//! ergonomics of `static` patterns without the `lazy_static` macro.
//! What: `Pattern` struct holds a name, a (lazily compiled) regex, and a
//! confidence score. `any_match` and `best_match` operate over slices.
//! Test: See `#[cfg(test)]` block.

use regex::Regex;
use std::sync::OnceLock;

/// A named, lazily-compiled regex pattern with a confidence score.
#[derive(Debug)]
pub struct Pattern {
    /// Human-readable name for this pattern.
    pub name: &'static str,
    /// Lazily compiled regex.
    regex: OnceLock<Regex>,
    /// Source pattern string (compiled on first use).
    pattern: &'static str,
    /// Confidence level when this pattern matches (0.0 - 1.0).
    pub confidence: f32,
}

impl Pattern {
    /// Construct a new pattern. Regex compilation is deferred until the first
    /// `matches`/`captures` call.
    pub const fn new(name: &'static str, pattern: &'static str, confidence: f32) -> Self {
        Self {
            name,
            regex: OnceLock::new(),
            pattern,
            confidence,
        }
    }

    /// Internal: get (or compile) the regex.
    fn compiled(&self) -> &Regex {
        self.regex
            .get_or_init(|| Regex::new(self.pattern).expect("invalid regex"))
    }

    /// Returns true if the pattern matches anywhere in `text`.
    pub fn matches(&self, text: &str) -> bool {
        self.compiled().is_match(text)
    }

    /// Returns captured groups from the first match, if any.
    pub fn captures(&self, text: &str) -> Option<Vec<String>> {
        self.compiled().captures(text).map(|caps| {
            caps.iter()
                .skip(1)
                .filter_map(|m| m.map(|m| m.as_str().to_string()))
                .collect()
        })
    }
}

/// Returns true if any pattern in the slice matches.
pub fn any_match(text: &str, patterns: &[Pattern]) -> bool {
    patterns.iter().any(|p| p.matches(text))
}

/// Returns the last `n` lines of `text` joined with newlines.
///
/// Why: Adapters typically only need to inspect recent pane output for state
/// detection — looking at the entire scrollback would be slow and prone to
/// false positives from stale content.
/// What: Splits `text` on lines, takes the last `n`, rejoins with `\n`.
/// Test: Empty input returns empty; n larger than line count returns full text.
pub fn last_n_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// Returns the highest-confidence matching pattern, if any.
pub fn best_match<'a>(text: &str, patterns: &'a [Pattern]) -> Option<&'a Pattern> {
    patterns.iter().filter(|p| p.matches(text)).max_by(|a, b| {
        a.confidence
            .partial_cmp(&b.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pattern_matches() {
        let p = Pattern::new("test", r"hello \w+", 0.9);
        assert!(p.matches("hello world"));
        assert!(!p.matches("goodbye world"));
    }

    #[test]
    fn test_pattern_captures() {
        let p = Pattern::new("test", r"hello (\w+)", 0.9);
        let caps = p.captures("hello world").unwrap();
        assert_eq!(caps, vec!["world"]);
    }

    #[test]
    fn test_any_match() {
        let patterns = [
            Pattern::new("a", r"foo", 0.5),
            Pattern::new("b", r"bar", 0.7),
        ];
        assert!(any_match("there is a foo here", &patterns));
        assert!(any_match("just bar", &patterns));
        assert!(!any_match("nothing", &patterns));
    }

    #[test]
    fn test_best_match_picks_highest_confidence() {
        let patterns = [
            Pattern::new("low", r"hello", 0.5),
            Pattern::new("high", r"hello", 0.9),
            Pattern::new("nomatch", r"goodbye", 1.0),
        ];
        let best = best_match("hello there", &patterns).unwrap();
        assert_eq!(best.name, "high");
    }
}
