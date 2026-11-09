//! The adapter side of the acquire protocol (SPEC-OKF-01 Appendix B
//! "Acquire role", §2.15).
//!
//! Why: every adapter kind speaks the same handshake and request loop, and
//! each must stop the moment its host goes away: the host closes stdin on
//! shutdown and kills the process group on cancel, and an adapter blocked in
//! a Google call must not outlive a host that closed its end.
//! What: [`serve`] runs one adapter over a frame stream. A reader thread
//! turns stdin into frames. The first frame must be the host's `hello` in
//! the `acquire` role with a protocol range that holds
//! [`ADAPTER_PROTOCOL`]; the reply names the kind. Each later `acquire`
//! request runs `list` or `fetch` while the loop also watches stdin: EOF
//! ends the process at once with exit 0, and any other frame mid-request is
//! a protocol error (exit 2). `shutdown` exits 0. A failed request is an
//! `error` frame with a code and a bounded message, never an empty success.
//! Test: `server::tests`, `stdin_eof_mid_request_ends_the_adapter`
//! (`tests/conformance.rs`).

use std::future::Future;
use std::io::{Read, Write};

use serde::Serialize;
use serde_json::{Map, Value, json};
use trusty_okf::import::record::SourceRecord;

use crate::wire::{ADAPTER_PROTOCOL, read_frame, write_frame};

/// The `adapter_version` the hello reply carries.
pub const ADAPTER_VERSION: &str = concat!("trusty-connectors/", env!("CARGO_PKG_VERSION"));

/// Exit code: shutdown or stdin EOF.
pub const EXIT_CLEAN: i32 = 0;
/// Exit code: the adapter could not start (bad arguments, no runtime).
pub const EXIT_START: i32 = 1;
/// Exit code: a protocol violation by the host, or a torn stream.
pub const EXIT_PROTOCOL: i32 = 2;
/// Exit code: stdout closed or a reply could not be written.
pub const EXIT_OUTPUT: i32 = 3;

/// The longest error message an `error` frame carries.
const MAX_MESSAGE: usize = 512;

/// A failed request: the `error` frame's code and message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fault {
    /// A protocol error code (`policy_refused`, `auth_required`, ...).
    pub code: String,
    /// A bounded message with no content excerpt.
    pub message: String,
}

