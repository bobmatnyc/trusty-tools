//! Newline-framed JSON over a hardened Unix socket — the workspace's one
//! framing entry point.
//!
//! Why: three call sites spoke this protocol by hand
//! (`embedder_client/uds.rs`, `trusty-agents`' `MessageBus` and its ctrl
//! socket) and #5089 step 3 added a fourth — console relaying a verified
//! webhook to `trusty-review` / `trusty-analyze`. A fourth bespoke copy is what
//! the common-entry-point rule exists to stop, and ADR-0034 §4 names the shared
//! module explicitly. #5089 step 3 landed the entry point; #5180 migrated the
//! legacy clients onto it. A fifth client, `bm25_client.rs`, was on that list
//! until #5689 deleted the crate it dialled.
//!
//! What: three shapes, one framing contract.
//! - [`send_framed_request`] — one request, one response. Dials through
//!   [`super::connect_hardened`] (so the socket's `0700` directory and `0600`
//!   mode are verified before a single byte is written), writes one
//!   newline-terminated JSON frame, half-closes the write side, and reads one
//!   newline-terminated JSON frame back. Bounded by a caller-supplied timeout
//!   and by [`MAX_FRAME_BYTES`]; [`send_framed_request_capped`] takes the
//!   budget explicitly for bulk-data callers.
//! - [`send_framed_notification`] — one frame out, no reply expected. The
//!   `MessageBus` peer never writes back, so a request helper would block on a
//!   response that does not exist.
//! - [`write_frame`] / [`encode_frame`] — the write half alone, for streaming
//!   NDJSON sites that send many frames over one already-open stream.
//!
//! Deliberately not JSON-RPC-aware: `Req` and `Resp` are whatever the caller
//! names. The framing is the shared part; the envelope is not.
//!
//! #8267: every dial here runs under [`super::retry::ConnectRetry`]. A caller
//! that needs a different bound — a stdio bridge's first dial, which may
//! precede the daemon's own bind — passes one to
//! [`send_framed_request_retrying`].
//!
//! Test: `send_framed_request_round_trips_a_typed_value`,
//! `send_framed_request_reports_no_response_when_peer_hangs_up`,
//! `send_framed_request_rejects_an_over_long_frame`,
//! `send_framed_request_capped_honours_a_caller_supplied_budget`,
//! `send_framed_notification_delivers_exactly_one_frame`,
//! `write_frame_terminates_each_value_with_one_newline` — all against a real
//! listener bound through `bind_hardened`; plus `read_failure_*` over
//! [`classify_read_failure`], which cover the platform split a socket test
//! cannot reproduce on both platforms.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::retry::ConnectRetry;
use super::{UdsSecurityError, connect_hardened};

/// Default largest response frame [`send_framed_request`] will buffer, in bytes.
///
/// A peer that never sends a newline would otherwise grow the read buffer
/// until the process dies. 8 MiB is far above any control-plane frame this
/// workspace exchanges and far below a memory problem.
///
/// #5180: bulk-data callers pass their own budget to
/// [`send_framed_request_capped`] instead — an embed reply carries one
/// JSON-encoded `f32` array per input text and outgrows this figure on a large
/// batch, which is a different problem from a peer that never terminates a
/// frame.
pub const MAX_FRAME_BYTES: u64 = 8 * 1024 * 1024;

