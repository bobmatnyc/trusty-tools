//! The host side of the converter worker: a pool of worker processes.
//!
//! Why: PDF, Office, HTML and email conversion runs in the separate
//! `trusty-summarizer-convert` process (plan section 3), so a parser crash,
//! hang or runaway output costs one file and the batch goes on.
//!
//! What: [`WorkerConverter`] is a [`Converter`] that keeps up to
//! `convert.workers` processes alive and reuses each one for many files, so
//! a batch pays start-up once per process, not once per file. Each request
//! has a time limit and a response-size limit. A process that crashes, times
//! out or exceeds the size limit is killed and dropped; the file gets an
//! [`Error::Worker`] naming the failure, and the next file starts a fresh
//! process. A healthy process goes back to the pool.
//! Test: `workers/summarizer-convert/tests/worker.rs`

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::config::ConvertConfig;
use crate::convert::{ConvertInput, Converter, Converters};
use crate::error::{Error, Result, WorkerFailure};
use crate::worker::proto::{
    BUILD_NAME, CONVERTER_NAME, HOST_CONVERTER_NAME, Hello, Request, Response, WORKER_BIN, build_of,
};

/// The formats the worker converts.
#[cfg(not(feature = "xberg14"))]
pub const WORKER_FORMATS: &[&str] = &["pdf", "docx", "pptx", "html", "htm", "eml"];
/// The formats the worker converts; the Xberg 1.4 build adds `xlsx`.
#[cfg(feature = "xberg14")]
pub const WORKER_FORMATS: &[&str] = &["pdf", "docx", "pptx", "html", "htm", "eml", "xlsx"];

/// Environment variable naming the worker binary.
pub const WORKER_ENV: &str = "TRUSTY_SUMMARIZER_CONVERT";

/// Bytes of the worker's stderr kept for a crash message.
const STDERR_TAIL: usize = 2048;

/// Finds the worker binary.
///
/// Why: it must be found from `cargo run`/`cargo test` and from two
/// installed binaries side by side, with no setup.
/// What: in order, `configured`, then `$TRUSTY_SUMMARIZER_CONVERT`, then the
/// directory holding the running executable, then that directory's parent
/// when it is cargo's `deps` directory (a test binary), then each `PATH`
/// entry. A named path (the first two) must exist; otherwise the error lists
/// every place looked.
/// Test: `workers/summarizer-convert/tests/worker.rs` (missing_worker_is_an_error_record,
/// worker_is_found_beside_the_executable)
pub fn locate_worker(configured: Option<&Path>) -> std::result::Result<PathBuf, String> {
    let named = configured
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os(WORKER_ENV).map(PathBuf::from));
    if let Some(path) = named {
        return if path.is_file() {
            Ok(path)
        } else {
            Err(format!("{} does not exist", path.display()))
        };
    }
    let file = format!("{WORKER_BIN}{}", std::env::consts::EXE_SUFFIX);
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        if dir.file_name().is_some_and(|n| n == "deps")
            && let Some(up) = dir.parent()
        {
            dirs.push(up.to_path_buf());
        }
        dirs.insert(0, dir);
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    if let Some(found) = dirs.iter().map(|d| d.join(&file)).find(|p| p.is_file()) {
        return Ok(found);
    }
    let looked: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
    Err(format!(
        "`{file}` not found beside the executable or on PATH (looked in {}); set convert.worker or {WORKER_ENV}",
        looked.join(", ")
    ))
}

/// A [`Converter`] backed by a pool of worker processes.
///
/// Thread-safe: concurrent `convert` calls use different processes, up to
/// `convert.workers`; further calls wait for a free one.
pub struct WorkerConverter {
    program: std::result::Result<PathBuf, String>,
    limits: ConvertConfig,
    pool: Mutex<Pool>,
    freed: Condvar,
    next_id: AtomicU64,
}

#[derive(Default)]
struct Pool {
    idle: Vec<Process>,
    live: usize,
}

