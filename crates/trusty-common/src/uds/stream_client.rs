//! The reading half of a multi-frame UDS response (#6286).
//!
//! Why: [`super::send_framed_request`] writes one frame and reads one frame,
//! which is the whole of what a request/response client needs and none of what a
//! token stream needs. `trusty-memory`'s chat endpoint pushes LLM tokens as they
//! arrive, so its client has to see frame N before frame N+1 exists. Putting the
//! reader here rather than in each consumer keeps ONE definition of what
//! terminates a stream — the property a truncated-stream bug hides behind.
//!
//! What: [`send_framed_stream_request`] dials, writes the request frame, and
//! hands back a [`FramedStream`]. Each [`FramedStream::next_frame`] yields the
//! next `"stream":"item"` payload; the stream finishes on the terminal frame the
//! server always writes. [`FramedStream::into_stream`] adapts the same value
//! into a `futures_util::Stream` for a consumer that composes.
//!
//! **EOF is never success.** A stream that ends without a terminal frame yields
//! [`UdsRpcError::NoResponse`], not `None`. A half-received answer read as a
//! complete one is the Fail-Open branch the [`RpcStreamFrame`] contract exists
//! to close, and it is asserted rather than assumed —
//! `stream_reports_a_truncated_stream_rather_than_an_empty_success`.
//!
//! **A non-streamed answer is reported, not decoded.** A server that answers a
//! streaming request with an ordinary response frame — the shape a method that
//! does not stream produces — yields [`UdsRpcError::NotAStream`] carrying that
//! response, so the caller reads the server's own refusal instead of a decode
//! failure about a missing field.
//!
//! Test: `stream_*` in this file's `tests` module, plus
//! `crate::uds::server::tests`' `stream_*` against a real server.

use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::Stream;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncBufReadExt, AsyncReadExt as _, BufReader};
use tokio::net::UnixStream;

use super::rpc::{MAX_FRAME_BYTES, UdsRpcError, dial_and_send};
use super::server::{RpcResponse, RpcStreamFrame, StreamPhase};

/// Dial `path`, ask for a stream, and read the answer frame by frame (#6286).
///
/// Why: the streaming counterpart of [`super::send_framed_request`], sharing its
/// dial, its hardening check and its framing so a streaming consumer is not a
/// second implementation of the transport.
///
/// What: writes `request` as one newline-terminated JSON frame — the caller is
/// responsible for the `"stream": true` field the server negotiates on; see the
/// [`server`] module's wire contract — then returns a [`FramedStream`] over the
/// still-open read half.
///
/// **`timeout` is per step, not per stream.** It bounds the dial and the request
/// write, and then each individual frame read. It deliberately does NOT bound
/// the stream's total duration: a token stream runs as long as the model does,
/// and a total budget would cut off a working answer. A caller that wants a
/// wall-clock ceiling wraps its own consumption loop.
///
/// # Errors
///
/// [`UdsRpcError::Dial`], [`UdsRpcError::Encode`], [`UdsRpcError::Write`] or
/// [`UdsRpcError::Timeout`] while opening. Everything after that is reported per
/// frame, through the stream.
///
/// [`server`]: super::server
///
/// Test: `stream_round_trips_many_frames_over_a_real_socket`,
/// `stream_client_reports_a_dial_failure_for_a_missing_socket`.
pub async fn send_framed_stream_request<Req, T>(
    path: &Path,
    request: &Req,
    timeout: Duration,
) -> Result<FramedStream<T>, UdsRpcError>
where
    Req: Serialize + ?Sized,
    T: DeserializeOwned,
{
    send_framed_stream_request_capped(path, request, timeout, MAX_FRAME_BYTES).await
}

/// [`send_framed_stream_request`] with an explicit per-frame budget.
///
/// Why: the same reason [`super::send_framed_request_capped`] exists — a bulk
/// payload outgrows [`MAX_FRAME_BYTES`], and the budget is the caller's to
/// state. It applies to EACH frame of the stream, not to their sum; the server
/// applies the same figure on its side through
/// [`super::server::RpcServeOptions::max_frame_bytes`], and the two must match
/// or one end merely moves which side refuses.
///
/// # Errors
///
/// The same set as [`send_framed_stream_request`].
///
/// Test: `stream_client_honours_a_caller_supplied_frame_budget`.
pub async fn send_framed_stream_request_capped<Req, T>(
    path: &Path,
    request: &Req,
    timeout: Duration,
    max_frame_bytes: u64,
) -> Result<FramedStream<T>, UdsRpcError>
where
    Req: Serialize + ?Sized,
    T: DeserializeOwned,
{
    let stream = match tokio::time::timeout(timeout, dial_and_send(path, request)).await {
        Ok(result) => result?,
        Err(_) => {
            return Err(UdsRpcError::Timeout {
                path: path.to_path_buf(),
                timeout,
            });
        }
    };

    Ok(FramedStream {
        reader: BufReader::new(stream),
        path: path.to_path_buf(),
        max_frame_bytes,
        frame_timeout: timeout,
        finished: false,
        _item: PhantomData,
    })
}

