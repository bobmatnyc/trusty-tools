//! The daemon auto-start guard, run at most once per MCP bridge (#8279).
//!
//! Why: `trusty-analyze mcp` used to await the #1078 auto-start guard before
//! reading stdin. The guard can spend its whole 30 s startup budget, which
//! equals the console's per-request timeout, so a loaded host timed out the
//! constant-reply `initialize` and the console retried forever (#8279).
//! What: [`DaemonGate`] wraps the guard in a `OnceCell`. The bridge starts it in
//! the background and the first daemon call awaits the same run, so the
//! handshake never waits on it and concurrent callers share one run.
//! Test: `concurrent_daemon_calls_run_the_guard_once`,
//! `initialize_answers_while_the_daemon_guard_never_returns`,
//! `a_failed_daemon_guard_is_an_in_band_error_and_the_bridge_keeps_serving`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::OnceCell;

/// One run of the guard, boxed so the gate is not generic over it.
type GuardFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

/// A daemon auto-start guard that runs once and remembers its outcome.
///
/// Why: the guard may spawn a daemon, so a second concurrent run could spawn a
/// second one. What: `ensure` runs the guard on first use and hands every
/// caller, concurrent or later, that one run's outcome.
pub struct DaemonGate {
    guard: Box<dyn Fn() -> GuardFuture + Send + Sync>,
    outcome: OnceCell<Result<(), String>>,
}

impl DaemonGate {
    /// Wrap `guard`, which reports a failure as a human-readable message.
    pub fn new<F, Fut>(guard: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), String>> + Send + 'static,
    {
        Self {
            guard: Box::new(move || Box::pin(guard())),
            outcome: OnceCell::new(),
        }
    }

    /// Run the guard if no run has happened yet, and return its outcome.
    ///
    /// Concurrent callers wait on the one in-flight run. The guard runs again
    /// only if the run in progress was cancelled before it finished.
    pub async fn ensure(&self) -> Result<(), String> {
        self.outcome.get_or_init(|| (self.guard)()).await.clone()
    }

