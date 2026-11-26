//! `$HOME/.trusty-tools/install.toml`, read without a TOML dependency (#11821).
//!
//! Why: the leaf rule (#9171 A1) keeps `toml` out of this crate, yet the
//! Architect must read the same install file tm reads, and refuse it when it is
//! broken (Architect ruling D1af(1), option A).
//! What: [`read_install_file`] maps an absent file to `None` and parses the one
//! shape the install file has: `key = "string"` lines, `#` comments and blank
//! lines. Keys are bare or quoted; values are basic strings (with TOML's
//! escapes) or literal strings. Every other construct, an unknown key, a
//! duplicate key or a non-string value is a `Parse` error. That refuses a few
//! valid TOML spellings tm's writer never emits (multi-line strings, dotted
//! keys); refusing is the fail-closed side.
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
/// error; `Parse` on a line outside the supported shape. Values are not
/// validated here; the resolver validates the winning one.
/// Test: `absent_file_is_none`, `broken_files_are_refused`,
/// `valid_spellings_parse`.
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
    let mut file = InstallFile::default();
    for (n, line) in raw.lines().enumerate() {
        parse_line(line, &mut file).map_err(|e| format!("{e} (line {})", n + 1))?;
    }
    Ok(file)
}

/// One line: blank, a comment, or `key = value [# comment]`.
fn parse_line(line: &str, file: &mut InstallFile) -> Result<(), String> {
    let rest = line.trim_matches([' ', '\t', '\r']);
    if rest.is_empty() || rest.starts_with('#') {
        return Ok(());
    }
    let (key, rest) = parse_key(rest)?;
    let rest = rest.trim_start_matches([' ', '\t']);
    let rest = rest
        .strip_prefix('=')
        .ok_or_else(|| format!("expected `=` after key `{key}`"))?;
    let (value, rest) = parse_string(rest.trim_start_matches([' ', '\t']))?;
    let rest = rest.trim_matches([' ', '\t', '\r']);
    if !(rest.is_empty() || rest.starts_with('#')) {
        return Err(format!("unexpected text after the value of `{key}`"));
    }
    let slot = match key.as_str() {
        "projects_root" => &mut file.projects_root,
        "config_dir" => &mut file.config_dir,
        "claude_config_dir" => &mut file.claude_config_dir,
        "codex_home" => &mut file.codex_home,
        other => return Err(format!("unknown key `{other}`")),
    };
    if slot.is_some() {
        return Err(format!("duplicate key `{key}`"));
    }
    *slot = Some(PathBuf::from(value));
    Ok(())
}

/// A bare key (`A-Z a-z 0-9 _ -`) or a quoted one; returns it and the rest.
fn parse_key(s: &str) -> Result<(String, &str), String> {
    if s.starts_with(['"', '\'']) {
        return parse_string(s);
    }
    let end = s
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(s.len());
    if end == 0 {
        return Err("expected a key".to_owned());
    }
    Ok((s[..end].to_owned(), &s[end..]))
}

/// A one-line basic (`"…"`) or literal (`'…'`) string; returns it and the rest.
fn parse_string(s: &str) -> Result<(String, &str), String> {
    if s.starts_with("\"\"\"") || s.starts_with("'''") {
        return Err("multi-line strings are not supported".to_owned());
    }
    let mut chars = s.char_indices();
    match chars.next() {
        Some((_, '\'')) => {
            for (i, c) in chars {
                if c == '\'' {
                    return Ok((s[1..i].to_owned(), &s[i + 1..]));
                }
                control_check(c)?;
            }
            Err("unterminated string".to_owned())
        }
        Some((_, '"')) => basic_string(s),
        _ => Err("expected a string value".to_owned()),
    }
}

/// The body of a basic string after its opening `"`, with escapes decoded.
fn basic_string(s: &str) -> Result<(String, &str), String> {
    let mut out = String::new();
    let mut chars = s.char_indices().skip(1);
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Ok((out, &s[i + 1..])),
            '\\' => {
                let (_, e) = chars.next().ok_or("unterminated string")?;
                match e {
                    'b' => out.push('\u{8}'),
                    't' => out.push('\t'),
                    'n' => out.push('\n'),
                    'f' => out.push('\u{c}'),
                    'r' => out.push('\r'),
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    'u' | 'U' => {
                        let len = if e == 'u' { 4 } else { 8 };
                        let hex: String = chars.by_ref().take(len).map(|(_, h)| h).collect();
                        let code = (hex.len() == len)
                            .then(|| u32::from_str_radix(&hex, 16).ok())
                            .flatten()
                            .and_then(char::from_u32)
                            .ok_or_else(|| format!("invalid unicode escape `\\{e}{hex}`"))?;
                        out.push(code);
                    }
                    other => return Err(format!("invalid escape `\\{other}`")),
                }
            }
            other => {
                control_check(other)?;
                out.push(other);
            }
        }
    }
    Err("unterminated string".to_owned())
}

/// TOML forbids control characters other than tab inside a one-line string.
fn control_check(c: char) -> Result<(), String> {
    if c != '\t' && c.is_control() {
        return Err(format!("control character {c:?} in a string"));
    }
    Ok(())
}
