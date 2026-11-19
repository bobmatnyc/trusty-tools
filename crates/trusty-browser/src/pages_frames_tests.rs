//! Unit tests for `pages_frames`: ref parsing and routing, frame order, offset
//! math and the snapshot text (#11362).

use super::*;

fn frame(
    id: &str,
    parent: Option<&str>,
    origin: &str,
    root: bool,
    owner: Option<[f64; 4]>,
) -> Frame {
    Frame {
        id: id.to_owned(),
        parent: parent.map(str::to_owned),
        loader: format!("L-{id}"),
        url: format!("{origin}/"),
        origin: origin.to_owned(),
        root,
        owner,
        refused: None,
        nodes: Vec::new(),
    }
}

/// The Omni layout: top F0 holds an in-process frame F1 and the out-of-process
/// embed T2, which holds an in-process frame T2a and a nested out-of-process T3.
fn omni() -> Vec<Frame> {
    vec![
        frame("F0", None, "https://app.test", false, None),
        frame(
            "T2",
            Some("F0"),
            "https://embed.test",
            true,
            Some([100.0, 200.0, 400.0, 300.0]),
        ),
        frame(
            "F1",
            Some("F0"),
            "https://app.test",
            false,
            Some([0.0, 0.0, 50.0, 50.0]),
        ),
        frame(
            "T3",
            Some("T2"),
            "https://deep.test",
            true,
            Some([10.0, 20.0, 200.0, 100.0]),
        ),
        // In T2's process: its owner box is already in T2's local root.
        frame(
            "T2a",
            Some("T2"),
            "https://embed.test",
            false,
            Some([5.0, 5.0, 20.0, 20.0]),
        ),
    ]
}

fn wire(v: &Json) -> String {
    v.dumps(false).unwrap_or_default()
}