    /// Start the guard on a background task without waiting for it.
    ///
    /// Why: keeps the #1078 timing — the daemon starts as soon as the bridge
    /// does — while the stdio loop answers the handshake. A failure is logged
    /// here and reported in-band by the call that needed the daemon.
    pub fn start_in_background(self: &Arc<Self>) {
        let gate = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(e) = gate.ensure().await {
                tracing::warn!("trusty-analyze daemon auto-start failed: {e}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use serde_json::Value;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines};

    use super::*;
    use crate::mcp::{stdio, AnalyzerMcpServer, Request};

    /// Budget for one response; the handshake needs milliseconds.
    const ANSWER_BUDGET: Duration = Duration::from_secs(2);

    /// A client end of the stdio loop: write request lines, read response lines.
    struct Client {
        tx: DuplexStream,
        rx: Lines<BufReader<DuplexStream>>,
    }

    impl Client {
        /// Start `serve_with_daemon_gate` on duplex pipes and return the client end.
        fn start(server: AnalyzerMcpServer, gate: DaemonGate) -> Self {
            let (tx, server_rx) = tokio::io::duplex(64 * 1024);
            let (server_tx, rx) = tokio::io::duplex(64 * 1024);
            tokio::spawn(stdio::serve_with_daemon_gate(
                server,
                Arc::new(gate),
                BufReader::new(server_rx),
                server_tx,
            ));
            Self {
                tx,
                rx: BufReader::new(rx).lines(),
            }
        }

        async fn send(&mut self, request: Value) {
            let mut line = request.to_string();
            line.push('\n');
            self.tx.write_all(line.as_bytes()).await.expect("write");
        }

        /// The next response line, or `None` when none arrives inside the budget.
        async fn answer(&mut self) -> Option<Value> {
            let line = tokio::time::timeout(ANSWER_BUDGET, self.rx.next_line()).await;
            match line {
                Ok(Ok(Some(text))) => serde_json::from_str(&text).ok(),
                _ => None,
            }
        }
    }

    /// A gate whose guard counts its runs and answers `outcome` after `delay`.
    fn counting_gate(
        runs: Arc<AtomicUsize>,
        delay: Duration,
        outcome: Result<(), String>,
    ) -> DaemonGate {
        DaemonGate::new(move || {
            let runs = Arc::clone(&runs);
            let outcome = outcome.clone();
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(delay).await;
                outcome
            }
        })
    }

    /// Why: #8279 — the handshake must not wait on the auto-start guard.
    /// What: a guard that never returns; `initialize` and `tools/list` must
    /// both answer inside the budget.
    /// Test: itself.
    #[tokio::test]
    async fn initialize_answers_while_the_daemon_guard_never_returns() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let server = AnalyzerMcpServer::new(tmp.path().join("absent.sock"));
        let gate = DaemonGate::new(std::future::pending::<Result<(), String>>);
        let mut client = Client::start(server, gate);

        client
            .send(serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}))
            .await;
        client
            .send(serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
        client
            .send(serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
            .await;

        let init = client.answer().await;
        assert!(
            init.is_some(),
            "no initialize response within {ANSWER_BUDGET:?} while the guard is pending"
        );
        let init = init.unwrap_or_default();
        assert_eq!(init["id"], 1);
        assert_eq!(init["result"]["serverInfo"]["name"], "trusty-analyzer");

        let list = client.answer().await.unwrap_or_default();
        assert_eq!(list["id"], 2, "tools/list must answer without the daemon");
        assert!(list["result"]["tools"].is_array());
    }

    /// Why: #8279 fail-open check — a guard failure used to exit the bridge.
    /// What: a guard that fails; `tools/call` must answer with an in-band tool
    /// error carrying the guard's message, and a later request must still be
    /// answered.
    /// Test: itself.
    #[tokio::test]
    async fn a_failed_daemon_guard_is_an_in_band_error_and_the_bridge_keeps_serving() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let server = AnalyzerMcpServer::new(tmp.path().join("absent.sock"));
        let runs = Arc::new(AtomicUsize::new(0));
        let gate = counting_gate(
            Arc::clone(&runs),
            Duration::ZERO,
            Err("stub guard: daemon never came up".into()),
        );
        let mut client = Client::start(server, gate);

        client
            .send(serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": "analyzer_health", "arguments": {}}
            }))
            .await;
        let call = client.answer().await;
        assert!(
            call.is_some(),
            "the bridge closed without answering tools/call"
        );
        let call = call.unwrap_or_default();
        assert_eq!(call["id"], 1);
        assert_eq!(call["result"]["isError"], true);
        let text = call["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        assert!(
            text.contains("stub guard: daemon never came up"),
            "the tool error must carry the guard failure, got: {text}"
        );

        client
            .send(serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
            .await;
        let list = client.answer().await.unwrap_or_default();
        assert_eq!(
            list["id"], 2,
            "the bridge must keep serving after a guard failure"
        );
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "the guard runs once per bridge"
        );
    }

    /// Why: the guard may spawn a daemon; two concurrent first calls must not
    /// spawn two.
    /// What: two concurrent daemon-needing dispatches against one gate whose
    /// guard is slow enough to overlap them; the guard must run exactly once.
    /// Test: itself.
    #[tokio::test]
    async fn concurrent_daemon_calls_run_the_guard_once() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let runs = Arc::new(AtomicUsize::new(0));
        let gate = counting_gate(Arc::clone(&runs), Duration::from_millis(100), Ok(()));
        let server =
            AnalyzerMcpServer::new(tmp.path().join("absent.sock")).with_daemon_gate(Arc::new(gate));
        let health = |id: i64| Request {
            jsonrpc: "2.0".into(),
            id: Some(Value::from(id)),
            method: "analyzer_health".into(),
            params: Value::Null,
        };

        let (a, b) = tokio::join!(server.dispatch(health(1)), server.dispatch(health(2)));

        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "two concurrent calls ran the guard"
        );
        assert_eq!((a.id, b.id), (Value::from(1), Value::from(2)));
    }
}