/// Everything that can go wrong on one framed exchange.
///
/// Every variant is terminal for the call — none is a "log and continue"
/// condition, because continuing would mean treating an unanswered request as
/// an answered one. `#[non_exhaustive]` for the same reason
/// [`UdsSecurityError`] carries it: this list grows as the transport tightens.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UdsRpcError {
    /// The socket failed verification, or `connect` failed.
    #[error("dial {path}: {source}")]
    Dial {
        /// Socket that could not be dialled.
        path: PathBuf,
        /// Why the dial was refused or failed.
        #[source]
        source: UdsSecurityError,
    },

    /// The request value could not be serialised.
    #[error("serialize request frame for {path}: {source}")]
    Encode {
        /// Socket the frame was destined for.
        path: PathBuf,
        /// Underlying serde error.
        #[source]
        source: serde_json::Error,
    },

    /// Writing the request frame failed.
    #[error("write request frame to {path}: {source}")]
    Write {
        /// Socket that could not be written to.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// Reading the response frame failed.
    #[error("read response frame from {path}: {source}")]
    Read {
        /// Socket that could not be read from.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// The peer closed the connection without writing a frame.
    ///
    /// Distinct from [`UdsRpcError::Read`] on purpose: "it hung up" and "the
    /// read syscall failed" have different causes, and a caller deciding
    /// whether to retry cares which one it got.
    ///
    /// Covers both ways a peer can hang up: a clean EOF, and an abortive close
    /// that surfaces as `ECONNRESET`. See [`classify_read_failure`] for why
    /// those must not be two different variants.
    #[error("{path} closed the connection without sending a response frame")]
    NoResponse {
        /// Socket whose peer hung up.
        path: PathBuf,
    },

    /// The peer sent more than [`MAX_FRAME_BYTES`] without a newline.
    #[error("response frame from {path} exceeded {limit} bytes without a newline")]
    FrameTooLarge {
        /// Socket that overran the budget.
        path: PathBuf,
        /// The budget, in bytes.
        limit: u64,
    },

    /// The response frame was not valid JSON for `Resp`.
    #[error("decode response frame from {path}: {source}")]
    Decode {
        /// Socket that sent the frame.
        path: PathBuf,
        /// Underlying serde error.
        #[source]
        source: serde_json::Error,
    },

    /// The exchange did not complete inside the caller's timeout.
    #[error("{path} did not complete the exchange within {timeout:?}")]
    Timeout {
        /// Socket that did not answer in time.
        path: PathBuf,
        /// The budget that elapsed.
        timeout: Duration,
    },

    // #6286: APPENDED, never inserted. `#[non_exhaustive]` makes a new variant
    // additive, but it does not make a MOVED one additive — placing these ahead
    // of `Timeout` shifted its implicit discriminant from 7 to 9, which
    // `cargo-semver-checks` reports as a major break because a downstream
    // `as isize` cast would change value. New variants go at the end.
    /// A streaming response ended on a terminal error frame (#6286).
    ///
    /// The server's own code and message, unrewritten. Distinct from
    /// [`UdsRpcError::NoResponse`] on purpose: the peer answered, and what it
    /// said is the reason the stream stopped.
    #[error("{path} ended the stream with {error}")]
    Stream {
        /// Socket the stream was read from.
        path: PathBuf,
        /// The server's terminal error.
        error: crate::uds::server::RpcError,
    },

    /// A streaming request was answered with an ordinary response frame (#6286).
    ///
    /// The method does not stream. Boxed because [`super::server::RpcResponse`]
    /// is much larger than every other variant's payload, and an enum is as big
    /// as its widest arm.
    #[error("{path} answered with a single response frame rather than a stream")]
    NotAStream {
        /// Socket that answered.
        path: PathBuf,
        /// The frame it sent, so the caller reads the server's own refusal.
        response: Box<crate::uds::server::RpcResponse>,
    },

    /// Every dial in a bounded connect retry failed (#8267).
    ///
    /// Appended, never inserted — see the #6286 note above. Carries the count
    /// so an operator reading one line knows the client did not give up after
    /// one try, and boxes the last attempt's own error so the socket path and
    /// the OS errno survive intact.
    #[error("{path}: {attempts} connect attempts failed; last error: {source}")]
    ConnectRetriesExhausted {
        /// Socket that could not be dialled.
        path: PathBuf,
        /// How many dials were made, including the first.
        attempts: u32,
        /// The last attempt's failure, verbatim.
        #[source]
        source: Box<UdsRpcError>,
    },

    /// The request frame was written, but half-closing the write side failed
    /// (#8267).
    ///
    /// Why it is not [`UdsRpcError::Write`]: `write_all` + `flush` failing
    /// leaves the peer without a newline-terminated frame, so it never
    /// dispatches and a redial repeats nothing. A `shutdown` failure is the
    /// opposite — the frame is already on the wire. On macOS `soshutdown`
    /// answers ENOTCONN once the peer has closed, which is exactly what a
    /// server that framed on `read_until(b'\n')`, replied and dropped looks
    /// like. Retrying that would deliver a second copy of a request the daemon
    /// had already executed, so this variant is never transient.
    ///
    /// The request's fate is unknown: it may have been dispatched, and the
    /// caller must not read this as "nothing was sent".
    #[error("half-close {path} after writing the request frame: {source}")]
    HalfClose {
        /// Socket whose write side could not be shut down.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },
}

impl UdsRpcError {
    /// Whether this failure means the socket could not be dialled at all.
    ///
    /// Why (#8267): `matches!(err, UdsRpcError::Dial { .. })` used to be the
    /// whole test for "nothing is serving this path", and the bounded retry
    /// breaks it — a dial that failed every attempt arrives as
    /// [`UdsRpcError::ConnectRetriesExhausted`] wrapping the `Dial`. A caller
    /// that branches on the transport verdict wants one predicate rather than
    /// a variant list that grows under it.
    ///
    /// Test: `is_dial_failure_sees_through_the_retry_wrapper`.
    #[must_use]
    pub fn is_dial_failure(&self) -> bool {
        match self {
            Self::Dial { .. } => true,
            Self::ConnectRetriesExhausted { source, .. } => source.is_dial_failure(),
            _ => false,
        }
    }
}

/// Send one JSON frame to `path` and decode the one frame that comes back.
///
/// Why: the single entry point every UDS request/response client in this
/// workspace routes through, so the framing contract, the pre-connect
/// permission check, the size cap and the timeout land once rather than at
/// four call sites (ADR-0034 §4, #5089 step 3).
///
/// What: dials via [`super::connect_hardened`], writes
/// `serde_json::to_vec(request)` followed by `\n`, shuts down the write half
/// (which is what lets a peer that reads to EOF proceed), then reads bytes up
/// to and including the next `\n` and deserialises them as `Resp`. The entire
/// sequence — including the connect — is wrapped in `timeout`.
///
/// A serialised JSON value never contains a bare newline outside a string
/// literal, and inside one it is escaped, so appending `\n` is an unambiguous
/// terminator for any `Req`.
///
/// # Errors
///
/// One [`UdsRpcError`] variant per failure point; see that enum. A returned
/// error always means the request was *not* known to have been processed — a
/// caller must not treat any of them as an acknowledgement.
///
/// Test: `send_framed_request_round_trips_a_typed_value`,
/// `send_framed_request_reports_no_response_when_peer_hangs_up`,
/// `send_framed_request_rejects_an_over_long_frame`,
/// `send_framed_request_reports_a_decode_failure`,
/// `send_framed_request_reports_dial_failure_for_a_missing_socket`,
/// `send_framed_request_times_out_on_a_silent_peer`.
pub async fn send_framed_request<Req, Resp>(
    path: &Path,
    request: &Req,
    timeout: Duration,
) -> Result<Resp, UdsRpcError>
where
    Req: Serialize + ?Sized,
    Resp: DeserializeOwned,
{
    send_framed_request_capped(path, request, timeout, MAX_FRAME_BYTES).await
}

/// [`send_framed_request`] with an explicit response-frame budget.
///
/// Why: #5180 — [`MAX_FRAME_BYTES`] is sized for control-plane frames, and the
/// embedder client's reply is bulk data (one JSON-encoded `f32` array per input
/// text). Forcing it through the shared default would have converted a working
/// large batch into a hard failure, so the budget becomes the caller's to state
/// rather than a reason not to share the framing.
/// What: identical to [`send_framed_request`] except that `max_frame_bytes`
/// replaces [`MAX_FRAME_BYTES`] as the point at which an unterminated response
/// is refused.
///
/// # Errors
///
/// The same [`UdsRpcError`] set as [`send_framed_request`];
/// [`UdsRpcError::FrameTooLarge`] reports `max_frame_bytes` as its `limit`.
///
/// Test: `send_framed_request_capped_honours_a_caller_supplied_budget`.
pub async fn send_framed_request_capped<Req, Resp>(
    path: &Path,
    request: &Req,
    timeout: Duration,
    max_frame_bytes: u64,
) -> Result<Resp, UdsRpcError>
where
    Req: Serialize + ?Sized,
    Resp: DeserializeOwned,
{
    send_framed_request_retrying(
        path,
        request,
        timeout,
        max_frame_bytes,
        ConnectRetry::per_request(),
    )
    .await
}

/// [`send_framed_request_capped`] with an explicit connect-retry bound.
///
/// Why (#8267): a process's FIRST dial is a different question from its
/// hundredth. A stdio MCP bridge launched before its daemon is listening has
/// no socket to dial yet, and the per-request bound — sized to ride out a full
/// accept queue, not a cold start — gives up long before the daemon binds.
/// Handing the policy to the caller is what lets that one dial wait seconds
/// while every later dial stays fast.
///
/// What: identical to [`send_framed_request_capped`] except that `retry`
/// replaces [`ConnectRetry::per_request`]. The caller's `timeout` still bounds
/// the whole sequence, retries included, so this can never extend a call past
/// the budget its caller stated.
///
/// # Errors
///
/// The same [`UdsRpcError`] set as [`send_framed_request_capped`], plus
/// [`UdsRpcError::ConnectRetriesExhausted`] once more than one dial was made.
///
/// Test: `uds_client_retries_transient_connect_refusal`,
/// `uds_client_reports_the_attempt_count_after_the_retry_bound`.
pub async fn send_framed_request_retrying<Req, Resp>(
    path: &Path,
    request: &Req,
    timeout: Duration,
    max_frame_bytes: u64,
    retry: ConnectRetry,
) -> Result<Resp, UdsRpcError>
where
    Req: Serialize + ?Sized,
    Resp: DeserializeOwned,
{
    send_framed_request_with_sleeper(path, request, timeout, max_frame_bytes, retry, tokio_sleep)
        .await
}

/// [`send_framed_request_retrying`] with the retry's sleeper supplied.
///
/// The seam the #8267 regression tests drive: they assert an attempt count and
/// a summed backoff, which against `tokio::time::sleep` would be a wall-clock
/// assertion on a millisecond schedule.
pub(super) async fn send_framed_request_with_sleeper<Req, Resp, S, SFut>(
    path: &Path,
    request: &Req,
    timeout: Duration,
    max_frame_bytes: u64,
    retry: ConnectRetry,
    sleep: S,
) -> Result<Resp, UdsRpcError>
where
    Req: Serialize + ?Sized,
    Resp: DeserializeOwned,
    S: Fn(Duration) -> SFut,
    SFut: std::future::Future<Output = ()>,
{
    match tokio::time::timeout(
        timeout,
        exchange::<Req, Resp, S, SFut>(path, request, max_frame_bytes, retry, sleep),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(UdsRpcError::Timeout {
            path: path.to_path_buf(),
            timeout,
        }),
    }
}

/// The retry driver's real sleeper.
///
/// A named function rather than a closure so it names one concrete `Future`
/// type at every call site instead of a fresh opaque one per site.
fn tokio_sleep(delay: Duration) -> tokio::time::Sleep {
    tokio::time::sleep(delay)
}

/// Send one JSON frame to `path` and return without waiting for a reply.
///
/// Why: #5180 — `trusty-agents`' `MessageBus::send_to` is fire-and-forget. The
/// receiving bus reads NDJSON lines and re-broadcasts them to in-process
/// subscribers; it never writes anything back. Routing it through
/// [`send_framed_request`] would block until the caller's timeout expired and
/// then report a failure for a delivery that succeeded, so the shared module
/// owes the one-way half of the contract rather than a request the peer cannot
/// answer.
///
/// What: dials via [`super::connect_hardened`], writes one newline-terminated
/// JSON frame, flushes, and half-closes the write side so the peer sees EOF.
/// The whole sequence is bounded by `timeout`.
///
/// # Errors
///
/// [`UdsRpcError::Dial`], [`UdsRpcError::Encode`], [`UdsRpcError::Write`], or
/// [`UdsRpcError::Timeout`]. `Ok(())` means the bytes reached the kernel, not
/// that the peer acted on them — that is what one-way means, and a caller that
/// needs an acknowledgement wants [`send_framed_request`] instead.
///
/// Test: `send_framed_notification_delivers_exactly_one_frame`,
/// `send_framed_notification_reports_dial_failure_for_a_missing_socket`.
pub async fn send_framed_notification<Req>(
    path: &Path,
    request: &Req,
    timeout: Duration,
) -> Result<(), UdsRpcError>
where
    Req: Serialize + ?Sized,
{
    match tokio::time::timeout(timeout, dial_and_send(path, request)).await {
        Ok(result) => result.map(|_stream| ()),
        Err(_) => Err(UdsRpcError::Timeout {
            path: path.to_path_buf(),
            timeout,
        }),
    }
}

/// Serialise `value` as one newline-terminated JSON frame.
///
/// Why: #5180 — every UDS client in this workspace open-coded the same two
/// steps (`serde_json::to_vec`, push `b'\n'`), so "what a frame is" was
/// asserted in five places instead of stated in one.
/// What: `serde_json::to_vec(value)` with a trailing `\n`. A serialised JSON
/// value never contains a bare newline outside a string literal, and inside one
/// it is escaped, so the terminator is unambiguous for any `value`.
///
/// # Errors
///
/// Whatever `serde_json::to_vec` returns for a value that cannot be serialised.
///
/// Test: `encode_frame_appends_exactly_one_newline`.
pub fn encode_frame<T>(value: &T) -> serde_json::Result<Vec<u8>>
where
    T: Serialize + ?Sized,
{
    let mut frame = serde_json::to_vec(value)?;
    frame.push(b'\n');
    Ok(frame)
}

/// Write one newline-terminated JSON frame to `writer` and flush it.
///
/// Why: #5180 — the streaming NDJSON sites (`trusty-agents`' ctrl socket) push
/// many frames down one already-connected stream, so they cannot use
/// [`send_framed_request`], which owns the dial and reads exactly one reply.
/// Exposing the write half on its own lets them share the framing anyway
/// instead of keeping a private copy.
/// What: [`encode_frame`], then `write_all` + `flush`.
///
/// # Errors
///
/// A serialisation failure surfaces as `io::ErrorKind::InvalidData`, matching
/// what the call sites this replaced already did; everything else is the
/// underlying write error.
///
/// Test: `write_frame_terminates_each_value_with_one_newline`.
pub async fn write_frame<W, T>(writer: &mut W, value: &T) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize + ?Sized,
{
    let frame =
        encode_frame(value).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    writer.write_all(&frame).await?;
    writer.flush().await
}

/// Dial `path`, write one frame, and half-close the write side.
///
/// Split out so [`send_framed_request_capped`], [`send_framed_notification`] and
/// [`super::stream_client::send_framed_stream_request_capped`] share one copy of
/// the dial-and-write sequence; the returned stream is the still-open read half
/// for callers that expect a reply.
pub(super) async fn dial_and_send<Req>(
    path: &Path,
    request: &Req,
) -> Result<UnixStream, UdsRpcError>
where
    Req: Serialize + ?Sized,
{
    dial_and_send_retrying(path, request, ConnectRetry::per_request(), tokio_sleep).await
}

/// [`dial_and_send`] under a caller-stated connect-retry bound (#8267).
///
/// Why here rather than around the whole exchange: a retry is only safe while
/// the request bytes have provably not reached the peer, which is true of a
/// failed dial and of the `ENOTCONN` first write, and false of everything
/// after. `retry::with_connect_retry` owns that classification; this function
/// owns only the attempt it repeats.
pub(super) async fn dial_and_send_retrying<Req, S, SFut>(
    path: &Path,
    request: &Req,
    retry: ConnectRetry,
    sleep: S,
) -> Result<UnixStream, UdsRpcError>
where
    Req: Serialize + ?Sized,
    S: Fn(Duration) -> SFut,
    SFut: std::future::Future<Output = ()>,
{
    super::retry::with_connect_retry(path, retry, sleep, |_attempt| {
        dial_and_send_once(path, request)
    })
    .await
}

/// One dial-write-half-close, with no retry of its own.
async fn dial_and_send_once<Req>(path: &Path, request: &Req) -> Result<UnixStream, UdsRpcError>
where
    Req: Serialize + ?Sized,
{
    let frame = encode_frame(request).map_err(|source| UdsRpcError::Encode {
        path: path.to_path_buf(),
        source,
    })?;

    let mut stream = connect_hardened(path)
        .await
        .map_err(|source| UdsRpcError::Dial {
            path: path.to_path_buf(),
            source,
        })?;

    // #8267: the send and the half-close are two error variants, not one.
    // `write_all` + `flush` failing leaves the peer without a newline-terminated
    // frame, so it never dispatches and a redial is safe. `shutdown` failing is
    // the opposite: the frame is already on the wire, and on macOS ENOTCONN here
    // means the peer closed AFTER reading it — the server frames on
    // `read_until(b'\n')` and replies without waiting for our half-close. Folding
    // both into `Write` made a retry re-send a request the daemon had executed.
    let send = async {
        stream.write_all(&frame).await?;
        stream.flush().await
    };
    send.await.map_err(|source| UdsRpcError::Write {
        path: path.to_path_buf(),
        source,
    })?;

    // Half-close: the peer's `read_to_end`/`read_until` sees EOF and knows the
    // request is complete. The read half stays open for the response.
    stream
        .shutdown()
        .await
        .map_err(|source| UdsRpcError::HalfClose {
            path: path.to_path_buf(),
            source,
        })?;

    Ok(stream)
}

/// The un-timed body of [`send_framed_request_capped`], split out so the
/// timeout wraps exactly one future and the error mapping stays readable.
async fn exchange<Req, Resp, S, SFut>(
    path: &Path,
    request: &Req,
    max_frame_bytes: u64,
    retry: ConnectRetry,
    sleep: S,
) -> Result<Resp, UdsRpcError>
where
    Req: Serialize + ?Sized,
    Resp: DeserializeOwned,
    S: Fn(Duration) -> SFut,
    SFut: std::future::Future<Output = ()>,
{
    let stream = dial_and_send_retrying(path, request, retry, sleep).await?;
    read_one_frame(stream, path, max_frame_bytes).await
}

/// Read bytes up to and including the next `\n` and decode them as `Resp`.
async fn read_one_frame<R, Resp>(
    source: R,
    path: &Path,
    max_frame_bytes: u64,
) -> Result<Resp, UdsRpcError>
where
    R: AsyncRead + Unpin,
    Resp: DeserializeOwned,
{
    let mut reader = BufReader::new(source.take(max_frame_bytes));
    let mut line: Vec<u8> = Vec::new();
    let read = match reader.read_until(b'\n', &mut line).await {
        Ok(read) => read,
        Err(source) => return Err(classify_read_failure(path, source, line.is_empty())),
    };

    if read == 0 && line.is_empty() {
        return Err(UdsRpcError::NoResponse {
            path: path.to_path_buf(),
        });
    }
    if !line.ends_with(b"\n") && line.len() as u64 >= max_frame_bytes {
        return Err(UdsRpcError::FrameTooLarge {
            path: path.to_path_buf(),
            limit: max_frame_bytes,
        });
    }

    serde_json::from_slice(&line).map_err(|source| UdsRpcError::Decode {
        path: path.to_path_buf(),
        source,
    })
}

/// Decide whether a failed response read means "the peer hung up" or "the read
/// syscall failed".
///
/// Why (#5182): one physical event — a target dropping the connection without
/// answering — reaches this client two different ways. If the peer's receive
/// buffer still holds unread bytes when it closes, Linux resets the connection
/// and our read fails with `ECONNRESET`; macOS hands us a clean EOF instead.
/// `webhook_relay::serve` hits exactly that case when it refuses an over-long
/// frame, having read only the first 64 bytes of it. Classifying by platform
/// means a caller that branches on the variant behaves one way on a developer's
/// machine and another way in CI and in production.
///
/// What: an abortive close with nothing buffered is reported as
/// [`UdsRpcError::NoResponse`], the same as a clean EOF. Anything else stays
/// [`UdsRpcError::Read`] — including a reset that arrives *after* some bytes
/// landed, because a truncated frame is not the same claim as "sent no response
/// frame", and `Read` keeps the errno in the message for diagnosis.
///
/// This renames a failure; it never converts one into a success. Both variants
/// are `Err`, and [`send_framed_request`]'s contract that no error may be read
/// as an acknowledgement covers them equally.
///
/// Test: `read_failure_from_an_abortive_close_reads_as_a_hang_up`,
/// `read_failure_after_partial_bytes_stays_a_read_error`,
/// `read_failure_from_an_unrelated_errno_stays_a_read_error`.
pub(super) fn classify_read_failure(
    path: &Path,
    source: std::io::Error,
    nothing_buffered: bool,
) -> UdsRpcError {
    let hung_up = matches!(
        source.kind(),
        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
    );
    if hung_up && nothing_buffered {
        return UdsRpcError::NoResponse {
            path: path.to_path_buf(),
        };
    }
    UdsRpcError::Read {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uds::bind_hardened;
    use serde::Deserialize;
    use std::path::PathBuf;
    use tokio::net::UnixListener;

    #[derive(Debug, Serialize)]
    struct Ping {
        method: &'static str,
        n: u32,
    }

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct Pong {
        echoed: u32,
    }

    /// How a stub listener answers exactly one connection.
    enum StubReply {
        /// Read the request, then write these bytes verbatim.
        Bytes(Vec<u8>),
        /// Read the request, then drop the connection without writing.
        HangUp,
        /// Accept, then never write and never close.
        Silence,
    }

    /// Bind a hardened socket in `dir` and serve one connection per `replies`.
    ///
    /// Returns the socket path; the listener task ends after the last reply.
    fn spawn_stub(dir: &Path, replies: Vec<StubReply>) -> PathBuf {
        let sock = dir.join("sockets").join("stub.sock");
        let listener: UnixListener = bind_hardened(&sock).expect("bind stub socket");
        tokio::spawn(async move {
            for reply in replies {
                let Ok((mut conn, _)) = listener.accept().await else {
                    return;
                };
                // Drain the request frame so the client's write always lands.
                let mut sink = Vec::new();
                let _ = conn.read_to_end(&mut sink).await;
                match reply {
                    StubReply::Bytes(bytes) => {
                        let _ = conn.write_all(&bytes).await;
                        let _ = conn.flush().await;
                    }
                    StubReply::HangUp => {}
                    StubReply::Silence => {
                        // Hold the connection open past any test's timeout.
                        tokio::time::sleep(Duration::from_secs(300)).await;
                    }
                }
            }
        });
        sock
    }

    #[tokio::test]
    async fn send_framed_request_round_trips_a_typed_value() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = spawn_stub(
            tmp.path(),
            vec![StubReply::Bytes(b"{\"echoed\":41}\n".to_vec())],
        );

        let got: Pong = send_framed_request(
            &sock,
            &Ping {
                method: "ping",
                n: 41,
            },
            Duration::from_secs(5),
        )
        .await
        .expect("round trip");

        assert_eq!(got, Pong { echoed: 41 });
    }

    #[tokio::test]
    async fn send_framed_request_accepts_a_frame_without_a_trailing_newline() {
        // Why: a peer that writes the JSON and closes is well-behaved enough —
        // EOF terminates the frame just as a newline does. Rejecting it would
        // strand a correct target behind a framing nicety.
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = spawn_stub(
            tmp.path(),
            vec![StubReply::Bytes(b"{\"echoed\":7}".to_vec())],
        );

        let got: Pong = send_framed_request(
            &sock,
            &Ping {
                method: "ping",
                n: 7,
            },
            Duration::from_secs(5),
        )
        .await
        .expect("round trip");

        assert_eq!(got, Pong { echoed: 7 });
    }

    #[tokio::test]
    async fn send_framed_request_reports_no_response_when_peer_hangs_up() {
        // Why: this is the arm that must never be mistaken for success. A
        // target that accepts the connection and then dies has NOT acknowledged
        // the work, and #5089's whole point is that the caller can tell.
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = spawn_stub(tmp.path(), vec![StubReply::HangUp]);

        let err = send_framed_request::<_, Pong>(
            &sock,
            &Ping {
                method: "ping",
                n: 1,
            },
            Duration::from_secs(5),
        )
        .await
        .expect_err("a silent hang-up is not a response");

        assert!(
            matches!(err, UdsRpcError::NoResponse { .. }),
            "expected NoResponse, got {err:?}"
        );
    }

    #[tokio::test]
    async fn send_framed_request_rejects_an_over_long_frame() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // One byte past the budget, with no newline anywhere in it.
        let flood = vec![b'x'; (MAX_FRAME_BYTES + 1) as usize];
        let sock = spawn_stub(tmp.path(), vec![StubReply::Bytes(flood)]);

        let err = send_framed_request::<_, Pong>(
            &sock,
            &Ping {
                method: "ping",
                n: 1,
            },
            Duration::from_secs(30),
        )
        .await
        .expect_err("an unterminated flood must not be buffered without bound");

        assert!(
            matches!(err, UdsRpcError::FrameTooLarge { limit, .. } if limit == MAX_FRAME_BYTES),
            "expected FrameTooLarge, got {err:?}"
        );
    }

    #[tokio::test]
    async fn send_framed_request_reports_a_decode_failure() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = spawn_stub(tmp.path(), vec![StubReply::Bytes(b"not json\n".to_vec())]);

        let err = send_framed_request::<_, Pong>(
            &sock,
            &Ping {
                method: "ping",
                n: 1,
            },
            Duration::from_secs(5),
        )
        .await
        .expect_err("garbage is not a response");

        assert!(
            matches!(err, UdsRpcError::Decode { .. }),
            "expected Decode, got {err:?}"
        );
    }

    #[tokio::test]
    async fn send_framed_request_reports_dial_failure_for_a_missing_socket() {
        // Why: the expected state until #5089 step 4 binds the target's
        // listener. The relay must get a clean, classifiable failure here
        // rather than a panic or a hang.
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = tmp.path().join("sockets").join("absent.sock");

        let err = send_framed_request::<_, Pong>(
            &sock,
            &Ping {
                method: "ping",
                n: 1,
            },
            Duration::from_secs(5),
        )
        .await
        .expect_err("no listener means no delivery");

        // #8267: an absent socket is retried, so the terminal error is the
        // retry wrapper around the same `Dial`. `is_dial_failure` is the
        // predicate that spans both.
        assert!(
            err.is_dial_failure(),
            "expected a dial failure, got {err:?}"
        );
    }

    /// Why (#5180): the embedder client raises the budget rather than being
    /// left out of the shared framing, so the budget must actually be the
    /// caller's — a `capped` call that silently used [`MAX_FRAME_BYTES`] would
    /// look identical on the happy path and fail only on a big batch in
    /// production.
    /// What: feeds an unterminated 4 KiB flood under a 1 KiB budget and asserts
    /// the reported limit is the caller's figure, not the module default.
    /// Test: this test itself.
    #[tokio::test]
    async fn send_framed_request_capped_honours_a_caller_supplied_budget() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let flood = vec![b'x'; 4096];
        let sock = spawn_stub(tmp.path(), vec![StubReply::Bytes(flood)]);

        let err = send_framed_request_capped::<_, Pong>(
            &sock,
            &Ping {
                method: "ping",
                n: 1,
            },
            Duration::from_secs(5),
            1024,
        )
        .await
        .expect_err("an unterminated flood past the caller's budget must be refused");

        assert!(
            matches!(err, UdsRpcError::FrameTooLarge { limit, .. } if limit == 1024),
            "expected FrameTooLarge at the caller's 1024-byte budget, got {err:?}"
        );
    }

    /// Why (#5180): `MessageBus`'s peer never replies. This is the test that
    /// fails if `send_to` is ever re-pointed at a request helper — the stub
    /// here writes nothing back, exactly like a real bus.
    /// What: sends a notification to a stub that only reads, and asserts both
    /// that the call succeeds and that the bytes on the wire are one
    /// newline-terminated JSON frame with no second newline.
    /// Test: this test itself.
    #[tokio::test]
    async fn send_framed_notification_delivers_exactly_one_frame() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = tmp.path().join("sockets").join("notify.sock");
        let listener: UnixListener = bind_hardened(&sock).expect("bind");

        let served = tokio::spawn(async move {
            let (mut conn, _) = listener.accept().await.expect("accept");
            let mut got = Vec::new();
            conn.read_to_end(&mut got).await.expect("drain");
            got
        });

        send_framed_notification(
            &sock,
            &Ping {
                method: "ping",
                n: 9,
            },
            Duration::from_secs(5),
        )
        .await
        .expect("a peer that never replies is still a successful delivery");

        let bytes = served.await.expect("join");
        let text = String::from_utf8(bytes).expect("utf8");
        assert_eq!(
            text, "{\"method\":\"ping\",\"n\":9}\n",
            "one frame, one trailing newline, nothing else"
        );
    }

    #[tokio::test]
    async fn send_framed_notification_reports_dial_failure_for_a_missing_socket() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = tmp.path().join("sockets").join("absent.sock");

        let err = send_framed_notification(
            &sock,
            &Ping {
                method: "ping",
                n: 1,
            },
            Duration::from_secs(5),
        )
        .await
        .expect_err("no listener means no delivery");

        assert!(
            err.is_dial_failure(),
            "expected a dial failure, got {err:?}"
        );
    }

    #[test]
    fn encode_frame_appends_exactly_one_newline() {
        let frame = encode_frame(&Ping {
            method: "ping",
            n: 3,
        })
        .expect("encode");
        assert_eq!(frame, b"{\"method\":\"ping\",\"n\":3}\n");
        assert_eq!(
            frame.iter().filter(|b| **b == b'\n').count(),
            1,
            "a frame carries exactly one newline, and it is the terminator"
        );
    }

    /// Why (#5180): this is the primitive the streaming ctrl socket writes
    /// through, where N frames share one stream. A missing or doubled
    /// terminator there desynchronises the reader for the rest of the
    /// connection, not just one message.
    /// What: writes two values into one buffer and asserts the result is two
    /// NDJSON lines.
    /// Test: this test itself.
    #[tokio::test]
    async fn write_frame_terminates_each_value_with_one_newline() {
        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, &Ping { method: "a", n: 1 })
            .await
            .expect("first frame");
        write_frame(&mut buf, &Ping { method: "b", n: 2 })
            .await
            .expect("second frame");

        assert_eq!(
            String::from_utf8(buf).expect("utf8"),
            "{\"method\":\"a\",\"n\":1}\n{\"method\":\"b\",\"n\":2}\n"
        );
    }

    #[tokio::test]
    async fn send_framed_request_times_out_on_a_silent_peer() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = spawn_stub(tmp.path(), vec![StubReply::Silence]);

        let err = send_framed_request::<_, Pong>(
            &sock,
            &Ping {
                method: "ping",
                n: 1,
            },
            Duration::from_millis(150),
        )
        .await
        .expect_err("a peer that never answers must not hold the caller open");

        assert!(
            matches!(err, UdsRpcError::Timeout { .. }),
            "expected Timeout, got {err:?}"
        );
    }

    #[test]
    fn read_failure_from_an_abortive_close_reads_as_a_hang_up() {
        // Linux resets the connection instead of sending EOF when the peer
        // closes with unread bytes still buffered, which is what
        // `webhook_relay::serve` does to an over-long frame. Same event as the
        // clean hang-up above, so it must reach the caller as the same variant.
        for kind in [
            std::io::ErrorKind::ConnectionReset,
            std::io::ErrorKind::ConnectionAborted,
        ] {
            let err = classify_read_failure(
                Path::new("/tmp/relay.sock"),
                std::io::Error::new(kind, "peer went away"),
                true,
            );
            assert!(
                matches!(err, UdsRpcError::NoResponse { .. }),
                "expected NoResponse for {kind:?}, got {err:?}"
            );
        }
    }

    #[test]
    fn read_failure_after_partial_bytes_stays_a_read_error() {
        // Bytes did arrive, so "closed without sending a response frame" would
        // be false. A truncated frame keeps its errno.
        let err = classify_read_failure(
            Path::new("/tmp/relay.sock"),
            std::io::Error::new(std::io::ErrorKind::ConnectionReset, "peer went away"),
            false,
        );

        assert!(
            matches!(err, UdsRpcError::Read { .. }),
            "expected Read, got {err:?}"
        );
    }

    #[test]
    fn read_failure_from_an_unrelated_errno_stays_a_read_error() {
        // Only an abortive close is a hang-up. Widening this would report a
        // genuine syscall failure as a well-behaved peer that chose not to
        // answer.
        let err = classify_read_failure(
            Path::new("/tmp/relay.sock"),
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            true,
        );

        assert!(
            matches!(err, UdsRpcError::Read { .. }),
            "expected Read, got {err:?}"
        );
    }

    // ---------------------------------------------------------------------
    // #8267: the bounded connect retry, driven through an injected sleeper so
    // the attempt count and the backoff schedule are asserted on the schedule
    // itself rather than on wall time.
    // ---------------------------------------------------------------------

    /// The issue's named regression: a dial that fails transiently and then
    /// succeeds must produce a working exchange, not a session-long failure.
    ///
    /// The injected sleeper is what makes the socket appear between attempt 1
    /// and attempt 2 — the real sequence, with the race taken out of it.
    ///
    /// Fails before the fix: with `ConnectRetry::per_request()` at one attempt
    /// (the pre-#8267 behaviour), the first `StatForConnect` ENOENT is the
    /// caller's answer and there is never a second dial.
    #[tokio::test]
    async fn uds_client_retries_transient_connect_refusal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = tmp.path().join("sockets").join("stub.sock");
        let sleeps = std::cell::Cell::new(0u32);

        let got: Pong = send_framed_request_with_sleeper(
            &sock,
            &Ping {
                method: "ping",
                n: 5,
            },
            Duration::from_secs(5),
            MAX_FRAME_BYTES,
            ConnectRetry::per_request(),
            |_delay| {
                let n = sleeps.get() + 1;
                sleeps.set(n);
                if n == 1 {
                    spawn_stub(
                        tmp.path(),
                        vec![StubReply::Bytes(b"{\"echoed\":5}\n".to_vec())],
                    );
                }
                async {}
            },
        )
        .await
        .expect("a socket that appears after the first attempt must still be reached");

        assert_eq!(got, Pong { echoed: 5 });
        assert_eq!(
            sleeps.get(),
            1,
            "one backoff, then the second attempt succeeds"
        );
    }

    /// The bound is real, and the error says what it is.
    ///
    /// Fails before the fix: one attempt, no `ConnectRetriesExhausted`, and
    /// nothing anywhere reporting how many dials were made.
    #[tokio::test]
    async fn uds_client_reports_the_attempt_count_after_the_retry_bound() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = tmp.path().join("sockets").join("absent.sock");
        let policy = ConnectRetry::per_request();
        let slept: std::cell::RefCell<Vec<Duration>> = std::cell::RefCell::new(Vec::new());

        let err = send_framed_request_with_sleeper::<_, Pong, _, _>(
            &sock,
            &Ping {
                method: "ping",
                n: 1,
            },
            Duration::from_secs(30),
            MAX_FRAME_BYTES,
            policy,
            |delay| {
                slept.borrow_mut().push(delay);
                async {}
            },
        )
        .await
        .expect_err("a socket nothing ever binds must fail, not hang");

        let slept = slept.into_inner();
        assert_eq!(
            slept,
            vec![Duration::from_millis(20), Duration::from_millis(40)],
            "the schedule doubles, and there is one fewer sleep than attempts"
        );
        assert!(
            slept.iter().sum::<Duration>() >= policy.backoff_floor(),
            "elapsed backoff must reach the policy's floor"
        );

        let UdsRpcError::ConnectRetriesExhausted {
            path,
            attempts,
            source,
        } = &err
        else {
            panic!("expected ConnectRetriesExhausted, got {err:?}");
        };
        assert_eq!(*attempts, policy.attempts, "exactly N attempts, no more");
        assert_eq!(path, &sock, "the error names the socket it could not reach");
        assert!(
            source.is_dial_failure(),
            "the last attempt's own error is carried, got {source:?}"
        );

        // The Fail-Open Check: the failure is an `Err` that names the path, the
        // count and the OS error — never a warning the caller reads as success.
        let text = err.to_string();
        assert!(text.contains(&sock.display().to_string()), "got {text}");
        assert!(text.contains("3 connect attempts failed"), "got {text}");
        assert!(
            text.contains("No such file or directory"),
            "the last OS error must survive: got {text}"
        );
    }

    /// The policy is the caller's, and `single_attempt` really is one dial.
    ///
    /// Why it matters: `trusty_mcp::DaemonBridgeJsonRpc` spends
    /// [`ConnectRetry::startup`] on its first forwarded request and
    /// [`ConnectRetry::per_request`] thereafter. If the parameter were ignored,
    /// both would silently be the default.
    #[tokio::test]
    async fn send_framed_request_retrying_honours_a_caller_supplied_bound() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = tmp.path().join("sockets").join("absent.sock");

        let err = send_framed_request_with_sleeper::<_, Pong, _, _>(
            &sock,
            &Ping {
                method: "ping",
                n: 1,
            },
            Duration::from_secs(5),
            MAX_FRAME_BYTES,
            ConnectRetry::single_attempt(),
            |_delay| async { panic!("a single-attempt policy must never sleep") },
        )
        .await
        .expect_err("one attempt, one failure");

        assert!(
            matches!(err, UdsRpcError::Dial { .. }),
            "a single attempt reports its own error unwrapped, got {err:?}"
        );
    }

    /// The duplicate-delivery guard, end to end.
    ///
    /// Why: a peer that reads one frame and closes without replying is what a
    /// daemon that dispatched the request and dropped looks like from here. The
    /// client must count that as delivered — whatever error arm it lands on —
    /// and never put a second copy on a fresh connection.
    ///
    /// The half-close error is timing-dependent at the OS level, so this
    /// asserts the invariant that holds on every arm: exactly one dispatch, and
    /// no retry wrapper. `a_failed_half_close_is_never_retried` pins the
    /// classification that makes it hold when the shutdown does fail.
    #[tokio::test]
    async fn a_peer_that_reads_one_frame_and_closes_is_never_sent_a_second_copy() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = tmp.path().join("sockets").join("once.sock");
        let listener: UnixListener = bind_hardened(&sock).expect("bind");
        let dispatched = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let counter = std::sync::Arc::clone(&dispatched);
        tokio::spawn(async move {
            // Two accepts on offer, so a second copy would be counted rather
            // than refused by a listener that had already stopped.
            for _ in 0..2 {
                let Ok((conn, _)) = listener.accept().await else {
                    return;
                };
                let mut reader = BufReader::new(conn);
                let mut line = Vec::new();
                if reader.read_until(b'\n', &mut line).await.is_ok() && !line.is_empty() {
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                // Drop without replying and without draining to EOF — the
                // dispatch-then-hang-up shape.
                drop(reader);
            }
        });

        let err = send_framed_request::<_, Pong>(
            &sock,
            &Ping {
                method: "ping",
                n: 1,
            },
            Duration::from_secs(5),
        )
        .await
        .expect_err("the peer never replies");

        assert!(
            !matches!(err, UdsRpcError::ConnectRetriesExhausted { .. }),
            "a frame that reached the peer must never be redialled, got {err:?}"
        );
        assert_eq!(
            dispatched.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the peer must see exactly one copy of the request"
        );
    }

    #[test]
    fn is_dial_failure_sees_through_the_retry_wrapper() {
        let sock = PathBuf::from("/tmp/absent.sock");
        let inner = UdsRpcError::Dial {
            path: sock.clone(),
            source: UdsSecurityError::Connect {
                path: sock.clone(),
                source: std::io::Error::from(std::io::ErrorKind::ConnectionRefused),
            },
        };
        assert!(inner.is_dial_failure());
        assert!(
            UdsRpcError::ConnectRetriesExhausted {
                path: sock.clone(),
                attempts: 3,
                source: Box::new(inner),
            }
            .is_dial_failure()
        );
        assert!(!UdsRpcError::NoResponse { path: sock }.is_dial_failure());
    }
}
