//! One process-wide tracing capture for tests that read log output (#11074).
//!
//! Why: a scoped subscriber misses callsites another test thread registered
//! first, and a test binary holds one global subscriber, so every test that
//! asserts on log lines shares this one instead of installing its own.
//! What: [`capture_logs`] installs a global TRACE subscriber once, writing
//! plain text into a shared buffer; [`logged`] returns everything captured.
//! Test: `malformed_bodies_never_echo_the_value`,
//! `token_and_session_id_never_reach_the_log_11074`.

use std::sync::{Mutex, PoisonError};

/// Every formatted tracing line this test binary emits once [`capture_logs`] ran.
static LOGS: Mutex<Vec<u8>> = Mutex::new(Vec::new());

struct LogSink;

impl std::io::Write for LogSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        LOGS.lock()
            .map_err(|_| std::io::Error::other("log capture poisoned"))?
            .extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Install the global TRACE subscriber writing into [`LOGS`], once.
pub(crate) fn capture_logs() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(|| LogSink)
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
}

/// Everything captured so far, as text.
pub(crate) fn logged() -> String {
    let logs = LOGS.lock().unwrap_or_else(PoisonError::into_inner);
    String::from_utf8_lossy(&logs).into_owned()
}
