//! Refusals: the `{"ok":false,"error":<name>,...}` replies of every socket.
//!
//! Why: clients branch on the error name and on the few detail fields the
//! contract names (`protocol.py` `Refusal`, `PAGE_ERRORS`, `REFUSAL_FIELDS`).
//! What: [`Refusal`] keeps the name and ordered details; the constant sets
//! mirror Python's, and [`request_string`]/[`need_request_id`] port the
//! request-field checks that name `request_id` (#11239, Architect ar-T6s).
//! Test: `captured_refusal_replies_rebuild_byte_identically` in
//! `tests/parity_frames.rs`, plus the unit tests below.

use crate::pyjson::Json;

/// One refusal; `response()` is the reply frame.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{code}")]
pub struct Refusal {
    /// Error name, e.g. `InvalidRequest`.
    pub code: String,
    /// Detail fields, in the order they were added.
    pub details: Vec<(String, Json)>,
}

impl Refusal {
    /// A bare refusal.
    pub fn new(code: impl Into<String>) -> Refusal {
        Refusal {
            code: code.into(),
            details: Vec::new(),
        }
    }

    /// Add one detail field.
    #[must_use]
    pub fn with(mut self, key: impl Into<String>, value: impl Into<Json>) -> Refusal {
        self.details.push((key.into(), value.into()));
        self
    }

    /// `{"ok": false, "error": code, **details}`.
    pub fn response(&self) -> Json {
        let mut out = Json::object([
            ("ok", Json::Bool(false)),
            ("error", Json::from(&*self.code)),
        ]);
        for (k, v) in &self.details {
            out.insert(k.clone(), v.clone());
        }
        out
    }
}

/// Page ops whose lost reply is `OutcomeUnknown` (they may have run).
pub const MUTATING: &[&str] = &[
    "page_back",
    "page_forward",
    "page_reload",
    "page_click",
    "page_hover",
    "page_type",
    "page_press",
    "page_select",
    "page_scroll",
    "page_navigate",
    "page_upload",
    "page_drag",
];

/// The only worker refusal fields relayed to the agent, in relay order.
pub const REFUSAL_FIELDS: &[&str] = &[
    "tab", "origin", "access", "document", "typed", "accepted", "entered",
];

/// Worker names for a command it could not finish; relayed without fields.
pub const UNFINISHED: &[&str] = &["OutcomeUnknown", "BrowserDisconnected"];

/// Worker error names relayed as-is; any other name becomes `BrowserRefused`.
pub const PAGE_ERRORS: &[&str] = &[
    "OutcomeUnknown",
    "BrowserDisconnected",
    "InvalidOperation",
    "InvalidRequest",
    "ProfileMismatch",
    "UnknownGrant",
    "OutOfScope",
    "TabNotFound",
    "PermissionNeeded",
    "DocumentChanged",
    "NavigationPending",
    "StaleElement",
    "NotEditable",
    "TextDropped",
    "NotSelectable",
    "NoSuchOption",
    "Obscured",
    "DebuggerBusy",
    "NoHistory",
    "NotDraggable",
    "InvalidUrl",
];

/// Longest request string field, in code points.
pub const MAX_STRING: usize = 8192;

/// A required non-empty string field of at most 8192 code points.
///
/// Why: one check for every string field. What: anything else is
/// `InvalidRequest`; for `request_id` the refusal names the field, others stay bare.
/// Test: `request_id_refusals_name_the_field_and_others_stay_bare`.
pub fn request_string<'a>(message: &'a Json, name: &str) -> Result<&'a str, Refusal> {
    match message.get(name).and_then(Json::as_str) {
        Some(s) if !s.is_empty() && s.chars().count() <= MAX_STRING => Ok(s),
        _ if name == "request_id" => Err(Refusal::new("InvalidRequest").with("field", name)),
        _ => Err(Refusal::new("InvalidRequest")),
    }
}

/// Refuse a frame that omits a required `request_id`, naming the field.
pub fn need_request_id(request: &Json, required: &[&str]) -> Result<(), Refusal> {
    if required.contains(&"request_id") && request.get("request_id").is_none() {
        return Err(Refusal::new("InvalidRequest").with("field", "request_id"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(r: &Refusal) -> String {
        r.response().dumps(true).unwrap_or_default()
    }

    #[test]
    fn request_id_refusals_name_the_field_and_others_stay_bare() {
        let named = r#"{"ok":false,"error":"InvalidRequest","field":"request_id"}"#;
        for frame in [r#"{}"#, r#"{"request_id":""}"#, r#"{"request_id":7}"#] {
            let msg = Json::parse(frame.as_bytes()).unwrap_or(Json::Null);
            let err = request_string(&msg, "request_id").err();
            assert_eq!(err.as_ref().map(wire).as_deref(), Some(named), "{frame}");
        }
        let long = Json::object([("op", Json::Str("x".repeat(MAX_STRING + 1)))]);
        let bare = request_string(&long, "op").err();
        assert_eq!(
            bare.as_ref().map(wire).as_deref(),
            Some(r#"{"ok":false,"error":"InvalidRequest"}"#)
        );
        let ok = Json::object([("op", Json::Str("é".repeat(MAX_STRING)))]);
        assert!(
            request_string(&ok, "op").is_ok(),
            "limit counts code points"
        );

        let missing = need_request_id(&Json::object::<&str>([]), &["request_id"]).err();
        assert_eq!(missing.as_ref().map(wire).as_deref(), Some(named));
        assert!(need_request_id(&Json::object::<&str>([]), &["tab"]).is_ok());
    }
}
