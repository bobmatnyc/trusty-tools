//! The `.env` subset `tm secrets exec --dotenv` reads (DOC-74 §15.8 tier 2).
//!
//! Why: S7 — a `.env` file holds `secret://` references, and `exec` resolves
//! them. `.env` dialects disagree on quoting, escapes, expansion and
//! multi-line values, so this parser accepts one small subset and rejects
//! the rest rather than guess what a line means.
//! What: [`parse_dotenv`]. The accepted subset, one entry per line:
//! - blank lines and lines whose first non-blank character is `#`;
//! - `KEY=value` or `export KEY=value`, where `KEY` is a POSIX name and sits
//!   directly against `=`;
//! - an unquoted value, trimmed, ending at ` #` (whitespace then `#`);
//! - `'single'` quotes: literal text, no escapes;
//! - `"double"` quotes: literal text, with no `\` and no `$`;
//! - after a closing quote, only whitespace or a ` # comment`.
//!
//! Rejected, as [`SecretsError::DotenvSyntax`] naming the line number:
//! multi-line values (an unclosed quote), escapes, `$`/`${VAR}` expansion
//! outside single quotes, a quote or `\` inside an unquoted value, an
//! unquoted value starting with `#`, control characters other than tab, and a
//! name defined twice. Errors never echo the line.
//! Test: `dotenv_accepts_the_documented_subset`,
//! `dotenv_rejects_unsupported_syntax_by_line`,
//! `dotenv_errors_never_echo_the_line`.

use std::collections::BTreeSet;

use super::resolve::{EnvEntry, validate_env_name};
use crate::api::SecretsError;

const BLANKS: [char; 2] = [' ', '\t'];

/// Parse `.env` text into env entries, in file order.
///
/// Why: see the module docs.
/// What: lines split on `\n`, a trailing `\r` dropped. Values are returned
/// unresolved; pass the entries to [`super::resolve_env`].
/// Test: `dotenv_accepts_the_documented_subset`,
/// `dotenv_rejects_unsupported_syntax_by_line`.
pub fn parse_dotenv(text: &str) -> Result<Vec<EnvEntry>, SecretsError> {
    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, line) in text.split('\n').enumerate() {
        let fail = |reason| SecretsError::DotenvSyntax {
            line: index + 1,
            reason,
        };
        let line = line.strip_suffix('\r').unwrap_or(line);
        let body = line.trim_start_matches(BLANKS);
        if body.trim_end_matches(BLANKS).is_empty() || body.starts_with('#') {
            continue;
        }
        let body = match body.strip_prefix("export") {
            Some(rest) if rest.starts_with(BLANKS) => rest.trim_start_matches(BLANKS),
            _ => body,
        };
        let Some((name, value)) = body.split_once('=') else {
            return Err(fail("is not `KEY=value`"));
        };
        validate_env_name(name).map_err(|_| {
            fail("has an invalid name (a POSIX name directly against `=` is required)")
        })?;
        let value = parse_value(value).map_err(fail)?;
        if !seen.insert(name) {
            return Err(fail("defines a name an earlier line already defined"));
        }
        entries.push(EnvEntry::new(name, value));
    }
    Ok(entries)
}

/// Parse the text after `=` into the literal value.
fn parse_value(value: &str) -> Result<String, &'static str> {
    let value = value.trim_matches(BLANKS);
    let inner = if let Some(rest) = value.strip_prefix('\'') {
        let (inner, after) = rest
            .split_once('\'')
            .ok_or("has an unclosed `'` (multi-line values are not supported)")?;
        closing_tail(after)?;
        inner
    } else if let Some(rest) = value.strip_prefix('"') {
        let (inner, after) = rest
            .split_once('"')
            .ok_or("has an unclosed `\"` (multi-line values are not supported)")?;
        closing_tail(after)?;
        if inner.contains('\\') {
            return Err("uses `\\` inside double quotes; escapes are not supported");
        }
        if inner.contains('$') {
            return Err("uses `$` inside double quotes; expansion is not supported");
        }
        inner
    } else {
        if value.starts_with('#') {
            return Err("has an unquoted value starting with `#`; quote it");
        }
        let comment = value
            .char_indices()
            .find(|&(at, c)| c == '#' && value[..at].ends_with(BLANKS))
            .map(|(at, _)| at);
        let inner = match comment {
            Some(at) => value[..at].trim_end_matches(BLANKS),
            None => value,
        };
        if inner.contains(['\'', '"']) {
            return Err("has a quote inside an unquoted value");
        }
        if inner.contains('\\') {
            return Err("uses `\\` in an unquoted value; escapes are not supported");
        }
        if inner.contains('$') {
            return Err("uses `$` in an unquoted value; expansion is not supported");
        }
        inner
    };
    if inner.chars().any(|c| c.is_control() && c != '\t') {
        return Err("has a control character in the value");
    }
    Ok(inner.to_string())
}

/// Accept only whitespace or a ` # comment` after a closing quote.
fn closing_tail(after: &str) -> Result<(), &'static str> {
    let rest = after.trim_start_matches(BLANKS);
    if rest.is_empty() || (rest.starts_with('#') && rest.len() < after.len()) {
        Ok(())
    } else {
        Err("has text after the closing quote")
    }
}
