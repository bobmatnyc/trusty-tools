//! Tests for [`super::serve`] with an in-memory adapter: the handshake, the
//! request loop, faults, shutdown and EOF (#10746 S13).

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::*;
use crate::wire::write_frame;

/// An adapter that answers from fixed data, or never answers.
struct Scripted {
    hang: bool,
}

impl Adapter for Scripted {
    const KIND: &'static str = "gmail";

    async fn list(&mut self, req: &Request) -> Result<Page, Fault> {
        if self.hang {
            std::future::pending::<()>().await;
        }
        if req.selection.get("fail").is_some() {
            return Err(Fault::new("auth_required", "x".repeat(2000)));
        }
        Ok(Page {
            entries: vec![Entry {
                item_id: "m1".into(),
                group: Some("t1".into()),
                ..Entry::default()
            }],
            cursor: None,
            complete: true,
        })
    }

    async fn fetch(&mut self, req: &Request) -> Result<SourceRecord, Fault> {
        Ok(SourceRecord {
            item_id: req.item_id.clone().unwrap_or_default(),
            ..SourceRecord::default()
        })
    }
}

fn hello() -> Value {
    json!({"type": "hello", "role": "acquire", "protocol_min": 1, "protocol_max": 1,
           "host_version": "test"})
}

fn acquire(id: u64, op: &str, extra: Value) -> Value {
    let mut m = json!({"type": "acquire", "request_id": id, "op": op, "kind": "gmail",
                       "store": "s", "selection": {"account": "a"}});
    if let (Some(m), Value::Object(e)) = (m.as_object_mut(), extra) {
        m.extend(e);
    }
    m
}

/// A served adapter over two pipes: the test plays the host.
struct Session {
    to: Option<std::io::PipeWriter>,
    from: std::io::PipeReader,
    done: std::thread::JoinHandle<i32>,
}

impl Session {
    fn start(adapter: Scripted) -> Session {
        let (in_r, in_w) = std::io::pipe().unwrap();
        let (out_r, out_w) = std::io::pipe().unwrap();
        let done = std::thread::spawn(move || serve(adapter, in_r, out_w));
        Session {
            to: Some(in_w),
            from: out_r,
            done,
        }
    }

    fn send(&mut self, v: &Value) {
        write_frame(self.to.as_mut().unwrap(), v).unwrap();
    }

    /// Send `v` and read its one reply.
    fn ask(&mut self, v: &Value) -> Value {
        self.send(v);
        let f = crate::wire::read_frame(&mut self.from)
            .unwrap()
            .expect("a reply");
        serde_json::from_slice(&f).unwrap()
    }

    /// Close stdin and return the exit code.
    fn finish(mut self) -> i32 {
        self.to = None;
        self.done.join().unwrap()
    }
}

/// Serve `frames` one at a time, each with its reply, then EOF.
fn exchange(adapter: Scripted, frames: &[Value]) -> (i32, Vec<Value>) {
    let mut s = Session::start(adapter);
    let replies = frames.iter().map(|f| s.ask(f)).collect();
    (s.finish(), replies)
}

/// Why: the handshake names the kind, and every page sends `complete`.
/// What: hello, list, fetch, shutdown.
/// Test: self-contained.
#[test]
fn hello_list_fetch_and_shutdown() {
    let mut s = Session::start(Scripted { hang: false });
    let r: Vec<Value> = [
        hello(),
        acquire(1, "list", json!({"cursor": null})),
        acquire(2, "fetch", json!({"item_id": "m1"})),
    ]
    .iter()
    .map(|f| s.ask(f))
    .collect();
    s.send(&json!({"type": "shutdown"}));
    let code = s.done.join().unwrap();
    assert_eq!(code, EXIT_CLEAN);
    assert_eq!(
        (r[0]["type"].clone(), r[0]["kind"].clone()),
        (json!("hello"), json!("gmail"))
    );
    assert_eq!(r[1]["request_id"], 1);
    assert_eq!(r[1]["complete"], true);
    assert_eq!(r[1]["entries"][0]["group"], "t1");
    assert!(r[1]["entries"][0].get("deleted").is_none());
    assert_eq!(r[2]["record"]["item_id"], "m1");
}

