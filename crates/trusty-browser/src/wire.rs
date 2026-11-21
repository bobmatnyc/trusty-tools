//! Async frame I/O on tokio streams, with Python's error classes.
//!
//! Why: each socket handler reacts to a bad frame the way Python's exception
//! class dictates (`Refusal` vs `ValueError` vs `EOFError` vs `OSError` vs
//! `TimeoutError`), so the async reader keeps those classes apart.
//! What: [`read_frame`] and [`write_frame`] wrap [`crate::frame`]; a write
//! drains within 5 s (`asyncio.wait_for(writer.drain(), 5)`).
//! Test: every exchange in `tests/parity_broker.rs`.

use std::io;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::frame::{self, FrameError, HEADER};
use crate::pyjson::Json;
use crate::refusal::Refusal;

/// A frame read or write that failed, by Python exception class.
#[derive(Debug, thiserror::Error)]
pub enum WireError {
    /// `Refusal("InvalidFrame")`: bad length, non-object, non-finite float.
    #[error("InvalidFrame")]
    Invalid,
    /// `ValueError` (a `JSONDecodeError`).
    #[error("malformed frame")]
    Malformed,
    /// `EOFError` (`IncompleteReadError`): the peer closed.
    #[error("end of stream")]
    Eof,
    /// `OSError`.
    #[error("socket: {0}")]
    Io(#[from] io::Error),
    /// `TimeoutError`.
    #[error("timed out")]
    Timeout,
}

impl WireError {
    /// The refusal Python raises for this error, when it is one.
    pub fn refusal(&self) -> Option<Refusal> {
        matches!(self, WireError::Invalid).then(|| Refusal::new("InvalidFrame"))
    }
}

fn read_err(e: io::Error) -> WireError {
    if e.kind() == io::ErrorKind::UnexpectedEof {
        WireError::Eof
    } else {
        WireError::Io(e)
    }
}

/// Read one frame.
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Json, WireError> {
    let mut header = [0u8; HEADER];
    reader.read_exact(&mut header).await.map_err(read_err)?;
    let size = frame::body_len(header).map_err(|_| WireError::Invalid)?;
    let mut body = vec![0u8; size];
    reader.read_exact(&mut body).await.map_err(read_err)?;
    frame::decode_body(&body).map_err(|e| match e {
        FrameError::Invalid => WireError::Invalid,
        FrameError::Malformed(_) => WireError::Malformed,
        FrameError::Io(e) => WireError::Io(e),
    })
}

/// Read one frame within `secs` seconds.
pub async fn read_frame_within<R: AsyncRead + Unpin>(
    reader: &mut R,
    secs: f64,
) -> Result<Json, WireError> {
    tokio::time::timeout(Duration::from_secs_f64(secs), read_frame(reader))
        .await
        .map_err(|_| WireError::Timeout)?
}

/// Write one frame; the write must finish within 5 s.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &Json,
) -> Result<(), WireError> {
    let bytes = frame::encode(message).map_err(|_| WireError::Invalid)?;
    let send = async {
        writer.write_all(&bytes).await?;
        writer.flush().await
    };
    tokio::time::timeout(Duration::from_secs(5), send)
        .await
        .map_err(|_| WireError::Timeout)?
        .map_err(WireError::Io)
}
