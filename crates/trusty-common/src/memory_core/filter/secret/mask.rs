//! Secret shape and masking: describe a secret without printing it (#8624).
//!
//! Why (#8624): the kuzu import review exports refused rows for an agent to
//! judge. The agent needs to know what kind of thing was refused — its rule,
//! length, character mix and randomness — and must never see the text itself.
//! What: [`classify_secret`] measures a span's [`SecretShape`];
//! [`mask_content`] replaces each [`SecretSpan`] with a marker built from its
//! rule and shape. `mask_content` is the only code that builds a marker.
//! Test: `marker_and_output_carry_no_four_char_window_of_the_token`,
//! `text_outside_spans_passes_through_byte_for_byte`,
//! `malformed_span_lists_are_errors_not_panics`.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::ops::Range;

use super::spans::SecretSpan;

/// Character-class strings, indexed by a 4-bit mask: bit 0 upper, bit 1
/// lower, bit 2 digit, bit 3 symbol. Each string lists its classes in the
/// fixed order `u`, `l`, `d`, `s`.
const CLASS_STRINGS: [&str; 16] = [
    "", "u", "l", "ul", "d", "ud", "ld", "uld", "s", "us", "ls", "uls", "ds", "uds", "lds", "ulds",
];

/// The shape of a secret: what a reviewer may learn about it without its text.
///
/// Why (#8624): a refused span is judged by its shape — a 40-char `ulds` run
/// at 4.9 bits/char reads as a key, a 12-char `l` run at 2.1 does not — so the
/// shape is reported in place of the text.
/// What: `len` is the length in chars. `cls` is the set of classes present,
/// in the fixed order `u` (ASCII upper), `l` (ASCII lower), `d` (ASCII digit),
/// `s` (any other char, including whitespace and non-ASCII); `""` for an empty
/// string. `ent_tenths` is the Shannon entropy over chars in bits per char,
/// times ten, rounded to the nearest integer (`46` is 4.6 bits/char).
/// Test: `cls_names_each_class_combination`, `ent_known_answers`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct SecretShape {
    pub len: usize,
    pub cls: &'static str,
    pub ent_tenths: u16,
}

/// Measure the shape of `text`, usually the text of one [`SecretSpan`].
///
/// Why (#8624): one definition of `len`, `cls` and `ent`, in trusty-common, so
/// every marker and every later threshold (#8658) reads the same numbers.
/// What: counts chars, ORs each char's class bit into a mask, and computes
/// `-Σ p·log2 p` over the char frequencies. Infallible and total over `&str`;
/// the empty string is `{ len: 0, cls: "", ent_tenths: 0 }`.
/// Test: `cls_names_each_class_combination`, `ent_known_answers`,
/// `len_counts_chars_not_bytes`.
pub fn classify_secret(text: &str) -> SecretShape {
    let mut mask = 0usize;
    let mut counts: HashMap<char, u32> = HashMap::new();
    let mut len = 0usize;
    for c in text.chars() {
        len += 1;
        mask |= if c.is_ascii_uppercase() {
            1
        } else if c.is_ascii_lowercase() {
            2
        } else if c.is_ascii_digit() {
            4
        } else {
            8
        };
        *counts.entry(c).or_default() += 1;
    }
    let total = len as f64;
    let bits: f64 = counts
        .values()
        .map(|&n| {
            let p = f64::from(n) / total;
            -p * p.log2()
        })
        .sum();
    // Entropy is at most log2(len) <= 64 bits, so tenths fit a u16.
    let ent_tenths = (bits * 10.0).round().clamp(0.0, f64::from(u16::MAX)) as u16;
    SecretShape {
        len,
        cls: CLASS_STRINGS[mask],
        ent_tenths,
    }
}

/// Why [`mask_content`] refused a span list. Carries offsets only, never text.
///
/// What: `index` is the 0-based position of the offending span in the list.
/// Test: `malformed_span_lists_are_errors_not_panics`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MaskError {
    #[error("span {index} ({range:?}) ends past the content's {len} bytes")]
    OutOfBounds {
        index: usize,
        range: Range<usize>,
        len: usize,
    },
    #[error("span {index} ({range:?}) is empty or reversed")]
    Empty { index: usize, range: Range<usize> },
    #[error("span {index} ({range:?}) does not start and end on char boundaries")]
    NotCharBoundary { index: usize, range: Range<usize> },
    #[error(
        "span {index} ({range:?}) overlaps or precedes the span before it, which ends at {prev_end}"
    )]
    Unordered {
        index: usize,
        range: Range<usize>,
        prev_end: usize,
    },
}

/// `content` with each span replaced by a marker that names its rule and shape.
///
/// Why (#8624): the review export must carry the row's surrounding text but
/// none of the secret. This is the only code that builds a marker, so a
/// marker format can never leak a prefix preview from a second builder.
/// What: each span becomes `⟦S{i}:{rule}:{len}:{cls}:{ent}⟧`, where `i` is
/// the span's 0-based index in `spans` (so `S0` is `spans[0]`), `rule` is
/// [`super::SecretRule::label`], and `len`/`cls`/`ent` are the span text's
/// [`classify_secret`] shape with `ent` printed to one decimal place. Text
/// outside the spans is copied byte for byte. `spans` must be what
/// [`super::find_secret_spans`] returns: non-empty, in bounds, on char
/// boundaries, ascending and disjoint. Any other list is a [`MaskError`],
/// checked before its span is read, so this never panics and never returns
/// a partial string.
/// Test: `marker_and_output_carry_no_four_char_window_of_the_token`,
/// `text_outside_spans_passes_through_byte_for_byte`,
/// `malformed_span_lists_are_errors_not_panics`, `markers_are_numbered_from_zero`.
pub fn mask_content(content: &str, spans: &[SecretSpan]) -> Result<String, MaskError> {
    let mut out = String::with_capacity(content.len());
    let mut prev_end = 0usize;
    for (index, span) in spans.iter().enumerate() {
        let range = span.range.clone();
        if range.end > content.len() {
            return Err(MaskError::OutOfBounds {
                index,
                range,
                len: content.len(),
            });
        }
        if range.start >= range.end {
            return Err(MaskError::Empty { index, range });
        }
        if range.start < prev_end {
            return Err(MaskError::Unordered {
                index,
                range,
                prev_end,
            });
        }
        // `get` is `None` exactly when an end is off a char boundary, since
        // both ends are already known to be in bounds and ordered.
        let (Some(before), Some(secret)) = (
            content.get(prev_end..range.start),
            content.get(range.clone()),
        ) else {
            return Err(MaskError::NotCharBoundary { index, range });
        };
        let shape = classify_secret(secret);
        out.push_str(before);
        // #8624: the one marker builder. Writing to a String cannot fail.
        let _ = write!(
            out,
            "⟦S{index}:{}:{}:{}:{}.{}⟧",
            span.rule.label(),
            shape.len,
            shape.cls,
            shape.ent_tenths / 10,
            shape.ent_tenths % 10
        );
        prev_end = range.end;
    }
    // `prev_end` is a span end already checked as a char boundary, or 0.
    out.push_str(content.get(prev_end..).unwrap_or_default());
    Ok(out)
}

#[cfg(test)]
#[path = "mask_tests.rs"]
mod tests;