/// Why: a failure is an error frame with a code and a bounded message,
/// and a bad request does not end the loop.
/// What: a fault, a wrong kind, a mistyped `operator_initiated`, an
/// unknown op; then a good request still answers.
/// Test: self-contained.
#[test]
fn faults_are_bounded_error_frames_and_the_loop_continues() {
    let (code, r) = exchange(
        Scripted { hang: false },
        &[
            hello(),
            acquire(1, "list", json!({"selection": {"fail": true}})),
            acquire(2, "list", json!({"kind": "gdocs"})),
            acquire(3, "list", json!({"operator_initiated": "yes"})),
            acquire(4, "remove", json!({})),
            acquire(5, "list", json!({})),
        ],
    );
    assert_eq!(code, EXIT_CLEAN);
    let codes: Vec<_> = r[1..5].iter().map(|v| v["code"].clone()).collect();
    assert_eq!(
        codes,
        vec![
            json!("auth_required"),
            json!("adapter_incompatible"),
            json!("protocol_error"),
            json!("protocol_error")
        ]
    );
    assert_eq!(
        r[1]["message"].as_str().unwrap().chars().count(),
        MAX_MESSAGE
    );
    assert_eq!(r[5]["type"], "listing");
}

/// Why: §2.15 r7.1 — an absent `operator_initiated` reads as false.
/// What: parse with and without the field.
/// Test: self-contained.
#[test]
fn absent_operator_initiated_reads_as_false() {
    let req = Request::parse(&acquire(1, "list", json!({}))).unwrap();
    assert!(!req.operator_initiated);
    let req = Request::parse(&acquire(1, "list", json!({"operator_initiated": true}))).unwrap();
    assert!(req.operator_initiated);
}

/// Why: the adapter speaks only protocol 1 in the acquire role, and the
/// first frame must be the hello.
/// What: a protocol range without 1, a worker-role hello, and a request
/// before any hello each end the adapter with exit 2.
/// Test: self-contained.
#[test]
fn a_bad_hello_ends_the_adapter() {
    for (first, code) in [
        (
            json!({"type": "hello", "role": "acquire", "protocol_min": 2, "protocol_max": 9}),
            "adapter_incompatible",
        ),
        (
            json!({"type": "hello", "protocol_min": 1, "protocol_max": 1}),
            "adapter_incompatible",
        ),
        (acquire(1, "list", json!({})), "protocol_error"),
    ] {
        let (exit, r) = exchange(Scripted { hang: false }, &[first]);
        assert_eq!(exit, EXIT_PROTOCOL);
        assert_eq!(r[0]["code"], code);
    }
    let (exit, r) = exchange(Scripted { hang: false }, &[]);
    assert_eq!((exit, r.len()), (EXIT_CLEAN, 0), "EOF before hello");
}

/// Why: a host that closes stdin while a request runs is gone; the adapter
/// must stop at once, not when the Google call returns.
/// What: a list that never completes, then EOF; serve returns 0 within 5 s.
/// Test: self-contained.
#[test]
fn eof_mid_request_ends_the_loop() {
    let mut s = Session::start(Scripted { hang: true });
    assert_eq!(s.ask(&hello())["type"], "hello");
    s.send(&acquire(1, "list", json!({})));
    std::thread::sleep(Duration::from_millis(100));
    let started = Instant::now();
    assert_eq!(s.finish(), EXIT_CLEAN);
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// Why: the host sends one request at a time; a second frame mid-request
/// is a protocol violation.
/// What: a hanging list followed by another request; exit 2.
/// Test: self-contained.
#[test]
fn a_frame_mid_request_is_a_protocol_error() {
    let mut s = Session::start(Scripted { hang: true });
    s.ask(&hello());
    s.send(&acquire(1, "list", json!({})));
    s.send(&acquire(2, "list", json!({})));
    assert_eq!(s.done.join().unwrap(), EXIT_PROTOCOL);
}
