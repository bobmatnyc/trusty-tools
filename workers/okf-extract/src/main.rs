//! `trusty-okf-xberg-worker`: the trusty-okf extraction worker (SPEC-OKF-01
//! §1.4, §2.16, Appendix B). Slice S10b of #10746.
//!
//! Why: trusty-okf hands every format but UTF-8 text and Markdown to a
//! separate process, so it never links xberg (B9) and a parser fault ends
//! only this process. The S10 client (`trusty_okf::extract::worker`) spawns
//! it, enforces every limit and deadline, and trusts nothing it reports that
//! the client can check.
//! What: reads frames on stdin and writes frames on stdout; stderr is
//! diagnostic only. The first frame must be `hello`; the worker answers with
//! protocol 1, its version, the xberg version, the configuration hash, its
//! formats and features, `deterministic: true` and the sandbox it observes.
//! Then each `extract` gets one `ok` or `error` reply, `shutdown` or the end
//! of stdin exits 0, and any malformed frame exits 2 (the host fails the item
//! `crashed` or `protocol_error`). The worker reads only bytes from stdin,
//! never a path, and opens no file or socket. Extraction runs on a thread
//! with a 64 MiB stack, so a deeply nested document cannot overflow it.
//! Test: `tests/fixtures.rs` (the Appendix B fixtures through the S10 client),
//! `hello_names_the_pinned_build`.
//!
//! # Spec References
//! - SPEC-OKF-01 §2.16, Appendix B.

mod extract;
mod pdf_check;
mod proto;
mod sandbox;

use std::io::{self, BufReader, BufWriter};
use std::process::ExitCode;

use serde_json::{Value, json};

use proto::{PROTOCOL, Request};

/// The worker's version, reported in `hello` and in `extracted_by`.
const WORKER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Stack size of the serving thread.
const STACK_BYTES: usize = 64 * 1024 * 1024;

/// The worker's `hello` reply. `protocol` is [`PROTOCOL`] whatever the host
/// asked: with no common version the host refuses it (§2.16 "Handshake").
fn hello(config_hash: &str, sandbox: &str) -> Value {
    let formats: Vec<&str> = extract::FORMATS.iter().map(|(e, _)| *e).collect();
    json!({
        "type": "hello",
        "protocol": PROTOCOL,
        "worker_version": WORKER_VERSION,
        "xberg_version": extract::XBERG_VERSION,
        "config_hash": config_hash,
        "formats": formats,
        "deterministic": true,
        "features": extract::FEATURES,
        "sandbox": sandbox,
    })
}

/// Serve one host session over `input` and `output`.
fn serve(input: &mut dyn io::Read, output: &mut dyn io::Write) -> Result<(), String> {
    let engine = extract::Engine::new().map_err(|e| format!("runtime: {e}"))?;
    match proto::read_request(input).map_err(|e| e.to_string())? {
        Some(Request::Hello { .. }) => {}
        Some(other) => return Err(format!("first frame is not hello: {other:?}")),
        None => return Ok(()),
    }
    proto::write_frame(output, &hello(engine.config_hash(), sandbox::detect()))
        .map_err(|e| e.to_string())?;
    loop {
        match proto::read_request(input).map_err(|e| e.to_string())? {
            Some(Request::Extract(header, bytes)) => {
                let reply = engine.reply(&header, bytes);
                proto::write_frame(output, &reply).map_err(|e| e.to_string())?;
            }
            Some(Request::Shutdown) | None => return Ok(()),
            Some(Request::Hello { .. }) => return Err("a second hello".into()),
        }
    }
}

fn main() -> ExitCode {
    let served = std::thread::Builder::new()
        .name("okf-extract".into())
        .stack_size(STACK_BYTES)
        .spawn(|| {
            let mut input = BufReader::new(io::stdin().lock());
            let mut output = BufWriter::new(io::stdout().lock());
            serve(&mut input, &mut output)
        });
    match served.map(|t| t.join()) {
        Ok(Ok(Ok(()))) => ExitCode::SUCCESS,
        Ok(Ok(Err(e))) => {
            eprintln!("trusty-okf-xberg-worker: {e}");
            ExitCode::from(2)
        }
        Ok(Err(_)) => ExitCode::from(101),
        Err(e) => {
            eprintln!("trusty-okf-xberg-worker: cannot start: {e}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the host checks every `hello` field and refuses unknown ones.
    /// What: the reply names protocol 1, this crate's version, xberg 1.3.10
    /// (equal to the `=` pin in Cargo.toml), the four formats, the three
    /// features, `deterministic: true`, and exactly the nine Appendix B keys.
    /// Test: self-contained.
    #[test]
    fn hello_names_the_pinned_build() {
        let h = hello("abc", "none");
        let keys: Vec<&str> = h.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys.len(), 9, "{keys:?}");
        assert_eq!(h["protocol"], 1);
        assert_eq!(h["xberg_version"], "1.3.10");
        assert_eq!(h["deterministic"], true);
        assert_eq!(h["formats"], json!(["pdf", "docx", "xlsx", "pptx"]));
        let manifest = include_str!("../Cargo.toml");
        assert!(
            manifest.contains(&format!("version = \"={}\"", extract::XBERG_VERSION)),
            "XBERG_VERSION must equal the Cargo.toml pin"
        );
    }

    /// Why: the first frame must be `hello`, and a clean end of stdin is a
    /// clean exit.
    /// What: an `extract` first is an error; an empty stdin serves nothing.
    /// Test: self-contained.
    #[test]
    fn the_first_frame_must_be_hello() {
        let mut first = Vec::new();
        proto::write_frame(&mut first, &json!({"type": "shutdown"})).unwrap();
        assert!(serve(&mut first.as_slice(), &mut Vec::new()).is_err());
        let mut out = Vec::new();
        assert!(serve(&mut (&[] as &[u8]), &mut out).is_ok());
        assert!(out.is_empty());
    }
}
