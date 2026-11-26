//! `$HOME/.trusty-tools/install.toml`, read without a TOML dependency (#11821).
//!
//! Why: the leaf rule (#9171 A1) keeps `toml` out of this crate, yet the
//! Architect must read the same install file tm reads, and refuse it when it is
//! broken (Architect ruling D1af(1), option A).
//! What: [`read_install_file`] maps an absent file to `None` and parses the
//! TOML 1.0 subset a flat table of string keys can hold, accepting and
//! refusing as trusty-common's `toml` 0.8 reader does: a leading BOM; `#`
//! comments; LF or CRLF line ends (a lone CR is refused); bare or one-line
//! quoted keys; basic, literal and multi-line strings with TOML 1.0 escapes
//! (multi-line CRLF reads as LF). A table header, a dotted key, a non-string
//! value, an unknown or duplicate key, or a control character outside a tab
//! is a `Parse` error, as each is for the `toml` crate with
//! `deny_unknown_fields`. The parity test in trusty-crate-contracts holds the
//! two readers to the same answer.
//! Test: `install_paths::tests` (`src/install_paths/tests.rs`).

use std::path::{Path, PathBuf};

use super::InstallPathsError;

/// Directory under home that holds the install file.
pub const TOOLS_DIR_NAME: &str = ".trusty-tools";

/// File name of the install file under `$HOME/.trusty-tools`.
pub const INSTALL_FILE_NAME: &str = "install.toml";

/// The keys `install.toml` may carry, as trusty-common's `InstallFile`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallFile {
    /// Root under which managed projects are cloned.
    pub projects_root: Option<PathBuf>,
    /// Directory holding `.trusty-tools` and `.trusty-mpm`.
    pub config_dir: Option<PathBuf>,
    /// Claude Code's user-tier config dir (read, not used by the Architect).
    pub claude_config_dir: Option<PathBuf>,
    /// Codex's home dir (read, not used by the Architect).
    pub codex_home: Option<PathBuf>,
}

/// `<home>/.trusty-tools/install.toml`, anchored at home, never the config dir.
pub fn install_file_path(home: &Path) -> PathBuf {
    home.join(TOOLS_DIR_NAME).join(INSTALL_FILE_NAME)
}

/// Read `<home>/.trusty-tools/install.toml`.
///
/// Why: the resolver needs "absent" (no file rung) apart from "broken".
/// What: `Ok(None)` when the file does not exist; `Read` on any other I/O
/// error, invalid UTF-8 included; `Parse` on input the `toml` crate refuses.
/// Values are not validated here; the resolver validates the winning one.
/// Test: `absent_file_is_none`, `broken_files_are_refused`,
/// `valid_spellings_parse`, `multi_line_strings_parse_as_toml_does`.
pub fn read_install_file(home: &Path) -> Result<Option<InstallFile>, InstallPathsError> {
    let path = install_file_path(home);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(InstallPathsError::Read { path, source }),
    };
    parse(&raw)
        .map(Some)
        .map_err(|message| InstallPathsError::Parse { path, message })
}

/// Parse the install file body; the error is a message naming the line.
pub(super) fn parse(raw: &str) -> Result<InstallFile, String> {
    // #11821: the `toml` crate accepts a BOM at the start only.
    let body = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let mut file = InstallFile::default();
    let mut rest = body;
    while !rest.is_empty() {
        rest = statement(rest, &mut file).map_err(|e| {
            let line = body[..body.len() - e.1.len()].matches('\n').count() + 1;
            format!("{} (line {line})", e.0)
        })?;
    }
    Ok(file)
}

/// A parse failure: the message and the input left where it happened.
type Fail<'a> = (String, &'a str);

fn fail<T>(message: impl Into<String>, at: &str) -> Result<T, Fail<'_>> {
    Err((message.into(), at))
}

