//! Worker result shapes for [`crate::pages::shaped`] (`pages.py` result types).
//!
//! Why: split from `pages.rs` to keep it under the 500-SLOC cap (#11503).
//! What: [`Ty`], [`Shape`] and one constant per worker result; `pages`
//! re-exports them, so callers keep using `pages::READ` and the rest.
//! Test: `clicked_and_shaped_match_python` in `src/pages_tests.rs`;
//! `page_ops_replay_byte_identically` in `tests/parity_pages.rs`.

/// One Python type a shaped field may have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    /// `str`
    Str,
    /// `NoneType`
    Null,
    /// `bool`
    Bool,
    /// `int`
    Int,
    /// `float`
    Float,
    /// `list`
    List,
}

/// A result shape: each key and its allowed types.
pub type Shape = &'static [(&'static str, &'static [Ty])];

const OPT: &[Ty] = &[Ty::Str, Ty::Null];
/// `PROBE` (`select`).
pub const PROBE: Shape = &[
    ("access", &[Ty::Bool]),
    ("document", OPT),
    ("origin", OPT),
    ("title", OPT),
];
/// `READ` (`page_read`).
pub const READ: Shape = &[
    ("document", &[Ty::Str]),
    ("origin", &[Ty::Str]),
    ("url", &[Ty::Str]),
    ("title", &[Ty::Str]),
    ("text", &[Ty::Str]),
    ("elements", &[Ty::List]),
    ("truncated", &[Ty::Bool]),
];
/// `FOUND` (`page_find`).
pub const FOUND: Shape = &[
    ("document", &[Ty::Str]),
    ("origin", &[Ty::Str]),
    ("matches", &[Ty::List]),
    ("truncated", &[Ty::Bool]),
];
/// `DONE`.
pub const DONE: Shape = &[("document", &[Ty::Str])];
/// `CUT`: a `page_type`/`page_press` that a maxlength cut (#11352).
pub const CUT: Shape = &[
    ("document", &[Ty::Str]),
    ("truncated", &[Ty::Bool]),
    ("typed", &[Ty::Int]),
    ("accepted", &[Ty::Int]),
];
/// `SCROLLED`.
pub const SCROLLED: Shape = &[
    ("document", &[Ty::Str]),
    ("scroll_y", &[Ty::Int]),
    ("scroll_height", &[Ty::Int]),
    ("viewport_height", &[Ty::Int]),
    ("grew", &[Ty::Bool]),
    ("at_end", &[Ty::Bool]),
];
/// `SELECTED` (#11285).
pub const SELECTED: Shape = &[
    ("document", &[Ty::Str]),
    ("changed", &[Ty::Bool]),
    ("value", &[Ty::Str]),
];
/// `SWITCHED` (#11293).
pub const SWITCHED: Shape = &[("window", &[Ty::Int]), ("focused", &[Ty::Bool])];
/// `DRAGGED` (#11288).
pub const DRAGGED: Shape = &[("document", &[Ty::Str]), ("dropped", &[Ty::Bool])];
/// `LANDED` (navigate and history).
pub const LANDED: Shape = &[
    ("access", &[Ty::Bool]),
    ("document", OPT),
    ("origin", OPT),
    ("settled", &[Ty::Bool]),
];
/// `OPENED` (`page_open_tab`, #11208).
pub const OPENED: Shape = &[
    ("tab", &[Ty::Int]),
    ("window", &[Ty::Int]),
    ("access", &[Ty::Bool]),
    ("document", OPT),
    ("origin", OPT),
    ("title", OPT),
];
/// `CLOSED` (`page_close_tab`, #11208).
pub const CLOSED: Shape = &[("closed", &[Ty::Bool])];
/// `LISTED` (`page_downloads`).
pub const LISTED: Shape = &[("origin", &[Ty::Str]), ("downloads", &[Ty::List])];
/// `DOWNLOAD`: one entry of `LISTED`.
pub const DOWNLOAD: Shape = &[
    ("id", &[Ty::Int]),
    ("path", &[Ty::Str]),
    ("url", &[Ty::Str]),
    ("mime", &[Ty::Str]),
    ("state", &[Ty::Str]),
    ("bytes", &[Ty::Int, Ty::Float]),
    ("total", &[Ty::Int, Ty::Float]),
    ("error", OPT),
    ("started", &[Ty::Str]),
];
