//! A stand-in converter worker for tests; not shipped.
//!
//! Why: the crash case needs a worker that crashes on demand, which the real
//! worker must never do. Built by `cargo test` as an example and run as a
//! separate process, so the host code under test is the production code.
//!
//! What: speaks the real protocol. The document's bytes choose the answer:
//! `crash` aborts after a stderr line, `hang` sleeps for a minute, `pid`
//! answers with the process id, anything else is echoed back as the text.
//! `--announce <converter>` puts that converter name in the hello line, as a
//! worker from the other build would.

use std::io::{self, BufRead, Read, Write};

use trusty_summarizer::worker::proto::{Hello, Request, Response};

fn main() -> io::Result<()> {
    let mut input = io::stdin().lock();
    let mut out = io::stdout().lock();
    let mut hello = Hello::current();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [flag, converter] = args.as_slice()
        && flag == "--announce"
    {
        hello.converter = converter.clone();
    }
    let hello = serde_json::to_string(&hello).map_err(io::Error::other)?;
    writeln!(out, "{hello}")?;
    out.flush()?;
    let mut header = String::new();
    loop {
        header.clear();
        if input.read_line(&mut header)? == 0 {
            return Ok(());
        }
        let req: Request = serde_json::from_str(&header).map_err(io::Error::other)?;
        let mut bytes = vec![0; req.len as usize];
        input.read_exact(&mut bytes)?;
        let text = match bytes.as_slice() {
            b"crash" => {
                eprintln!("stand-in: crashing on request {}", req.id);
                std::process::abort();
            }
            b"hang" => {
                std::thread::sleep(std::time::Duration::from_secs(60));
                String::new()
            }
            b"pid" => format!("pid {}", std::process::id()),
            other => String::from_utf8_lossy(other).into_owned(),
        };
        let line =
            serde_json::to_string(&Response::Ok { id: req.id, text }).map_err(io::Error::other)?;
        writeln!(out, "{line}")?;
        out.flush()?;
    }
}
