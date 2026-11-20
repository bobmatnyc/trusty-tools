//! Unit tests for `pages.rs`; expected values come from Python 0.7.0
//! (`pages.validate`, `page_fields.key_allowed`, `page_query.clicked`).

use super::*;

fn parse(text: &str) -> Json {
    Json::parse(text.as_bytes()).unwrap_or_else(|e| panic!("{text}: {e}"))
}

fn outcome(frame: &str) -> String {
    match validate(&parse(frame)) {
        Ok(op) => op.to_owned(),
        Err(r) => r.response().dumps(false).unwrap_or_default(),
    }
}

const BARE: &str = r#"{"ok":false,"error":"InvalidRequest"}"#;
const NAMED: &str = r#"{"ok":false,"error":"InvalidRequest","field":"request_id"}"#;

#[test]
fn validation_refuses_what_python_refuses() {
    let l = r#""lease":"L""#;
    let x200 = "x".repeat(200);
    let x201 = "x".repeat(201);
    let x8193 = "x".repeat(8193);
    let files10 = [r#"{"name":"a","type":"","data":"YQ=="}"#; 10].join(",");
    let cases: Vec<(String, &str)> = vec![
        (r#"{"op":"tabs"}"#.into(), "tabs"),
        (r#"{"op":"tabs","x":1}"#.into(), BARE),
        (r#"{"op":"select","tab":0}"#.into(), "select"),
        (r#"{"op":"select","tab":1.0}"#.into(), BARE),
        (
            format!(r#"{{"op":"page_read",{l},"max_chars":250000}}"#),
            "page_read",
        ),
        (
            format!(r#"{{"op":"page_read",{l},"max_chars":250001}}"#),
            BARE,
        ),
        (
            format!(r#"{{"op":"page_read",{l},"viewport_only":1}}"#),
            BARE,
        ),
        (format!(r#"{{"op":"page_read",{l},"scope_ref":""}}"#), BARE),
        (
            format!(r#"{{"op":"page_find",{l},"text":"{x200}"}}"#),
            "page_find",
        ),
        (format!(r#"{{"op":"page_find",{l},"text":"{x201}"}}"#), BARE),
        (
            format!(r#"{{"op":"page_find",{l},"role":"b","limit":0}}"#),
            BARE,
        ),
        (
            format!(r#"{{"op":"page_click",{l},"request_id":"r","ref":"e","dialog":"maybe"}}"#),
            BARE,
        ),
        (
            r#"{"op":"page_click","lease":"","request_id":"r","ref":"e"}"#.into(),
            BARE,
        ),
        (
            format!(r#"{{"op":"page_click",{l},"request_id":"","ref":5}}"#),
            NAMED,
        ),
        (format!(r#"{{"op":"page_click",{l},"ref":"e"}}"#), NAMED),
        (
            format!(
                r#"{{"op":"page_scroll",{l},"request_id":"r","to":"element","ref":"e","wait_ms":5000}}"#
            ),
            "page_scroll",
        ),
        (
            format!(
                r#"{{"op":"page_scroll",{l},"request_id":"r","to":"down","ref":"e","wait_ms":0}}"#
            ),
            BARE,
        ),
        (
            format!(
                r#"{{"op":"page_scroll",{l},"request_id":"r","to":"down","ref":null,"wait_ms":true}}"#
            ),
            BARE,
        ),
        (
            format!(
                r#"{{"op":"page_scroll",{l},"request_id":"r","to":"down","ref":null,"wait_ms":5001}}"#
            ),
            BARE,
        ),
        (
            format!(
                r#"{{"op":"page_press",{l},"request_id":"r","ref":"e","key":"é","modifiers":["Meta","Alt"]}}"#
            ),
            "page_press",
        ),
        (
            format!(
                r#"{{"op":"page_press",{l},"request_id":"r","ref":5,"key":"a","modifiers":[]}}"#
            ),
            BARE,
        ),
        (
            format!(
                r#"{{"op":"page_press",{l},"request_id":"r","ref":null,"key":"ab","modifiers":[]}}"#
            ),
            BARE,
        ),
        (
            format!(
                r#"{{"op":"page_press",{l},"request_id":"r","ref":null,"key":"\u200b","modifiers":[]}}"#
            ),
            BARE,
        ),
        (
            format!(r#"{{"op":"page_type",{l},"request_id":"r","ref":"e","text":"{x8193}"}}"#),
            BARE,
        ),
        (
            format!(r#"{{"op":"page_select",{l},"request_id":"r","ref":"e","value":""}}"#),
            "page_select",
        ),
        (
            format!(
                r#"{{"op":"page_upload",{l},"request_id":"r","ref":"e","files":[{{"name":"a/b","type":"","data":""}}]}}"#
            ),
            BARE,
        ),
        (
            format!(r#"{{"op":"page_upload",{l},"request_id":"r","ref":"e","files":[]}}"#),
            BARE,
        ),
        (
            format!(r#"{{"op":"page_upload",{l},"request_id":"r","ref":"e","files":[{files10}]}}"#),
            "page_upload",
        ),
        (
            format!(r#"{{"op":"page_navigate",{l},"request_id":"r","url":""}}"#),
            BARE,
        ),
        (
            format!(r#"{{"op":"page_navigate",{l},"request_id":"r","url":"http://127.0.0.1:1/"}}"#),
            "page_navigate",
        ),
        (
            format!(r#"{{"op":"page_drag",{l},"request_id":"r","ref":"e","target":""}}"#),
            BARE,
        ),
        (format!(r#"{{"op":"release",{l}}}"#), "release"),
        (format!(r#"{{"op":"switch",{l},"request_id":"r"}}"#), BARE),
        (r#"{"op":5}"#.into(), BARE),
        (r#"{"op":"nope"}"#.into(), BARE),
    ];
    for (frame, want) in &cases {
        assert_eq!(outcome(frame), *want, "{}", &frame[..frame.len().min(120)]);
    }
}

#[test]
fn key_allowed_matches_python_categories() {
    let cases = [
        ("a", true),
        ("é", true),
        (" ", true),
        ("Space", true),
        ("\t", false),
        ("\u{ad}", false),
        ("\u{378}", false),
        ("\u{1F600}", true),
        ("\u{3000}", false),
        ("\u{e000}", false),
        ("ab", false),
        ("", false),
    ];
    for (key, want) in cases {
        assert_eq!(key_allowed(&Json::from(key)), want, "{key:?}");
    }
    assert!(!key_allowed(&Json::Int(1)));
    assert_eq!(keycat().len(), 712, "table parsed whole");
}

#[test]
fn clicked_and_shaped_match_python() {
    let ok = [
        r#"{"document":"d"}"#,
        r#"{"document":"d","dialog":{"type":"alert","message":"","accepted":false}}"#,
    ];
    for raw in ok {
        assert_eq!(clicked(&parse(raw)), Ok(parse(raw)), "{raw}");
    }
    let refused = [
        r#"{"document":"d","dialog":{"type":"other","message":"","accepted":false}}"#,
        r#"{"document":"d","dialog":null}"#,
        r#"{"document":1}"#,
        r#"{"dialog":{}}"#,
    ];
    for raw in refused {
        assert_eq!(
            clicked(&parse(raw)),
            Err(Refusal::new("BrowserRefused")),
            "{raw}"
        );
    }
    // `bool` is not `int`, and a float total is allowed only where the shape says so.
    assert!(shaped(&parse(r#"{"window":true,"focused":true}"#), SWITCHED).is_err());
    assert!(shaped(&parse(r#"{"window":1,"focused":true,"x":1}"#), SWITCHED).is_err());
    assert!(shaped(&parse(r#"{"window":1,"focused":true}"#), SWITCHED).is_ok());
}

#[test]
fn journal_copy_and_equality_follow_python() {
    let request =
        parse(r#"{"op":"page_upload","files":[{"name":"a","type":"t","data":"aGk="}],"z":1}"#);
    let want = r#"{"op":"page_upload","files":[{"name":"a","type":"t","data":"96e3186af2815065fbc894de1bdadda7830527fffdba6bdd709789cf0ee79a6a"}],"z":1}"#;
    assert_eq!(
        remembered(&request).dumps(false).ok().as_deref(),
        Some(want)
    );
    // Python: {"a":1,"b":[1.0,True]} == {"b":[1,1],"a":True} is True.
    assert!(py_eq(
        &parse(r#"{"a":1,"b":[1.0,true]}"#),
        &parse(r#"{"b":[1,1],"a":true}"#)
    ));
    assert!(!py_eq(&parse(r#"{"a":"1"}"#), &parse(r#"{"a":1}"#)));
    assert!(!py_eq(&parse(r#"{"a":1}"#), &parse(r#"{"a":1,"b":null}"#)));
}
