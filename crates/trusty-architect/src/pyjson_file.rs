//! `json.load(open(path, encoding="utf-8"))` with Python's error text (#12028 P1f).
//!
//! Why: `il_agent.py` prints the exception a failed state read raised
//! (`state.json malformed ({e})`), so the port must name the same error in
//! the same words.
//! What: [`load`] reads the file as a text-mode open does (UTF-8, universal
//! newlines), decodes it with [`crate::pyjson_load::loads`] for the error and
//! with [`Json::parse`] for the value. [`LoadError`] keeps `FileNotFoundError`
//! apart, because `datg.load_state` words it differently.
//! Test: `load_reports_python_error_text`.

use std::path::Path;

use crate::il::json::Json;

/// Why a JSON file could not be loaded; the text is Python's `str(e)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LoadError {
    /// `FileNotFoundError`.
    NotFound(String),
    /// Any other `OSError`, a `UnicodeDecodeError` or a `JSONDecodeError`.
    Other(String),
}

impl LoadError {
    /// `str(e)`.
    #[cfg(test)]
    pub(crate) fn text(&self) -> &str {
        match self {
            LoadError::NotFound(s) | LoadError::Other(s) => s,
        }
    }
}

/// `str(UnicodeDecodeError)` for bytes that are not UTF-8.
pub(crate) fn decode_error(bytes: &[u8], e: &std::str::Utf8Error) -> String {
    let at = e.valid_up_to();
    let b = bytes.get(at).copied().unwrap_or(0);
    let why = match e.error_len() {
        None => "unexpected end of data",
        Some(_) if (0x80..=0xc1).contains(&b) || b >= 0xf5 => "invalid start byte",
        Some(_) => "invalid continuation byte",
    };
    format!("'utf-8' codec can't decode byte 0x{b:02x} in position {at}: {why}")
}

/// Loads the JSON document at `path` as `json.load` on a text-mode open does.
pub(crate) fn load(path: &Path) -> Result<Json, LoadError> {
    let text = read_text(path)?;
    crate::pyjson_load::loads(&text).map_err(|e| LoadError::Other(e.py_str()))?;
    Json::parse(&text).map_err(LoadError::Other)
}

/// The text a `open(path, encoding="utf-8").read()` returns (UTF-8,
/// universal newlines), or Python's error text.
// See #12029: `dashboard_schema.load_schema` reads its files the same way.
pub(crate) fn read_text(path: &Path) -> Result<String, LoadError> {
    let shown = path.to_string_lossy();
    let bytes = std::fs::read(path).map_err(|e| {
        let text = crate::writers::errno_text(&e, &shown);
        match e.kind() {
            std::io::ErrorKind::NotFound => LoadError::NotFound(text),
            _ => LoadError::Other(text),
        }
    })?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|e| LoadError::Other(decode_error(&bytes, &e)))?
        .to_owned();
    Ok(crate::writers::universal_newlines(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_reports_python_error_text() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("state.json");
        assert!(
            matches!(load(&p), Err(LoadError::NotFound(t)) if t.starts_with("[Errno 2] No such file or directory: '"))
        );
        std::fs::write(&p, "").unwrap();
        assert_eq!(
            load(&p).unwrap_err().text(),
            "Expecting value: line 1 column 1 (char 0)"
        );
        std::fs::write(&p, b"\xff").unwrap();
        assert_eq!(
            load(&p).unwrap_err().text(),
            "'utf-8' codec can't decode byte 0xff in position 0: invalid start byte"
        );
        std::fs::write(&p, "{\"a\": [1]}").unwrap();
        assert_eq!(load(&p).unwrap().dump(), "{\"a\":[1]}");
    }
}