impl Fault {
    /// A fault with `code` and `message`.
    pub fn new(code: &str, message: impl Into<String>) -> Fault {
        Fault {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

/// One host `acquire` request.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// The ID the reply must echo.
    pub request_id: u64,
    /// `list` or `fetch`.
    pub op: String,
    /// The store kind (`gmail`, `gdocs`).
    pub kind: String,
    /// The store ID.
    pub store: String,
    /// The store selection (the registry row's parameters).
    pub selection: Map<String, Value>,
    /// Whether an operator started the run; absent reads as false (§2.15).
    pub operator_initiated: bool,
    /// The page cursor of a `list`.
    pub cursor: Option<String>,
    /// The item of a `fetch`.
    pub item_id: Option<String>,
}

impl Request {
    /// Parse an `acquire` frame. Errors: a missing or mistyped field.
    pub fn parse(msg: &Value) -> Result<Request, Fault> {
        let bad = |what: &str| Fault::new("protocol_error", format!("acquire request: {what}"));
        let text = |k: &str| msg.get(k).and_then(Value::as_str).map(str::to_string);
        let opt_text = |k: &str| match msg.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(bad(&format!("{k} is not a string"))),
        };
        // #10746 S13: §2.15 r7.1 — an absent flag is a run no operator
        // started; anything but a boolean is refused, never guessed.
        let operator_initiated = match msg.get("operator_initiated") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err(bad("operator_initiated is not a boolean")),
        };
        Ok(Request {
            request_id: msg
                .get("request_id")
                .and_then(Value::as_u64)
                .ok_or_else(|| bad("no request_id"))?,
            op: text("op").ok_or_else(|| bad("no op"))?,
            kind: text("kind").ok_or_else(|| bad("no kind"))?,
            store: text("store").ok_or_else(|| bad("no store"))?,
            selection: match msg.get("selection") {
                Some(Value::Object(m)) => m.clone(),
                _ => return Err(bad("selection is not an object")),
            },
            operator_initiated,
            cursor: opt_text("cursor")?,
            item_id: opt_text("item_id")?,
        })
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One listed item on the wire (the host's `WireEntry`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Entry {
    /// The immutable source ID.
    pub item_id: String,
    /// The thread of a message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// The source revision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// The source timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    /// A skip reason the host reports instead of fetching.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip: Option<String>,
    /// The source marks the item deleted.
    #[serde(skip_serializing_if = "is_false")]
    pub deleted: bool,
}

/// One page of a listing. `complete` is always sent explicitly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Page {
    /// The page's entries.
    pub entries: Vec<Entry>,
    /// The next page's cursor; `None` on the final page.
    pub cursor: Option<String>,
    /// False when the adapter cannot show the listing is whole.
    pub complete: bool,
}

/// One adapter kind.
pub(crate) trait Adapter {
    /// The store kind it serves.
    const KIND: &'static str;
    /// One page of the listing from `req.cursor`.
    fn list(&mut self, req: &Request) -> impl Future<Output = Result<Page, Fault>>;
    /// The record of `req.item_id`.
    fn fetch(&mut self, req: &Request) -> impl Future<Output = Result<SourceRecord, Fault>>;
}

fn bounded(s: &str) -> String {
    s.chars().take(MAX_MESSAGE).collect()
}

fn error_frame(id: u64, f: &Fault) -> Value {
    json!({"type": "error", "request_id": id, "code": f.code, "message": bounded(&f.message)})
}

/// Whether the host's `hello` asks for the acquire role over a protocol
/// range that holds [`ADAPTER_PROTOCOL`].
fn hello_ok(msg: &Value) -> bool {
    let n = |k: &str| msg.get(k).and_then(Value::as_u64);
    msg["type"] == "hello"
        && msg["role"] == "acquire"
        && matches!((n("protocol_min"), n("protocol_max")),
            (Some(lo), Some(hi)) if lo <= ADAPTER_PROTOCOL && ADAPTER_PROTOCOL <= hi)
}

/// Run `adapter` over `input` and `out` until shutdown, EOF or a protocol
/// violation; returns the process exit code.
pub(crate) fn serve<A: Adapter>(
    mut adapter: A,
    input: impl Read + Send + 'static,
    mut out: impl Write,
) -> i32 {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return EXIT_START;
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    // The reader thread ends with the stream; dropping `tx` closes the
    // channel, which the loop reads as EOF.
    std::thread::spawn(move || {
        let mut input = input;
        loop {
            match read_frame(&mut input) {
                Ok(Some(frame)) => {
                    if tx.send(Ok(frame)).is_err() {
                        return;
                    }
                }
                Ok(None) => return,
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            }
        }
    });
    rt.block_on(async move {
        let parse = |frame: Vec<u8>| serde_json::from_slice::<Value>(&frame).ok();
        let hello = match rx.recv().await {
            None => return EXIT_CLEAN,
            Some(Err(_)) => return EXIT_PROTOCOL,
            Some(Ok(frame)) => parse(frame).unwrap_or(Value::Null),
        };
        if !hello_ok(&hello) {
            let code = if hello["type"] == "hello" {
                "adapter_incompatible"
            } else {
                "protocol_error"
            };
            let _ = write_frame(&mut out, &error_frame(0, &Fault::new(code, "bad hello")));
            return EXIT_PROTOCOL;
        }
        let reply = json!({"type": "hello", "protocol": ADAPTER_PROTOCOL,
                           "adapter_version": ADAPTER_VERSION, "kind": A::KIND});
        if write_frame(&mut out, &reply).is_err() {
            return EXIT_OUTPUT;
        }
        loop {
            let msg = match rx.recv().await {
                None => return EXIT_CLEAN,
                Some(Err(_)) => return EXIT_PROTOCOL,
                Some(Ok(frame)) => match parse(frame) {
                    Some(m) => m,
                    None => return EXIT_PROTOCOL,
                },
            };
            match msg["type"].as_str() {
                Some("shutdown") => return EXIT_CLEAN,
                Some("acquire") => {}
                _ => return EXIT_PROTOCOL,
            }
            let id = msg["request_id"].as_u64().unwrap_or(0);
            let reply = match Request::parse(&msg) {
                Err(f) => error_frame(id, &f),
                Ok(req) if req.kind != A::KIND => error_frame(
                    id,
                    &Fault::new(
                        "adapter_incompatible",
                        format!("this adapter serves {}, not {}", A::KIND, req.kind),
                    ),
                ),
                Ok(req) => {
                    let work = async {
                        match req.op.as_str() {
                            "list" => adapter.list(&req).await.map(|p| {
                                json!({"type": "listing", "request_id": id,
                                       "entries": p.entries, "cursor": p.cursor,
                                       "complete": p.complete})
                            }),
                            "fetch" => adapter
                                .fetch(&req)
                                .await
                                .map(|r| json!({"type": "item", "request_id": id, "record": r})),
                            other => {
                                Err(Fault::new("protocol_error", format!("unknown op {other}")))
                            }
                        }
                    };
                    // #10746 S13: the host sends one request at a time, so
                    // a stdin event mid-request is EOF (the host is gone:
                    // stop now) or a protocol violation.
                    tokio::select! {
                        done = work => done.unwrap_or_else(|f| error_frame(id, &f)),
                        next = rx.recv() => return match next {
                            None => EXIT_CLEAN,
                            Some(_) => EXIT_PROTOCOL,
                        },
                    }
                }
            };
            if write_frame(&mut out, &reply).is_err() {
                return EXIT_OUTPUT;
            }
        }
    })
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