#[test]
fn frame_refs_parse_and_route_only_within_their_snapshot() {
    assert_eq!(parse_ref("f0:12"), Some((0, 12)));
    assert_eq!(parse_ref("f199:999999999999"), Some((199, 999_999_999_999)));
    for bad in [
        "1.0",
        "f:1",
        "f1:",
        "f1:0",
        "f1000:1",
        "F1:2",
        "f1:2:3",
        "f1:-2",
        "f1:1234567890123",
    ] {
        assert_eq!(parse_ref(bad), None, "{bad}");
    }
    let tables = vec![FrameTable {
        lease: "L".into(),
        document: "d1".into(),
        frames: vec![
            ("F0".into(), "L0".into()),
            ("T2".into(), "L2".into()),
            ("T9".into(), String::new()),
        ],
    }];
    let routed = route_ref(&tables, "L", 7, Some("d1"), &Json::from("f1:41")).expect("routed");
    assert_eq!(wire(&routed), r#"{"frame":"T2","loader":"L2","node":41}"#);
    // A top-document ref and a null pass through unchanged.
    assert_eq!(
        route_ref(&tables, "L", 7, Some("d1"), &Json::from("3.4")),
        Ok(Json::from("3.4"))
    );
    assert_eq!(
        route_ref(&tables, "L", 7, Some("d1"), &Json::Null),
        Ok(Json::Null)
    );
    let stale = Err(Refusal::new("StaleElement").with("tab", 7));
    for (lease, document, r) in [
        ("L", Some("d1"), "f3:1"),  // no such ordinal
        ("L", Some("d1"), "f2:1"),  // a frame with no document loader
        ("M", Some("d1"), "f1:41"), // another lease's snapshot
        ("L", Some("d2"), "f1:41"), // the top document moved since the snapshot
        ("L", None, "f1:41"),
    ] {
        assert_eq!(
            route_ref(&tables, lease, 7, document, &Json::from(r)),
            stale,
            "{lease} {document:?} {r}"
        );
    }
}

#[test]
fn frames_order_depth_first_and_sum_offsets_across_processes() {
    let frames = omni();
    let ordered = order(&frames);
    let ids: Vec<(&str, Option<usize>)> = ordered
        .iter()
        .map(|&(i, p)| (frames[i].id.as_str(), p))
        .collect();
    assert_eq!(
        ids,
        [
            ("F0", None),
            ("T2", Some(0)),
            ("T3", Some(1)),
            ("T2a", Some(1)),
            ("F1", Some(0))
        ]
    );
    let boxes = rects(&frames, &ordered);
    assert_eq!(
        boxes,
        [
            None,
            Some([100.0, 200.0, 400.0, 300.0]),
            // T3's owner (10, 20) in T2's local root, which sits at (100, 200).
            Some([110.0, 220.0, 200.0, 100.0]),
            // An in-process frame of T2 is offset by T2's root too.
            Some([105.0, 205.0, 20.0, 20.0]),
            // An in-process frame of the top frame needs no offset.
            Some([0.0, 0.0, 50.0, 50.0]),
        ]
    );
    // An unreadable owner box leaves that frame, and the frames inside it, without a box.
    let mut blind = omni();
    blind[1].owner = None;
    let boxes = rects(&blind, &order(&blind));
    assert_eq!(boxes[1..4], [None, None, None]);
    // A frame whose parent is not listed hangs under the top frame; a repeated id is listed once.
    let mut odd = omni();
    odd.push(frame("X", Some("gone"), "https://x.test", true, None));
    odd.push(frame("T3", Some("T2"), "https://deep.test", true, None));
    let ids: Vec<&str> = order(&odd)
        .iter()
        .map(|&(i, _)| odd[i].id.as_str())
        .collect();
    assert_eq!(ids, ["F0", "T2", "T3", "T2a", "F1", "X"]);
}

#[test]
fn the_text_marks_cross_origin_canvas_and_refused_frames() {
    let mut frames = omni();
    frames[0].nodes = vec![Node {
        depth: 0,
        role: "button".into(),
        name: "Export".into(),
        backend: 4,
        value: None,
    }];
    frames[1].nodes = vec![
        Node {
            depth: 0,
            role: "textbox".into(),
            name: "Ask".into(),
            backend: 41,
            value: Some("why".into()),
        },
        Node {
            depth: 1,
            role: "StaticText".into(),
            name: "Revenue \"up\"".into(),
            backend: 44,
            value: None,
        },
    ];
    frames[3].nodes = vec![Node {
        depth: 0,
        role: "canvas".into(),
        name: "chart".into(),
        backend: 42,
        value: None,
    }];
    frames[4].refused = Some("PdfViewer".into());
    let (text, list, table) = assemble(&frames, "L", "d1");
    let want = concat!(
        "frame f0 https://app.test\n",
        "  button \"Export\" [f0:4]\n",
        "frame f1 https://embed.test (cross-origin, out-of-process) in f0 at 100,200 400x300\n",
        "  textbox \"Ask\" = \"why\" [f1:41]\n",
        "    text \"Revenue \\\"up\\\"\" [f1:44]\n",
        "frame f2 https://deep.test (cross-origin, out-of-process) in f1 at 110,220 200x100\n",
        "  canvas \"chart\" [f2:42] (canvas: pixels only; read it with page_screenshot, act with page_click_at)\n",
        "frame f3 https://embed.test (cross-origin) in f1 at 105,205 20x20 refused PdfViewer: read it with ",
        "page_screenshot, act with page_click_at and page_type_at\n",
        "frame f4 https://app.test in f0 at 0,0 50x50\n",
    );
    assert_eq!(text, want);
    let Json::Array(items) = &list else {
        panic!("list")
    };
    assert_eq!(
        wire(&items[2]),
        r#"{"frame":2,"parent":1,"origin":"https://deep.test","url":"https://deep.test/","cross_origin":true,"out_of_process":true,"rect":{"x":110.0,"y":220.0,"width":200.0,"height":100.0},"refused":null}"#
    );
    assert_eq!(table.frames[2], ("T3".to_owned(), "L-T3".to_owned()));
    assert_eq!((table.lease.as_str(), table.document.as_str()), ("L", "d1"));
}

#[test]
fn a_malformed_worker_snapshot_is_refused() {
    let good = r#"{"frame":"F0","parent":null,"loader":"L0","url":"https://a.test/","origin":"https://a.test","root":false,"owner":null,"refused":null,"nodes":[[0,"button","Go",4,null]]}"#;
    let child = r#"{"frame":"T2","parent":"F0","loader":"L2","url":"https://b.test/","origin":"https://b.test","root":true,"owner":[1,2,3.5,4],"refused":"FrameUnreachable","nodes":[]}"#;
    let parse = |frames: &str| {
        Json::parse(format!(r#"{{"frames":[{frames}]}}"#).as_bytes()).unwrap_or(Json::Null)
    };
    let ok = frames_of(&parse(&format!("{good},{child}"))).expect("frames");
    assert_eq!(ok[1].owner, Some([1.0, 2.0, 3.5, 4.0]));
    assert_eq!(ok[0].nodes[0].backend, 4);
    let refused = Err(Refusal::new("BrowserRefused"));
    for bad in [
        String::new(),            // no top frame
        format!("{good},{good}"), // two top frames
        good.replace(r#""nodes":"#, r#""extra":1,"nodes":"#),
        good.replace(r#"4,null]"#, r#"0,null]"#), // backend id 0
        good.replace(r#""Go""#, r#"7"#),          // a name that is not a string
        child.replace("FrameUnreachable", "Other"), // an unknown refusal
        child.replace("[1,2,3.5,4]", "[1,2,3]"),  // an owner box of three numbers
        good.replace(r#""L0""#, &format!("{:?}", "x".repeat(65))),
    ] {
        let frames = if bad.contains("\"T2\"") {
            format!("{good},{bad}")
        } else {
            bad.clone()
        };
        assert_eq!(
            frames_of(&parse(&frames)).map(|f| f.len()),
            refused.clone(),
            "{bad}"
        );
    }
    assert_eq!(frames_of(&Json::Null).map(|f| f.len()), refused);
}
