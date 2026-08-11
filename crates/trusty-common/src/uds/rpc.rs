//! One request, one response, newline-framed JSON over a hardened Unix socket.
//!
//! Why: three call sites already speak this exact protocol by hand
//! (`embedder_client/uds.rs`, `bm25_client.rs`, `trusty-agents`' `ctrl/socket.rs`)
//! and #5089 step 3 adds a fourth — console relaying a verified webhook to
//! `trusty-review` / `trusty-analyze`. Writing a fourth bespoke copy is what the
//! common-entry-point rule exists to stop, and ADR-0034 §4 names the shared
//! module explicitly. The three existing clients are migrated in a follow-up;
//! this lands the entry point they migrate onto.
//!
//! What: [`send_framed_request`] dials through [`super::connect_hardened`] (so
//! the socket's `0700` directory and `0600` mode are verified before a single
//! byte of the request is written), writes one newline-terminated JSON frame,
//! half-closes the write side, and reads one newline-terminated JSON frame back.
//! The whole exchange is bounded by a caller-supplied timeout and the response
//! by [`MAX_FRAME_BYTES`].
//!
//! Deliberately not JSON-RPC-aware: `Req` and `Resp` are whatever the caller
//! names. The framing is the shared part; the envelope is not.
//!
//! Test: `tests.rs` — `send_framed_request_*` against a real listener bound
//! through `bind_hardened`, covering the round trip, a server that closes
//! without answering, an over-long frame, a malformed frame, an absent socket,
//! and the timeout; plus `read_failure_*` over [`classify_read_failure`], which
//! cover the platform split a socket test cannot reproduce on both platforms.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use super::{UdsSecurityError, connect_hardened};

/// Largest response frame [`send_framed_request`] will buffer, in bytes.
///
/// A peer that never sends a newline would otherwise grow the read buffer
/// until the process dies. 8 MiB is far above any frame this workspace
/// exchanges (the largest is an embedding batch) and far below a memory
/// problem.
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
    match tokio::time::timeout(timeout, exchange::<Req, Resp>(path, request)).await {
        Ok(result) => result,
        Err(_) => Err(UdsRpcError::Timeout {
            path: path.to_path_buf(),
            timeout,
        }),
    }
}

/// The un-timed body of [`send_framed_request`], split out so the timeout wraps
/// exactly one future and the error mapping stays readable.
async fn exchange<Req, Resp>(path: &Path, request: &Req) -> Result<Resp, UdsRpcError>
where
    Req: Serialize + ?Sized,
    Resp: DeserializeOwned,
{
    let mut frame = serde_json::to_vec(request).map_err(|source| UdsRpcError::Encode {
        path: path.to_path_buf(),
        source,
    })?;
    frame.push(b'\n');

    let mut stream = connect_hardened(path)
        .await
        .map_err(|source| UdsRpcError::Dial {
            path: path.to_path_buf(),
            source,
        })?;

    let write = async {
        stream.write_all(&frame).await?;
        stream.flush().await?;
        // Half-close: the peer's `read_to_end`/`read_until` sees EOF and knows
        // the request is complete. The read half stays open for the response.
        stream.shutdown().await
    };
    write.await.map_err(|source| UdsRpcError::Write {
        path: path.to_path_buf(),
        source,
    })?;

    let mut reader = BufReader::new(stream.take(MAX_FRAME_BYTES));
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
    if !line.ends_with(b"\n") && line.len() as u64 >= MAX_FRAME_BYTES {
        return Err(UdsRpcError::FrameTooLarge {
            path: path.to_path_buf(),
            limit: MAX_FRAME_BYTES,
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
fn classify_read_failure(
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

        assert!(
            matches!(err, UdsRpcError::Dial { .. }),
            "expected Dial, got {err:?}"
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
}
