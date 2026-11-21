//! Input limits and text helpers shared by every write op.
//!
//! Why: `datg.py` bounds every field through `_lim`, `_title` and `_short`, and
//! counts Unicode characters after Python's `strip`. Messages are part of the
//! contract (a rejected op prints them), so they are reproduced exactly.
//! What: [`lim`], [`title`], [`int_in`], [`short`], [`py_or`].
//! Test: `lim_matches_python_messages`, `short_truncates_with_an_ellipsis`.

use super::error::{IlResult, invalid};
use super::json::Json;
use crate::pystr::{collapse_space, strip};

/// Maximum title length.
pub const TITLE_MAX: usize = 120;

/// `str(v or "")`: the empty string for a missing or falsy value.
pub fn py_or(v: Option<&Json>) -> String {
    v.filter(|j| j.truthy())
        .map(Json::py_str)
        .unwrap_or_default()
}

/// `_lim`: `str(v).strip()` bounded to `mx` characters, one line unless `ml`, non-empty when `req`.
///
/// # Errors
/// `Invalid` with `datg.py`'s message.
pub fn lim(v: Option<&Json>, name: &str, mx: usize, req: bool, ml: bool) -> IlResult<String> {
    let s = match v {
        None | Some(Json::Null) => String::new(),
        Some(j) => strip(&j.py_str()).to_owned(),
    };
    if (!ml && s.contains('\n')) || s.chars().count() > mx || (req && s.is_empty()) {
        return invalid(format!(
            "{name} must be {}<= {mx} chars{}",
            if ml { "" } else { "one line, " },
            if req { ", non-empty" } else { "" }
        ));
    }
    Ok(s)
}

/// `_title`: a non-empty single line of at most [`TITLE_MAX`] characters.
///
/// # Errors
/// `Invalid` when empty, too long or multi-line.
pub fn title(v: Option<&Json>) -> IlResult<String> {
    let t = strip(&py_or(v)).to_owned();
    if t.is_empty() || t.chars().count() > TITLE_MAX || t.contains('\n') {
        return invalid(format!("title required, one line, <= {TITLE_MAX} chars"));
    }
    Ok(t)
}

/// `_int`: an integer (not a bool) within `lo..=hi`.
///
/// # Errors
/// `Invalid` when it is not.
pub fn int_in(v: Option<&Json>, name: &str, lo: i64, hi: i64) -> IlResult<i64> {
    match v {
        Some(Json::Int(n)) if (lo..=hi).contains(n) => Ok(*n),
        _ => invalid(format!("{name} must be an integer {lo}-{hi}")),
    }
}

/// `_short`: whitespace collapsed, cut to `n` characters with a trailing `…`.
pub fn short(v: &str, n: usize) -> String {
    let v = collapse_space(v);
    if v.chars().count() <= n {
        return v;
    }
    let mut out: String = v.chars().take(n.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lim_matches_python_messages() {
        let s = Json::from("  a  ");
        assert_eq!(lim(Some(&s), "x", 5, true, false).unwrap(), "a");
        let e = lim(Some(&Json::from("abcdef")), "answer", 5, false, true).unwrap_err();
        assert_eq!(e.to_string(), "answer must be <= 5 chars");
        let e = lim(None, "task", 5, true, false).unwrap_err();
        assert_eq!(
            e.to_string(),
            "task must be one line, <= 5 chars, non-empty"
        );
        assert!(lim(Some(&Json::from("a\nb")), "t", 9, false, false).is_err());
        assert_eq!(
            lim(Some(&Json::from("a\nb")), "t", 9, false, true).unwrap(),
            "a\nb"
        );
        assert!(title(Some(&Json::from("   "))).is_err());
    }

    #[test]
    fn short_truncates_with_an_ellipsis() {
        assert_eq!(short("a   b\nc", 10), "a b c");
        assert_eq!(short("abcdef", 4), "abc…");
        assert_eq!(short("éééé", 4), "éééé");
    }
}
