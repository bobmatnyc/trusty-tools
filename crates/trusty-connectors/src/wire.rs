//! Frames of the acquire transport (SPEC-OKF-01 Appendix B "Transport").
//!
//! Why: the adapter and the trusty-okf host must agree on every byte; the
//! limits come from the host's own constants so they cannot drift.
//! What: a frame is a 4-byte big-endian length, then that many bytes of
//! JSON. [`read_frame`] returns `Ok(None)` on a clean EOF at a frame
//! boundary and an error for a torn or oversized frame; [`write_frame`]
//! writes and flushes one frame.
//! Test: `wire::tests`.

use std::io::{self, Read, Write};

use serde_json::Value;
pub use trusty_okf::import::adapter::{ADAPTER_PROTOCOL, MAX_FRAME_BYTES};

/// Read one frame; `Ok(None)` when the stream ends before a length.
pub fn read_frame(r: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        match r.read(&mut len[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "torn length")),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let n = u32::from_be_bytes(len) as usize;
    if n > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame of {n} bytes"),
        ));
    }
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

/// Write `msg` as one frame and flush.
pub fn write_frame(w: &mut impl Write, msg: &Value) -> io::Result<()> {
    let body = msg.to_string();
    let len = u32::try_from(body.len())
        .ok()
        .filter(|n| *n as usize <= MAX_FRAME_BYTES)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "reply over the frame cap"))?;
    w.write_all(&len.to_be_bytes())?;
    w.write_all(body.as_bytes())?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn frames_round_trip_and_bad_frames_are_errors() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &json!({"type": "hello"})).unwrap();
        write_frame(&mut buf, &json!({"n": 1})).unwrap();
        let mut r = buf.as_slice();
        let a: Value = serde_json::from_slice(&read_frame(&mut r).unwrap().unwrap()).unwrap();
        assert_eq!(a["type"], "hello");
        assert!(read_frame(&mut r).unwrap().is_some());
        assert!(read_frame(&mut r).unwrap().is_none(), "clean EOF");
        let torn = [0u8, 0];
        assert!(read_frame(&mut torn.as_slice()).is_err());
        let short = [0u8, 0, 0, 5, b'{'];
        assert!(read_frame(&mut short.as_slice()).is_err());
        let huge = (MAX_FRAME_BYTES as u32 + 1).to_be_bytes();
        assert!(read_frame(&mut huge.as_slice()).is_err());
    }
}
