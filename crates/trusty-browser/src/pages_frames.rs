//! Cross-origin child frames in a `page_read` / `page_find` result (#11301).
//!
//! Why: a read may now carry content from cross-origin child iframes the
//! extension holds host permission for. The broker must still refuse any
//! result field it does not know, and the audit must name the child origins
//! that were read.
//! What: [`framed`] checks a `READ`/`FOUND` result plus the optional
//! `frames` table and `denied_frames` list; [`child_hosts`] lists the URL
//! hosts of the child frames a result read, for the `frames` audit field.
//! Test: `a_result_with_frames_passes_and_an_unknown_field_still_refuses`,
//! `child_hosts_names_each_child_frame_once` below.

use crate::pages::{self, Shape, Ty};
use crate::pages_url::host;
use crate::pyjson::Json;
use crate::refusal::Refusal;

/// One `frames` entry: the top document is `id` 0.
pub const FRAME: Shape = &[
    ("id", &[Ty::Int]),
    ("origin", &[Ty::Str]),
    ("url", &[Ty::Str]),
    ("document", &[Ty::Str]),
];
/// One `denied_frames` entry; it never carries child content.
pub const DENIED: Shape = &[("origin", &[Ty::Str]), ("reason", &[Ty::Str])];
/// Reason codes a denied frame may carry (`docs/activation-codes.md`).
pub const REASONS: &[&str] = &["no_permission", "opaque", "frame_limit", "navigated"];
/// The worker reports the top frame and at most eight child frames.
pub const MAX_FRAMES: usize = 9;
/// At most this many denied origins.
pub const MAX_DENIED: usize = 20;

fn refused() -> Refusal {
    Refusal::new("BrowserRefused")
}

fn entries(value: Option<&Json>, max: usize) -> Result<&[Json], Refusal> {
    match value {
        Some(Json::Array(items)) if items.len() <= max => Ok(items),
        _ => Err(refused()),
    }
}

/// Why: the frame fields are optional, so an old worker's result still
/// passes byte for byte.
/// What: `value` as `shape` when it has neither frame key; else both keys,
/// each entry exactly [`FRAME`] or [`DENIED`] (a known reason), a first frame
/// with `id` 0, and the rest of `value` exactly `shape`. Anything else is
/// `BrowserRefused`.
/// Test: `a_result_with_frames_passes_and_an_unknown_field_still_refuses`.
pub fn framed(value: &Json, shape: Shape) -> Result<Json, Refusal> {
    let Json::Object(pairs) = value else {
        return Err(refused());
    };
    let is_frame_key = |k: &str| k == "frames" || k == "denied_frames";
    if !pairs.iter().any(|(k, _)| is_frame_key(k)) {
        return pages::shaped(value, shape);
    }
    let rest = Json::Object(
        pairs
            .iter()
            .filter(|(k, _)| !is_frame_key(k))
            .cloned()
            .collect(),
    );
    pages::shaped(&rest, shape)?;
    let frames = entries(value.get("frames"), MAX_FRAMES)?;
    let denied = entries(value.get("denied_frames"), MAX_DENIED)?;
    for frame in frames {
        pages::shaped(frame, FRAME)?;
    }
    if frames.first().and_then(|f| f.get("id")) != Some(&Json::Int(0)) {
        return Err(refused());
    }
    for entry in denied {
        pages::shaped(entry, DENIED)?;
        let reason = entry.get("reason").and_then(Json::as_str);
        if !reason.is_some_and(|r| REASONS.contains(&r)) {
            return Err(refused());
        }
    }
    Ok(value.clone())
}

/// The URL hosts of the child frames (`id` other than 0) a result read, each
/// once, in table order; `None` when it read none.
pub fn child_hosts(result: &Json) -> Option<Json> {
    let Some(Json::Array(frames)) = result.get("frames") else {
        return None;
    };
    let mut hosts: Vec<Json> = Vec::new();
    for frame in frames {
        if frame.get("id") == Some(&Json::Int(0)) {
            continue;
        }
        let origin = frame.get("origin").and_then(Json::as_str);
        if let Ok(Some(h)) = host(origin) {
            let h = Json::from(h);
            if !hosts.contains(&h) {
                hosts.push(h);
            }
        }
    }
    (!hosts.is_empty()).then_some(Json::Array(hosts))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Json {
        Json::parse(text.as_bytes()).unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    const READ: &str = r#""document":"D","origin":"https://a.test","url":"https://a.test/","title":"T","text":"","elements":[{"ref":"1.0","frame":0},{"ref":"7:2.0","frame":7}],"truncated":false"#;
    const FRAMES: &str = r#""frames":[{"id":0,"origin":"https://a.test","url":"https://a.test/","document":"D"},{"id":7,"origin":"https://b.test:8443","url":"https://b.test:8443/child","document":"C"}]"#;

    #[test]
    fn a_result_with_frames_passes_and_an_unknown_field_still_refuses() {
        let denied = r#""denied_frames":[{"origin":"https://c.test","reason":"no_permission"}]"#;
        let ok = [
            format!("{{{READ}}}"),
            format!("{{{READ},{FRAMES},{denied}}}"),
            format!(r#"{{{READ},{FRAMES},"denied_frames":[]}}"#),
        ];
        for text in &ok {
            assert!(framed(&parse(text), pages::READ).is_ok(), "{text}");
        }
        let bad = [
            format!(r#"{{{READ},"extra":1}}"#),
            format!(r#"{{{READ},{FRAMES},{denied},"extra":1}}"#),
            format!("{{{READ},{FRAMES}}}"),
            format!("{{{READ},{denied}}}"),
            format!(
                r#"{{{READ},{FRAMES},"denied_frames":[{{"origin":"https://c.test","reason":"why"}}]}}"#
            ),
            format!(
                r#"{{{READ},{FRAMES},"denied_frames":[{{"origin":"https://c.test","reason":"opaque","text":"x"}}]}}"#
            ),
            format!(
                r#"{{{READ},"frames":[{{"id":7,"origin":"o","url":"u","document":"C"}}],"denied_frames":[]}}"#
            ),
            format!(
                r#"{{{READ},"frames":[{{"id":0,"origin":"o","url":"u","document":"C","text":"x"}}],"denied_frames":[]}}"#
            ),
        ];
        for text in &bad {
            assert!(framed(&parse(text), pages::READ).is_err(), "{text}");
        }
        let found = r#"{"document":"D","origin":"https://a.test","matches":[],"truncated":false,"frames":[{"id":0,"origin":"o","url":"u","document":"D"}],"denied_frames":[]}"#;
        assert!(framed(&parse(found), pages::FOUND).is_ok());
    }

    #[test]
    fn child_hosts_names_each_child_frame_once() {
        let twice = r#"{"id":9,"origin":"https://b.test:8443","url":"u","document":"E"}"#;
        let text = format!(r#"{{{READ},"frames":[{FRAMES_INNER},{twice}],"denied_frames":[]}}"#);
        assert_eq!(
            child_hosts(&parse(&text)),
            Some(Json::Array(vec![Json::from("b.test")]))
        );
        assert_eq!(child_hosts(&parse(&format!("{{{READ}}}"))), None);
    }

    const FRAMES_INNER: &str = r#"{"id":0,"origin":"https://a.test","url":"https://a.test/","document":"D"},{"id":7,"origin":"https://b.test:8443","url":"https://b.test:8443/child","document":"C"}"#;
}
