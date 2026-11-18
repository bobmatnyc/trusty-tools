//! Frame codec and refusal parity against the Python 0.7.0 captures in
//! `tests/fixtures/frames/`.

mod common;

use common::{items, json, read, s};
use trusty_browser::frame::{self, FrameError, MAX_FRAME};
use trusty_browser::pyjson::Json;
use trusty_browser::refusal::Refusal;

#[test]
fn every_captured_frame_reencodes_byte_identically() {
    let index = json("frames/index.json");
    assert_eq!(items(&index).len(), 68);
    for entry in items(&index) {
        let name = s(entry, "file");
        let raw = read(&format!("frames/{name}"));
        let message = frame::decode(&raw).unwrap_or_else(|e| panic!("{name}: {e}"));
        // Broker replies are Python `write_frame` (ensure_ascii); client frames
        // were sent with `ensure_ascii=False`, as the extension does.
        let ascii = s(entry, "direction") == "broker_to_client";
        let again = frame::encode_as(&message, ascii).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(again, raw, "{name}: Rust bytes differ from Python bytes");
        let streamed =
            frame::read_frame(&mut raw.as_slice()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(streamed, message, "{name}");
    }
}

#[test]
fn captured_refusal_replies_rebuild_byte_identically() {
    let mut checked = 0;
    let mut captured: Vec<Vec<u8>> = Vec::new();
    for entry in items(&json("frames/index.json")) {
        if s(entry, "direction") != "broker_to_client" {
            continue;
        }
        let name = s(entry, "file");
        let raw = read(&format!("frames/{name}"));
        let Ok(Json::Object(fields)) = frame::decode(&raw) else {
            panic!("{name}")
        };
        if fields.first().map(|(k, v)| (k.as_str(), v)) != Some(("ok", &Json::Bool(false))) {
            continue;
        }
        let code = fields
            .iter()
            .find(|(k, _)| k == "error")
            .and_then(|(_, v)| v.as_str());
        let mut refusal = Refusal::new(code.unwrap_or_else(|| panic!("{name}: no error")));
        for (k, v) in fields.iter().filter(|(k, _)| k != "ok" && k != "error") {
            refusal = refusal.with(k.clone(), v.clone());
        }
        let rebuilt = frame::encode(&refusal.response()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(rebuilt, raw, "{name}");
        captured.push(raw);
        checked += 1;
    }
    assert!(checked >= 15, "only {checked} refusal frames found");

    // Every refusal in refusals.json, rebuilt by Rust, is one of those captured byte strings.
    for case in items(&json("refusals.json")) {
        let reply = case.get("reply").unwrap_or_else(|| panic!("{case:?}"));
        let Json::Object(fields) = reply else {
            panic!("{case:?}")
        };
        if reply.get("ok") != Some(&Json::Bool(false)) {
            continue;
        }
        let mut refusal = Refusal::new(s(reply, "error"));
        for (k, v) in fields.iter().filter(|(k, _)| k != "ok" && k != "error") {
            refusal = refusal.with(k.clone(), v.clone());
        }
        let rebuilt = frame::encode(&refusal.response()).unwrap_or_default();
        assert!(captured.contains(&rebuilt), "{}", s(case, "case"));
    }
}

#[test]
fn select_without_request_id_capture_is_pinned() {
    // Phase 0 note: Python forwarded `select` with no request_id (it is not a
    // required field for select) and relayed the worker's empty result as
    // BrowserRefused. The behaviour lives in pages.py, so phase 3 ports it;
    // here only the captured bytes are pinned.
    assert_eq!(
        &read("frames/46-agent-select-no-request-id.bin")[4..],
        br#"{"op":"select","tab":7}"#
    );
    assert_eq!(
        frame::encode(&Refusal::new("BrowserRefused").response()).ok(),
        Some(read("frames/49-agent-select-no-request-id-reply.bin"))
    );
}

#[test]
fn frame_limits_and_shapes_are_refused() {
    let frame_of = |body: &[u8]| {
        let mut raw = u32::try_from(body.len())
            .unwrap_or(u32::MAX)
            .to_ne_bytes()
            .to_vec();
        raw.extend_from_slice(body);
        raw
    };
    assert!(
        matches!(frame::decode(&frame_of(b"")), Err(FrameError::Invalid)),
        "length 0"
    );
    assert!(
        matches!(frame::decode(&frame_of(b"[1]")), Err(FrameError::Invalid)),
        "array"
    );
    assert!(
        matches!(frame::decode(&frame_of(b"\"x\"")), Err(FrameError::Invalid)),
        "string"
    );
    assert!(matches!(
        frame::decode(&frame_of(b"{")),
        Err(FrameError::Malformed(_))
    ));
    let over = (MAX_FRAME as u32 + 1).to_ne_bytes();
    assert!(
        matches!(frame::body_len(over), Err(FrameError::Invalid)),
        "1 MiB + 1"
    );
    assert_eq!(
        frame::body_len((MAX_FRAME as u32).to_ne_bytes()).ok(),
        Some(MAX_FRAME)
    );

    let nan = Json::object([("x", Json::Float(f64::NAN))]);
    assert!(
        matches!(frame::encode(&nan), Err(FrameError::Invalid)),
        "allow_nan=False"
    );
    // {"x":"<pad>"} is 8 bytes of framing around the pad.
    let fits = Json::object([("x", Json::Str("a".repeat(MAX_FRAME - 8)))]);
    assert_eq!(
        frame::encode(&fits).map(|v| v.len()).ok(),
        Some(MAX_FRAME + 4)
    );
    let big = Json::object([("x", Json::Str("a".repeat(MAX_FRAME - 7)))]);
    assert!(
        matches!(frame::encode(&big), Err(FrameError::Invalid)),
        "body over 1 MiB"
    );
}