/// The frames of one streaming response, read as they arrive (#6286).
///
/// Why: an async iterator rather than a collected `Vec` is the whole point — a
/// consumer that could wait for the last token would not need a stream.
/// What: [`next_frame`] yields the next item payload decoded as `T`, `None` once
/// the terminal frame has been read, and exactly one `Err` for a terminal error,
/// a protocol violation, or a read failure. After any of those the stream is
/// finished and every later call returns `None` — an error is reported once, not
/// on every poll.
///
/// [`into_stream`] adapts the same value into a `futures_util::Stream` for a
/// caller that composes with combinators; the two are the same reader, so use
/// one or the other.
///
/// [`next_frame`]: FramedStream::next_frame
/// [`into_stream`]: FramedStream::into_stream
///
/// Test: `stream_round_trips_many_frames_over_a_real_socket`,
/// `stream_reports_a_handler_error_as_a_terminal_frame`,
/// `stream_reports_a_truncated_stream_rather_than_an_empty_success`,
/// `stream_is_finished_after_it_reports_an_error`.
pub struct FramedStream<T> {
    reader: BufReader<UnixStream>,
    path: PathBuf,
    max_frame_bytes: u64,
    frame_timeout: Duration,
    finished: bool,
    _item: PhantomData<fn() -> T>,
}

/// Hand-written rather than derived: `#[derive(Debug)]` would add a spurious
/// `T: Debug` bound, and `T` is only ever a `PhantomData` marker here.
impl<T> std::fmt::Debug for FramedStream<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FramedStream")
            .field("path", &self.path)
            .field("max_frame_bytes", &self.max_frame_bytes)
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

