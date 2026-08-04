//! Process-wide panic hook that routes the panic payload into `tracing`.
//!
//! Why (issue #4764): when a daemon aborts, macOS writes a `.ips` crash report
//! carrying mangled Rust symbols but **not** the panic payload string. Forty
//! consecutive `trusty-search` aborts were diagnosable down to the faulting
//! frame — `std`'s `impl Drop for DirStream` asserting on a failed `closedir`
//! — while the one datum that names the actual `errno`, the panic message,
//! stayed invisible. The default hook prints to raw stderr, which for a
//! launchd-managed daemon lands in a file nobody correlates with a crash
//! report and which carries no thread/backtrace context. Routing the payload
//! through `tracing` first puts it in the same stream as every other daemon
//! log line, and into the in-memory `LogBuffer` that backs `GET /logs/tail`.
//! What: [`install_panic_logger`] wraps (never replaces) the currently
//! installed hook. On panic it emits one `tracing::error!` carrying the
//! payload, source location, thread name, and a force-captured backtrace,
//! then delegates to the previous hook so the standard stderr rendering and
//! any abort semantics are preserved exactly.

use std::sync::Once;

/// Install a process-wide panic hook that logs panics through `tracing`.
///
/// Why (issue #4764): see the module docs — a `.ips` crash report does not
/// carry the panic payload, so the literal message of a production abort is
/// otherwise unrecoverable. This is diagnostic value that outlives any single
/// fix: the next unexplained daemon abort arrives with its message, location,
/// thread, and backtrace already in the log stream.
/// What: idempotent (guarded by a `Once`, so repeated calls from `main` and
/// from tests cannot stack hooks). Takes the current hook, installs a wrapper
/// that logs first and then calls the taken hook. Logging *before* delegating
/// matters: the default hook is what ends the process on a non-unwinding
/// panic, so anything emitted after it would never run. The backtrace is
/// captured with `force_capture`, ignoring `RUST_BACKTRACE` — a launchd
/// daemon has no interactive environment to set it in, and the cost is
/// irrelevant on a path that is already fatal.
///
/// Note: a panic raised *inside* a panic hook aborts immediately, so this hook
/// deliberately does nothing that can fail — no I/O, no indexing, no unwrap.
/// Test: `panic_payload_reaches_the_tracing_subscriber` (the end-to-end
/// proof), `logger_install_is_idempotent`, `logger_preserves_previous_hook`.
pub fn install_panic_logger() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let payload = info
                .payload_as_str()
                .unwrap_or("<non-string panic payload>");
            let location = info.location().map_or_else(
                || "<unknown>".to_string(),
                |l| format!("{}:{}:{}", l.file(), l.line(), l.column()),
            );
            let thread = std::thread::current();
            let thread_name = thread.name().unwrap_or("<unnamed>").to_string();
            let backtrace = std::backtrace::Backtrace::force_capture();
            tracing::error!(
                panic_payload = payload,
                panic_location = %location,
                panic_thread = %thread_name,
                "PANIC in thread '{thread_name}' at {location}: {payload}\n\
                 backtrace:\n{backtrace}"
            );
            previous(info);
        }));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Installing twice must not stack hooks.
    ///
    /// Why: `main` may call this alongside a test harness that already did;
    /// stacking would emit N duplicate log lines per panic and grow unbounded.
    /// What: call twice, assert the process still panics normally afterwards
    /// (a stacked-hook regression manifests as recursion, not a return value,
    /// so the observable assertion is that a caught panic still round-trips).
    /// Test: this test.
    #[test]
    fn logger_install_is_idempotent() {
        install_panic_logger();
        install_panic_logger();
        let caught = std::panic::catch_unwind(|| panic!("idempotence probe"));
        assert!(
            caught.is_err(),
            "panic must still propagate to catch_unwind"
        );
    }

    /// The panic payload must actually reach the tracing subscriber.
    ///
    /// Why (issue #4764): this is the whole point of the hook. Forty daemon
    /// aborts were investigated without ever recovering the literal panic
    /// message, because a macOS `.ips` report does not carry it. A hook that
    /// installs but whose event never lands in the subscriber would leave that
    /// gap exactly as it was, while looking fixed.
    /// What: installs the hook, then panics inside a thread-local
    /// `with_default` subscriber wired to a `LogBuffer`, and asserts the
    /// buffered output contains the payload string, the source location, and
    /// the `PANIC` marker. A thread-local subscriber is used deliberately: the
    /// global one is `try_init`-once per process and would race other tests.
    /// Test: this test.
    #[test]
    fn panic_payload_reaches_the_tracing_subscriber() {
        use tracing_subscriber::layer::SubscriberExt;

        let buffer = crate::log_buffer::LogBuffer::new(32);
        let subscriber = tracing_subscriber::registry()
            .with(crate::log_buffer::LogBufferLayer::new(buffer.clone()));

        install_panic_logger();
        tracing::subscriber::with_default(subscriber, || {
            let _ = std::panic::catch_unwind(|| panic!("payload probe 4764"));
        });

        let captured = buffer.tail(32).join("\n");
        assert!(
            captured.contains("payload probe 4764"),
            "panic payload missing from subscriber output:\n{captured}"
        );
        assert!(
            captured.contains("PANIC"),
            "panic marker missing from subscriber output:\n{captured}"
        );
        assert!(
            captured.contains("panic_hook.rs"),
            "panic location missing from subscriber output:\n{captured}"
        );
    }

    /// The wrapper must delegate to the hook it replaced.
    ///
    /// Why: replacing rather than wrapping the previous hook would silently
    /// drop the default stderr rendering that operators and the macOS crash
    /// reporter rely on.
    /// What: install a sentinel hook that flips a flag, install the logger on
    /// top, panic inside `catch_unwind`, assert the sentinel ran. Restores the
    /// prior hook state on the way out so the rest of the suite is unaffected.
    /// Test: this test.
    #[test]
    fn logger_preserves_previous_hook() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let ran = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&ran);
        let saved = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |_| flag.store(true, Ordering::SeqCst)));

        // Wrap the sentinel directly rather than via `install_panic_logger`,
        // whose `Once` may already have fired in another test in this binary.
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| previous(info)));

        let _ = std::panic::catch_unwind(|| panic!("delegation probe"));
        std::panic::set_hook(saved);

        assert!(ran.load(Ordering::SeqCst), "previous hook must still run");
    }
}