impl std::fmt::Debug for WorkerConverter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerConverter")
            .field("program", &self.program)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl WorkerConverter {
    /// A converter using the worker [`locate_worker`] finds for `config`. A
    /// missing worker is not an error here: each file then gets a
    /// `worker_missing` record, so text files in the batch still work.
    pub fn new(config: &ConvertConfig) -> Self {
        WorkerConverter {
            program: locate_worker(config.worker.as_deref()),
            limits: config.clone(),
            pool: Mutex::new(Pool::default()),
            freed: Condvar::new(),
            next_id: AtomicU64::new(0),
        }
    }

    /// Registers one shared converter for every [`WORKER_FORMATS`] entry.
    pub fn register(converters: &mut Converters, config: &ConvertConfig) {
        let shared: Arc<dyn Converter> = Arc::new(WorkerConverter::new(config));
        for format in WORKER_FORMATS {
            converters.register(format, Arc::clone(&shared));
        }
    }

    /// The worker path in use, or why none was found.
    pub fn program(&self) -> std::result::Result<&Path, &str> {
        self.program.as_deref().map_err(String::as_str)
    }

    fn timeout(&self) -> Duration {
        Duration::from_millis(self.limits.timeout_ms)
    }

    /// Takes an idle process, or starts one when fewer than
    /// `convert.workers` are alive, or waits for one to be returned.
    fn acquire(&self) -> Result<Process> {
        let program = self.program.as_ref().map_err(|m| Error::Worker {
            failure: WorkerFailure::Missing,
            message: m.clone(),
        })?;
        let mut pool = self.pool.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if let Some(p) = pool.idle.pop() {
                return Ok(p);
            }
            if pool.live < self.limits.workers {
                pool.live += 1;
                drop(pool);
                let started = Process::start(
                    program,
                    Duration::from_millis(self.limits.start_timeout_ms),
                    self.limits.max_output_bytes,
                );
                if started.is_err() {
                    self.release(None);
                }
                return started;
            }
            pool = self.freed.wait(pool).unwrap_or_else(|p| p.into_inner());
        }
    }

    /// Returns a healthy process to the pool, or forgets a dead one.
    fn release(&self, process: Option<Process>) {
        let mut pool = self.pool.lock().unwrap_or_else(|p| p.into_inner());
        match process {
            Some(p) => pool.idle.push(p),
            None => pool.live -= 1,
        }
        drop(pool);
        self.freed.notify_one();
    }
}

impl Converter for WorkerConverter {
    fn name(&self) -> &str {
        HOST_CONVERTER_NAME
    }

    fn caches_text(&self) -> bool {
        true
    }

    fn convert(&self, input: &ConvertInput<'_>) -> Result<String> {
        let mut process = self.acquire()?;
        let request = Request {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            format: input.format.to_string(),
            len: input.bytes.len() as u64,
        };
        let outcome = process.request(&request, input.bytes, self.timeout());
        let healthy = !matches!(outcome, Err(Error::Worker { .. }));
        if healthy {
            self.release(Some(process));
        } else {
            process.kill();
            self.release(None);
        }
        let text = strip_style_labels(&outcome?);
        if text.trim().is_empty() {
            return Err(Error::NoText {
                converter: HOST_CONVERTER_NAME.into(),
            });
        }
        Ok(text)
    }
}