impl<T> FramedStream<T>
where
    T: DeserializeOwned,
{
    /// The socket this stream is reading from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the next item, or `None` once the stream has ended.
    ///
    /// # Errors
    ///
    /// [`UdsRpcError::Stream`] for the server's own terminal error;
    /// [`UdsRpcError::NotAStream`] when the answer was an ordinary response
    /// frame; [`UdsRpcError::NoResponse`] for EOF without a terminal frame;
    /// [`UdsRpcError::FrameTooLarge`], [`UdsRpcError::Decode`],
    /// [`UdsRpcError::Read`] and [`UdsRpcError::Timeout`] per frame.
    ///
    /// Test: `stream_round_trips_many_frames_over_a_real_socket`,
    /// `stream_reports_a_truncated_stream_rather_than_an_empty_success`,
    /// `stream_client_reports_a_unary_answer_as_not_a_stream`.
    pub async fn next_frame(&mut self) -> Option<Result<T, UdsRpcError>> {
        if self.finished {
            return None;
        }
        match self.read_next().await {
            Ok(Some(item)) => Some(Ok(item)),
            Ok(None) => {
                self.finished = true;
                None
            }
            Err(e) => {
                // One report per failure: a caller looping to exhaustion must
                // not spin on a socket that will never produce another frame.
                self.finished = true;
                Some(Err(e))
            }
        }
    }

    /// Adapt this reader into a `futures_util::Stream`.
    ///
    /// Test: `stream_into_stream_yields_the_same_items_as_next_frame`.
    pub fn into_stream(self) -> impl Stream<Item = Result<T, UdsRpcError>> {
        futures_util::stream::unfold(self, |mut reader| async move {
            reader.next_frame().await.map(|item| (item, reader))
        })
    }

    /// One frame: `Ok(Some(item))`, `Ok(None)` for a clean end, or the failure.
    async fn read_next(&mut self) -> Result<Option<T>, UdsRpcError> {
        let line = match tokio::time::timeout(self.frame_timeout, self.read_line()).await {
            Ok(result) => result?,
            Err(_) => {
                return Err(UdsRpcError::Timeout {
                    path: self.path.clone(),
                    timeout: self.frame_timeout,
                });
            }
        };

        let Some(line) = line else {
            // EOF with no terminal frame. Reporting this as a clean end would
            // hand the caller a truncated answer as a complete one.
            return Err(UdsRpcError::NoResponse {
                path: self.path.clone(),
            });
        };

        let frame: RpcStreamFrame = match serde_json::from_slice(&line) {
            Ok(frame) => frame,
            Err(source) => return Err(self.classify_non_stream_frame(&line, source)),
        };

        match frame.stream {
            StreamPhase::Item => {
                let payload = frame.result.unwrap_or(serde_json::Value::Null);
                serde_json::from_value(payload)
                    .map(Some)
                    .map_err(|source| UdsRpcError::Decode {
                        path: self.path.clone(),
                        source,
                    })
            }
            StreamPhase::End => Ok(None),
            StreamPhase::Error => Err(UdsRpcError::Stream {
                path: self.path.clone(),
                // A terminal error frame without an `error` half is a server
                // that broke its own contract; saying so beats a silent end.
                error: frame.error.unwrap_or_else(|| {
                    super::server::RpcError::internal(
                        "terminal stream error frame carried no error",
                    )
                }),
            }),
        }
    }

    /// A frame that is not an [`RpcStreamFrame`]: say which of the two things it
    /// was.
    ///
    /// An ordinary [`RpcResponse`] here means the method does not stream, and
    /// the server's own message is worth more to the caller than serde's
    /// complaint about a missing `stream` field.
    fn classify_non_stream_frame(&self, line: &[u8], source: serde_json::Error) -> UdsRpcError {
        match serde_json::from_slice::<RpcResponse>(line) {
            Ok(response) => UdsRpcError::NotAStream {
                path: self.path.clone(),
                response: Box::new(response),
            },
            Err(_) => UdsRpcError::Decode {
                path: self.path.clone(),
                source,
            },
        }
    }

    /// Read up to and including the next newline, bounded per frame.
    ///
    /// `Ok(None)` is EOF with nothing buffered. `take` is re-applied for each
    /// frame, so the budget is per frame rather than per connection — bytes past
    /// the limit stay in the `BufReader` for the next call.
    async fn read_line(&mut self) -> Result<Option<Vec<u8>>, UdsRpcError> {
        let mut line: Vec<u8> = Vec::new();
        let mut bounded = (&mut self.reader).take(self.max_frame_bytes);
        let read = match bounded.read_until(b'\n', &mut line).await {
            Ok(read) => read,
            Err(source) => {
                return Err(super::rpc::classify_read_failure(
                    &self.path,
                    source,
                    line.is_empty(),
                ));
            }
        };

        if read == 0 && line.is_empty() {
            return Ok(None);
        }
        if !line.ends_with(b"\n") && line.len() as u64 >= self.max_frame_bytes {
            return Err(UdsRpcError::FrameTooLarge {
                path: self.path.clone(),
                limit: self.max_frame_bytes,
            });
        }
        Ok(Some(line))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uds::bind_hardened;
    use serde_json::json;
    use tokio::io::AsyncWriteExt as _;
    use tokio::net::UnixListener;

    /// Bind a hardened socket and answer one connection with `bytes` verbatim,
    /// then hang up. That is enough to drive every reader arm: the server half
    /// is exercised end to end in `uds::server::tests`.
    fn spawn_replaying(dir: &Path, bytes: Vec<u8>) -> PathBuf {
        let sock = dir.join("sockets").join("stream.sock");
        let listener: UnixListener = bind_hardened(&sock).expect("bind");
        tokio::spawn(async move {
            let Ok((mut conn, _)) = listener.accept().await else {
                return;
            };
            let mut sink = Vec::new();
            let _ = conn.read_to_end(&mut sink).await;
            let _ = conn.write_all(&bytes).await;
            let _ = conn.flush().await;
        });
        sock
    }

    async fn open(sock: &Path) -> FramedStream<String> {
        send_framed_stream_request(sock, &json!({ "stream": true }), Duration::from_secs(5))
            .await
            .expect("open the stream")
    }

    #[tokio::test]
    async fn stream_client_reads_items_until_the_terminal_frame() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = spawn_replaying(
            tmp.path(),
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"stream\":\"item\",\"result\":\"Hel\"}\n\
              {\"jsonrpc\":\"2.0\",\"id\":1,\"stream\":\"item\",\"result\":\"lo\"}\n\
              {\"jsonrpc\":\"2.0\",\"id\":1,\"stream\":\"end\"}\n"
                .to_vec(),
        );

        let mut stream = open(&sock).await;
        let mut got = Vec::new();
        while let Some(item) = stream.next_frame().await {
            got.push(item.expect("no error expected"));
        }

        assert_eq!(got, vec!["Hel".to_string(), "lo".to_string()]);
    }

    #[tokio::test]
    async fn stream_reports_a_truncated_stream_rather_than_an_empty_success() {
        // The Fail-Open branch this contract exists to close: two tokens then a
        // hang-up must NOT read as a complete two-token answer.
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = spawn_replaying(
            tmp.path(),
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"stream\":\"item\",\"result\":\"Hel\"}\n".to_vec(),
        );

        let mut stream = open(&sock).await;
        assert_eq!(
            stream.next_frame().await.expect("an item").expect("ok"),
            "Hel"
        );

        let err = stream
            .next_frame()
            .await
            .expect("a truncated stream must report, not end")
            .expect_err("EOF without a terminal frame is not a success");
        assert!(
            matches!(err, UdsRpcError::NoResponse { .. }),
            "expected NoResponse, got {err:?}"
        );
    }

    #[tokio::test]
    async fn stream_is_finished_after_it_reports_an_error() {
        // A caller looping to exhaustion must not spin on a dead socket.
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = spawn_replaying(tmp.path(), Vec::new());

        let mut stream = open(&sock).await;
        assert!(
            stream.next_frame().await.expect("a report").is_err(),
            "an empty answer is a protocol violation"
        );
        assert!(
            stream.next_frame().await.is_none(),
            "the failure is reported once, then the stream is done"
        );
    }

    #[tokio::test]
    async fn stream_client_reports_a_unary_answer_as_not_a_stream() {
        // A method that does not stream answers in the unary shape. The caller
        // must read the server's own refusal, not a serde complaint.
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = spawn_replaying(
            tmp.path(),
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32010,\"message\":\"no\"}}\n"
                .to_vec(),
        );

        let mut stream = open(&sock).await;
        let err = stream
            .next_frame()
            .await
            .expect("a report")
            .expect_err("a unary frame is not a stream item");

        match err {
            UdsRpcError::NotAStream { response, .. } => {
                assert_eq!(response.error.expect("an error").code, -32010);
            }
            other => panic!("expected NotAStream, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stream_client_honours_a_caller_supplied_frame_budget() {
        // The budget is per frame, and it is the caller's figure — a `capped`
        // call that silently used MAX_FRAME_BYTES would look identical on the
        // happy path and fail only on a big frame in production.
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = spawn_replaying(tmp.path(), vec![b'x'; 4096]);

        let mut stream: FramedStream<String> = send_framed_stream_request_capped(
            &sock,
            &json!({ "stream": true }),
            Duration::from_secs(5),
            1024,
        )
        .await
        .expect("open");

        let err = stream
            .next_frame()
            .await
            .expect("a report")
            .expect_err("an unterminated flood past the budget must be refused");
        assert!(
            matches!(err, UdsRpcError::FrameTooLarge { limit, .. } if limit == 1024),
            "expected FrameTooLarge at 1024, got {err:?}"
        );
    }

    #[tokio::test]
    async fn stream_into_stream_yields_the_same_items_as_next_frame() {
        use futures_util::StreamExt as _;

        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = spawn_replaying(
            tmp.path(),
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"stream\":\"item\",\"result\":\"a\"}\n\
              {\"jsonrpc\":\"2.0\",\"id\":1,\"stream\":\"end\"}\n"
                .to_vec(),
        );

        let items: Vec<String> = open(&sock)
            .await
            .into_stream()
            .map(|item| item.expect("ok"))
            .collect()
            .await;

        assert_eq!(items, vec!["a".to_string()]);
    }

    #[tokio::test]
    async fn stream_client_reports_a_dial_failure_for_a_missing_socket() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = tmp.path().join("sockets").join("absent.sock");

        let err = send_framed_stream_request::<_, String>(
            &sock,
            &json!({ "stream": true }),
            Duration::from_secs(5),
        )
        .await
        .expect_err("no listener means no stream");

        assert!(
            matches!(err, UdsRpcError::Dial { .. }),
            "expected Dial, got {err:?}"
        );
    }
}
