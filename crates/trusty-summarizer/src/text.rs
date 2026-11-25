//! Line indexing and 1-based inclusive line ranges.
//!
//! Why: the outline's references and `read --lines` must agree on what a
//! line is. Both use [`LineIndex`]: a line is a `split_inclusive('\n')`
//! piece, so a final line without a newline counts and an empty text has
//! zero lines.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// A 1-based inclusive range, serialized as `[start, end]`.
///
/// Invariant: `1 <= start <= end`. Used for both line and paragraph ranges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "[usize; 2]", into = "[usize; 2]")]
pub struct Span {
    /// First item, 1-based.
    pub start: usize,
    /// Last item, inclusive.
    pub end: usize,
}

impl Span {
    /// Builds a span; debug-asserts the invariant.
    pub fn new(start: usize, end: usize) -> Self {
        debug_assert!(start >= 1 && start <= end, "bad span {start}-{end}");
        Span { start, end }
    }

    /// Number of items covered.
    pub fn len(&self) -> usize {
        self.end - self.start + 1
    }

    /// Always false: a span covers at least one item.
    pub fn is_empty(&self) -> bool {
        false
    }
}

impl From<[usize; 2]> for Span {
    fn from(v: [usize; 2]) -> Self {
        Span {
            start: v[0],
            end: v[1],
        }
    }
}

impl From<Span> for [usize; 2] {
    fn from(s: Span) -> Self {
        [s.start, s.end]
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.start, self.end)
    }
}

impl FromStr for Span {
    type Err = Error;

    /// Parses `A-B` or a single line `A`; both 1-based, `A <= B`.
    fn from_str(s: &str) -> Result<Self> {
        let bad = || Error::InvalidRange(format!("`{s}` is not A-B with 1 <= A <= B"));
        let (a, b) = match s.split_once('-') {
            Some((a, b)) => (a.trim(), b.trim()),
            None => (s.trim(), s.trim()),
        };
        let start: usize = a.parse().map_err(|_| bad())?;
        let end: usize = b.parse().map_err(|_| bad())?;
        if start == 0 || start > end {
            return Err(bad());
        }
        Ok(Span { start, end })
    }
}

/// Byte offsets of every line start in a text.
#[derive(Debug, Clone)]
pub struct LineIndex<'a> {
    text: &'a str,
    starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    /// Indexes `text` in one pass.
    pub fn new(text: &'a str) -> Self {
        let mut starts = Vec::new();
        let mut offset = 0;
        for piece in text.split_inclusive('\n') {
            starts.push(offset);
            offset += piece.len();
        }
        LineIndex { text, starts }
    }

    /// Number of lines.
    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    /// Line `n` (1-based) without its `\n` or `\r\n` terminator.
    ///
    /// Precondition: `1 <= n <= line_count()`.
    pub fn line(&self, n: usize) -> &'a str {
        let raw = self.raw(Span::new(n, n));
        let raw = raw.strip_suffix('\n').unwrap_or(raw);
        raw.strip_suffix('\r').unwrap_or(raw)
    }

    /// The exact bytes of lines `span`, terminators included.
    ///
    /// Errors with [`Error::InvalidRange`] when the span ends past the last
    /// line.
    pub fn slice(&self, span: Span) -> Result<&'a str> {
        if span.start == 0 || span.start > span.end || span.end > self.line_count() {
            return Err(Error::InvalidRange(format!(
                "{span} is outside 1-{} (the document has {} lines)",
                self.line_count(),
                self.line_count()
            )));
        }
        Ok(self.raw(span))
    }

    /// Like [`LineIndex::slice`], but an end past the last line is cut to the
    /// last line; returns the span actually served.
    ///
    /// Errors with [`Error::InvalidRange`] when the start is past the last
    /// line, or the span is malformed.
    pub fn slice_clamped(&self, span: Span) -> Result<(&'a str, Span)> {
        let n = self.line_count();
        let served = if span.start >= 1 && span.start <= span.end && span.start <= n {
            Span::new(span.start, span.end.min(n))
        } else {
            span
        };
        Ok((self.slice(served)?, served))
    }

    fn raw(&self, span: Span) -> &'a str {
        let from = self.starts[span.start - 1];
        let to = self
            .starts
            .get(span.end)
            .copied()
            .unwrap_or(self.text.len());
        &self.text[from..to]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_counts_follow_split_inclusive() {
        assert_eq!(LineIndex::new("").line_count(), 0);
        assert_eq!(LineIndex::new("a").line_count(), 1);
        assert_eq!(LineIndex::new("a\n").line_count(), 1);
        assert_eq!(LineIndex::new("a\n\nb").line_count(), 3);
    }

    #[test]
    fn slice_returns_exact_bytes() {
        let idx = LineIndex::new("one\r\ntwo\nthree");
        assert_eq!(idx.slice(Span::new(2, 3)).unwrap(), "two\nthree");
        assert_eq!(idx.slice(Span::new(1, 1)).unwrap(), "one\r\n");
        assert_eq!(idx.line(1), "one");
        assert!(idx.slice(Span::new(3, 4)).is_err());
    }

    #[test]
    fn span_parses_and_rejects() {
        assert_eq!("2-5".parse::<Span>().unwrap(), Span::new(2, 5));
        assert_eq!("7".parse::<Span>().unwrap(), Span::new(7, 7));
        for bad in ["0-2", "5-2", "a-b", "", "3-"] {
            assert!(bad.parse::<Span>().is_err(), "{bad}");
        }
    }
}