/// Removes the ` (style_name: …)` labels Xberg appends to DOCX paragraphs,
/// e.g. `Survey (style_name: heading 1)` becomes `Survey`. Edits stay
/// within a line, so line numbers do not move.
///
/// Test: `style_labels_are_removed_within_lines`
pub(crate) fn strip_style_labels(text: &str) -> String {
    const OPEN: &str = "(style_name:";
    if !text.contains(OPEN) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(OPEN) {
        let close = rest[at..].find([')', '\n']);
        match close {
            Some(off) if rest.as_bytes()[at + off] == b')' => {
                out.push_str(rest[..at].trim_end_matches([' ', '\t']));
                rest = &rest[at + off + 1..];
            }
            _ => {
                // Unclosed on this line: not a label; keep it.
                out.push_str(&rest[..at + OPEN.len()]);
                rest = &rest[at + OPEN.len()..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// What the stdout reader thread saw.
enum Line {
    /// One complete line, newline included.
    Full(Vec<u8>),
    /// A line longer than the size limit; the reader stopped.
    TooLong,
    /// End of output, possibly after a partial line, or a read error.
    End,
}

/// One running worker.
struct Process {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<Line>,
    stderr: Arc<Mutex<Vec<u8>>>,
    stderr_reader: Option<std::thread::JoinHandle<()>>,
}

impl Process {
    /// Starts the worker and checks its hello line.
    fn start(program: &Path, timeout: Duration, max_line: u64) -> Result<Process> {
        let fail = |failure, message: String| Error::Worker { failure, message };
        let mut child = Command::new(program)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                fail(
                    WorkerFailure::Spawn,
                    format!("cannot start {}: {e}", program.display()),
                )
            })?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(fail(
                WorkerFailure::Spawn,
                "worker pipes unavailable".into(),
            ));
        };
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut buf = Vec::new();
                let read = (&mut reader).take(max_line + 1).read_until(b'\n', &mut buf);
                let line = match read {
                    Ok(_) if buf.ends_with(b"\n") => Line::Full(buf),
                    Ok(_) if buf.len() as u64 > max_line => Line::TooLong,
                    _ => Line::End,
                };
                let last = !matches!(line, Line::Full(_));
                if tx.send(line).is_err() || last {
                    return;
                }
            }
        });
        let tail = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&tail);
        let stderr_reader = std::thread::spawn(move || {
            let mut stderr = stderr;
            let mut chunk = [0u8; 1024];
            while let Ok(n @ 1..) = stderr.read(&mut chunk) {
                let mut t = sink.lock().unwrap_or_else(|p| p.into_inner());
                t.extend_from_slice(&chunk[..n]);
                let excess = t.len().saturating_sub(STDERR_TAIL);
                t.drain(..excess);
            }
        });
        let mut process = Process {
            child,
            stdin,
            lines,
            stderr: tail,
            stderr_reader: Some(stderr_reader),
        };
        let hello = match process.next_line(Instant::now() + timeout, "start") {
            Ok(line) => line,
            Err(e) => {
                process.kill();
                return Err(e);
            }
        };
        match serde_json::from_slice::<Hello>(&hello) {
            Ok(h) if h == Hello::current() => Ok(process),
            other => {
                process.kill();
                let got = match other {
                    Ok(h) => format!(
                        "converter `{}` protocol {} ({})",
                        h.converter,
                        h.protocol,
                        build_of(&h.converter)
                    ),
                    Err(e) => format!("an unreadable hello ({e})"),
                };
                Err(fail(
                    WorkerFailure::Protocol,
                    format!(
                        "{} reports {got}; this host is the {BUILD_NAME} and expects converter `{CONVERTER_NAME}`",
                        program.display()
                    ),
                ))
            }
        }
    }

    /// Sends one document and waits for its answer.
    ///
    /// Errors: [`Error::Worker`] when the process is no longer usable
    /// (crash, timeout, oversize or garbled answer); [`Error::Conversion`]
    /// when the worker answered with an error and stays usable.
    fn request(&mut self, req: &Request, bytes: &[u8], timeout: Duration) -> Result<String> {
        let deadline = Instant::now() + timeout;
        let mut header = serde_json::to_vec(req).map_err(|e| Error::Worker {
            failure: WorkerFailure::Protocol,
            message: e.to_string(),
        })?;
        header.push(b'\n');
        let line = std::thread::scope(|scope| {
            let stdin = &mut self.stdin;
            // A writer thread, so a worker that stops reading cannot block
            // the time limit. Its own errors (a broken pipe after a crash)
            // are reported through the reader side.
            scope.spawn(move || {
                let _ = stdin
                    .write_all(&header)
                    .and_then(|()| stdin.write_all(bytes))
                    .and_then(|()| stdin.flush());
            });
            let line = Self::wait_line(&self.lines, deadline);
            if matches!(line, Err(Waited::TimedOut | Waited::TooLong)) {
                // Unblocks the writer before the scope joins it. A crashed
                // worker needs no kill: its closed pipe fails the write.
                let _ = self.child.kill();
            }
            line
        });
        let line = match line {
            Ok(line) => line,
            Err(Waited::TimedOut) => {
                return Err(self.failure(WorkerFailure::Timeout, timeout, "convert"));
            }
            Err(Waited::TooLong) => {
                return Err(self.failure(WorkerFailure::OutputTooLarge, timeout, "convert"));
            }
            Err(Waited::Ended) => {
                return Err(self.failure(WorkerFailure::Crashed, timeout, "convert"));
            }
        };
        let response: Response = serde_json::from_slice(&line).map_err(|e| Error::Worker {
            failure: WorkerFailure::Protocol,
            message: format!("unreadable answer: {e}"),
        })?;
        if response.id() != req.id {
            return Err(Error::Worker {
                failure: WorkerFailure::Protocol,
                message: format!(
                    "answer for request {} while waiting for {}",
                    response.id(),
                    req.id
                ),
            });
        }
        match response {
            Response::Ok { text, .. } => Ok(text),
            Response::Error { kind, message, .. } => Err(Error::Conversion {
                converter: CONVERTER_NAME.into(),
                message: format!("{kind}: {message}"),
            }),
        }
    }

    fn wait_line(
        lines: &Receiver<Line>,
        deadline: Instant,
    ) -> std::result::Result<Vec<u8>, Waited> {
        let left = deadline.saturating_duration_since(Instant::now());
        match lines.recv_timeout(left) {
            Ok(Line::Full(line)) => Ok(line),
            Ok(Line::TooLong) => Err(Waited::TooLong),
            Ok(Line::End) | Err(RecvTimeoutError::Disconnected) => Err(Waited::Ended),
            Err(RecvTimeoutError::Timeout) => Err(Waited::TimedOut),
        }
    }

    /// The next stdout line during start-up.
    fn next_line(&mut self, deadline: Instant, during: &str) -> Result<Vec<u8>> {
        let timeout = deadline.saturating_duration_since(Instant::now());
        Self::wait_line(&self.lines, deadline).map_err(|w| {
            let failure = match w {
                Waited::TimedOut => WorkerFailure::Timeout,
                Waited::TooLong => WorkerFailure::OutputTooLarge,
                Waited::Ended => WorkerFailure::Crashed,
            };
            self.failure(failure, timeout, during)
        })
    }

    /// Kills the process and describes `failure`, with the exit status and
    /// the end of its stderr for a crash.
    fn failure(&mut self, failure: WorkerFailure, timeout: Duration, during: &str) -> Error {
        let message = match failure {
            WorkerFailure::Timeout => format!(
                "no answer within the {} ms limit during {during}; worker killed",
                timeout.as_millis()
            ),
            WorkerFailure::OutputTooLarge => {
                "answer over the convert.max_output_bytes limit; worker killed".to_string()
            }
            _ => {
                let status = self.exit_status();
                // The process has exited, so its stderr is closed and the
                // reader thread ends after the last bytes.
                if let Some(reader) = self.stderr_reader.take() {
                    let _ = reader.join();
                }
                let tail = self.stderr.lock().unwrap_or_else(|p| p.into_inner());
                let tail = String::from_utf8_lossy(&tail);
                let tail = tail.trim();
                if tail.is_empty() {
                    format!("worker exited during {during}: {status}")
                } else {
                    format!("worker exited during {during}: {status}; stderr: {tail}")
                }
            }
        };
        self.kill();
        Error::Worker { failure, message }
    }

    /// The exit status of a process whose stdout has closed. One that is
    /// still running a second later is killed, and that is reported.
    fn exit_status(&mut self) -> String {
        let until = Instant::now() + Duration::from_secs(1);
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return status.to_string(),
                Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(5)),
                Ok(None) => {
                    self.kill();
                    return "closed its output and was killed".into();
                }
                Err(e) => return format!("unknown status ({e})"),
            }
        }
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.kill();
    }
}

enum Waited {
    TimedOut,
    TooLong,
    Ended,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_labels_are_removed_within_lines() {
        let t = "Harbor Lights Survey (style_name: heading 1)\nBody text (style_name: Normal).\nkeep (style_name: open\n";
        assert_eq!(
            strip_style_labels(t),
            "Harbor Lights Survey\nBody text.\nkeep (style_name: open\n"
        );
        assert_eq!(strip_style_labels("plain\n"), "plain\n");
    }
}