/// One line: blank, a comment, or `key = value [comment]`, with its line end.
fn statement<'a>(s: &'a str, file: &mut InstallFile) -> Result<&'a str, Fail<'a>> {
    let s = skip_ws(s);
    if s.starts_with('[') {
        return fail("table headers are not allowed", s);
    }
    let s = if s.is_empty() || s.starts_with(['#', '\n', '\r']) {
        s
    } else {
        key_value(s, file)?
    };
    let s = comment(skip_ws(s))?;
    line_end(s)
}

/// `key = value`, stored into its slot.
fn key_value<'a>(s: &'a str, file: &mut InstallFile) -> Result<&'a str, Fail<'a>> {
    let (key, rest) = key(s)?;
    let rest = skip_ws(rest);
    if rest.starts_with('.') {
        return fail("dotted keys are not allowed", rest);
    }
    let Some(rest) = rest.strip_prefix('=') else {
        return fail(format!("expected `=` after key `{key}`"), rest);
    };
    let (value, rest) = string_value(skip_ws(rest))?;
    let slot = match key.as_str() {
        "projects_root" => &mut file.projects_root,
        "config_dir" => &mut file.config_dir,
        "claude_config_dir" => &mut file.claude_config_dir,
        "codex_home" => &mut file.codex_home,
        other => return fail(format!("unknown key `{other}`"), s),
    };
    if slot.is_some() {
        return fail(format!("duplicate key `{key}`"), s);
    }
    *slot = Some(PathBuf::from(value));
    Ok(rest)
}

/// A bare key (`A-Z a-z 0-9 _ -`) or a one-line quoted key.
fn key(s: &str) -> Result<(String, &str), Fail<'_>> {
    if let Some(body) = s.strip_prefix('"') {
        return basic(body);
    }
    if let Some(body) = s.strip_prefix('\'') {
        return literal(body);
    }
    let end = s
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(s.len());
    if end == 0 {
        return fail("expected a key", s);
    }
    Ok((s[..end].to_owned(), &s[end..]))
}

/// A string value of any of the four kinds.
fn string_value(s: &str) -> Result<(String, &str), Fail<'_>> {
    if let Some(body) = s.strip_prefix("\"\"\"") {
        multi_line(trim_first_newline(body), true)
    } else if let Some(body) = s.strip_prefix("'''") {
        multi_line(trim_first_newline(body), false)
    } else if let Some(body) = s.strip_prefix('"') {
        basic(body)
    } else if let Some(body) = s.strip_prefix('\'') {
        literal(body)
    } else {
        fail("expected a string value", s)
    }
}

/// A one-line basic string after its opening `"`.
fn basic(s: &str) -> Result<(String, &str), Fail<'_>> {
    let mut out = String::new();
    let mut rest = s;
    loop {
        let Some(c) = rest.chars().next() else {
            return fail("unterminated string", rest);
        };
        match c {
            '"' => return Ok((out, &rest[1..])),
            '\\' => rest = escape(&rest[1..], &mut out)?,
            c => {
                allowed(c, rest)?;
                out.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
}

/// A one-line literal string after its opening `'`.
fn literal(s: &str) -> Result<(String, &str), Fail<'_>> {
    let Some(end) = s.find('\'') else {
        return fail("unterminated string", s);
    };
    for c in s[..end].chars() {
        allowed(c, s)?;
    }
    Ok((s[..end].to_owned(), &s[end + 1..]))
}

/// A multi-line string after its opening delimiter (and trimmed newline).
fn multi_line(s: &str, escapes: bool) -> Result<(String, &str), Fail<'_>> {
    let quote = if escapes { '"' } else { '\'' };
    let mut out = String::new();
    let mut rest = s;
    loop {
        let Some(c) = rest.chars().next() else {
            return fail("unterminated multi-line string", rest);
        };
        if c == quote {
            let run = rest.len() - rest.trim_start_matches(quote).len();
            if run >= 3 {
                // Up to two quotes before the closing three are content.
                let inner = (run - 3).min(2);
                out.extend(std::iter::repeat_n(quote, inner));
                return Ok((out, &rest[inner + 3..]));
            }
            out.extend(std::iter::repeat_n(quote, run));
            rest = &rest[run..];
        } else if c == '\\' && escapes {
            rest = escape_or_line_end(&rest[1..], &mut out)?;
        } else if let Some(after) = newline(rest) {
            out.push('\n');
            rest = after;
        } else {
            allowed(c, rest)?;
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
}

/// After a `\` in a multi-line basic string: a line-ending backslash (which
/// drops the whitespace and newlines that follow) or an ordinary escape.
fn escape_or_line_end<'a>(s: &'a str, out: &mut String) -> Result<&'a str, Fail<'a>> {
    let ws = skip_ws(s);
    if let Some(mut rest) = newline(ws) {
        loop {
            rest = skip_ws(rest);
            match newline(rest) {
                Some(after) => rest = after,
                None => return Ok(rest),
            }
        }
    }
    escape(s, out)
}

/// The escape after a `\`: TOML 1.0's set, as `toml` 0.8 reads it.
fn escape<'a>(s: &'a str, out: &mut String) -> Result<&'a str, Fail<'a>> {
    let Some(e) = s.chars().next() else {
        return fail("unterminated string", s);
    };
    let simple = match e {
        'b' => Some('\u{8}'),
        't' => Some('\t'),
        'n' => Some('\n'),
        'f' => Some('\u{c}'),
        'r' => Some('\r'),
        '"' => Some('"'),
        '\\' => Some('\\'),
        _ => None,
    };
    if let Some(c) = simple {
        out.push(c);
        return Ok(&s[1..]);
    }
    let len = match e {
        'u' => 4,
        'U' => 8,
        other => return fail(format!("invalid escape `\\{other}`"), s),
    };
    let hex = s
        .get(1..=len)
        .filter(|h| h.bytes().all(|b| b.is_ascii_hexdigit()));
    let code = hex
        .and_then(|h| u32::from_str_radix(h, 16).ok())
        .and_then(char::from_u32);
    let Some(code) = code else {
        return fail(format!("invalid unicode escape after `\\{e}`"), s);
    };
    out.push(code);
    Ok(&s[1 + len..])
}

/// A `#` comment up to the line end; nothing when `s` holds no comment.
fn comment(s: &str) -> Result<&str, Fail<'_>> {
    let Some(body) = s.strip_prefix('#') else {
        return Ok(s);
    };
    let end = body.find(['\n', '\r']).unwrap_or(body.len());
    for c in body[..end].chars() {
        allowed(c, body)?;
    }
    Ok(&body[end..])
}

/// End of input or one LF / CRLF line end; anything else is refused.
fn line_end(s: &str) -> Result<&str, Fail<'_>> {
    if s.is_empty() {
        return Ok(s);
    }
    match newline(s) {
        Some(rest) => Ok(rest),
        None if s.starts_with('\r') => fail("a lone carriage return", s),
        None => fail("unexpected text after the value", s),
    }
}

/// The rest after one LF or CRLF at the start of `s`.
fn newline(s: &str) -> Option<&str> {
    s.strip_prefix('\n').or_else(|| s.strip_prefix("\r\n"))
}

/// A newline right after a multi-line string's opening delimiter is dropped.
fn trim_first_newline(s: &str) -> &str {
    newline(s).unwrap_or(s)
}

/// Spaces and tabs, TOML's only whitespace.
fn skip_ws(s: &str) -> &str {
    s.trim_start_matches([' ', '\t'])
}

/// TOML 1.0 bars U+0000-U+001F (tab aside) and U+007F in strings and comments.
fn allowed(c: char, at: &str) -> Result<(), Fail<'_>> {
    if (c < ' ' && c != '\t') || c == '\u{7f}' {
        return fail(format!("control character {c:?}"), at);
    }
    Ok(())
}
