//! The one HTTP-client constructor every loopback/daemon caller routes through
//! (#4392).
//!
//! Why: reqwest 0.12 sends `127.0.0.1` through `HTTP_PROXY` / `http_proxy` /
//! `ALL_PROXY` when one of them is exported. hyper-util's proxy matcher has no
//! runtime loopback exemption — the only bypass is an explicit `NO_PROXY` entry
//! the operator is not required to have — so on a machine with a corporate proxy
//! configured, every tm↔daemon call is routed to the proxy and fails. The daemon
//! is up; the caller reports it down. `.no_proxy()` on the builder is the fix,
//! and it has to live at ONE entry point: the workspace held ~133 bare
//! `reqwest::Client::builder()` sites and adding the call per-site guarantees the
//! next site forgets it.
//!
//! What:
//! - [`loopback_client_builder`] — the primitive. A `ClientBuilder` with proxies
//!   disabled and NO timeout policy, for callers that own their own bounds (an
//!   SSE stream must not carry a whole-request timeout at all).
//! - [`loopback_client`] — that builder plus the standard
//!   [`LOOPBACK_CONNECT_TIMEOUT`] / [`LOOPBACK_REQUEST_TIMEOUT`] bounds, for
//!   callers with no bespoke timing requirement.
//! - [`blocking_loopback_client_builder`] — the `reqwest::blocking` counterpart,
//!   behind the `blocking-http` feature.
//!
//! Scope: LOOPBACK callers only. A client that genuinely talks to the public
//! internet — crates.io in `update`, an inference provider in `inference` /
//! `chat`, the GitHub API — must keep honouring the operator's proxy, and must
//! NOT be routed through here.
//!
//! Test: `tests::loopback_client_ignores_exported_http_proxy` sets `HTTP_PROXY`
//! to a dead address and proves both halves — a bare builder is diverted, this
//! one still reaches a loopback stub.

use std::time::Duration;

/// TCP connect bound for a loopback call.
///
/// Why: a closed loopback port refuses instantly, but a firewalled or wedged one
/// can hang for the OS default (~75s). Bounding connect separately from the whole
/// request keeps a dead peer cheap.
pub const LOOPBACK_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Whole-request bound (connect + headers + body) for a loopback call.
///
/// Why: a live daemon answers in single-digit milliseconds; 5s is generous for a
/// loaded one and short enough that a CLI never appears to freeze.
pub const LOOPBACK_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Start a `reqwest::Client` for a loopback/daemon target — proxies off, no
/// timeout policy applied.
///
/// Why: `.no_proxy()` is the load-bearing call (see the module docs); leaving the
/// timeouts to the caller is what lets every existing site adopt this entry point
/// without changing its own timing contract, including the SSE readers that must
/// have no whole-request bound.
/// What: `reqwest::Client::builder().no_proxy()`. Chain the caller's own
/// `.timeout()` / `.connect_timeout()` and `.build()`.
/// Test: `tests::loopback_client_ignores_exported_http_proxy`.
pub fn loopback_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder().no_proxy()
}

/// Build a ready-to-use loopback client with the standard bounds.
///
/// Why: most callers want "talk to the daemon, fail fast, never via a proxy" and
/// should not restate the two timeouts.
/// What: [`loopback_client_builder`] plus [`LOOPBACK_CONNECT_TIMEOUT`] and
/// [`LOOPBACK_REQUEST_TIMEOUT`].
/// Test: `tests::loopback_client_ignores_exported_http_proxy`,
/// `tests::loopback_client_builds`.
pub fn loopback_client() -> reqwest::Result<reqwest::Client> {
    loopback_client_builder()
        .connect_timeout(LOOPBACK_CONNECT_TIMEOUT)
        .timeout(LOOPBACK_REQUEST_TIMEOUT)
        .build()
}

/// [`loopback_client_builder`] for the synchronous `reqwest::blocking` API.
///
/// Why: the index-registration and search-readiness paths run on detached
/// threads with no reactor, so they use the blocking client. They are loopback
/// callers and inherit the same defect.
/// What: `reqwest::blocking::Client::builder().no_proxy()`, with the timeout
/// policy left to the caller for the same reason as the async form.
/// Test: `tests::blocking_loopback_client_ignores_exported_http_proxy`.
#[cfg(feature = "blocking-http")]
pub fn blocking_loopback_client_builder() -> reqwest::blocking::ClientBuilder {
    reqwest::blocking::Client::builder().no_proxy()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// A loopback stub that answers one `200 OK` per connection, forever.
    ///
    /// Returns the bound `host:port`. Modelled on
    /// `daemon_guard::tests::spin_until_ready_returns_ok_for_live_server` — a
    /// real listener rather than a mocked client, so the proxy behaviour under
    /// test is the real transport's.
    async fn stub_server() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback stub");
        let addr = listener.local_addr().expect("stub addr").to_string();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    use tokio::io::AsyncWriteExt;
                    let _ = stream
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                        .await;
                });
            }
        });
        addr
    }

    /// An address with nothing listening: bind port 0, read it, release it.
    fn dead_addr() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind to free a port");
        let addr = listener.local_addr().expect("dead addr").to_string();
        drop(listener);
        addr
    }

    /// Set `HTTP_PROXY`, run `body`, restore the previous value.
    ///
    /// SAFETY: every caller is `#[serial]`, so no other test in this binary is
    /// reading or writing the environment concurrently.
    fn with_http_proxy<T>(value: &str, body: impl FnOnce() -> T) -> T {
        let previous = std::env::var("HTTP_PROXY").ok();
        unsafe { std::env::set_var("HTTP_PROXY", value) };
        let out = body();
        unsafe {
            match previous {
                Some(v) => std::env::set_var("HTTP_PROXY", v),
                None => std::env::remove_var("HTTP_PROXY"),
            }
        }
        out
    }

    /// Why (#4392): this is the whole defect. reqwest reads the proxy
    /// environment at client-BUILD time and routes `127.0.0.1` through it, so a
    /// developer with a corporate proxy exported sees every loopback daemon call
    /// fail while the daemon is demonstrably up.
    ///
    /// Both halves are asserted so that deleting `.no_proxy()` as "hygiene"
    /// fails loudly: a bare builder must be diverted to the dead proxy, and
    /// [`loopback_client`] must still reach the stub.
    /// What: exports `HTTP_PROXY` pointing at a released ephemeral port, builds
    /// both clients under it, and issues the same loopback GET with each.
    /// `#[serial]` because `HTTP_PROXY` is process-global.
    /// Test: This is the test.
    ///
    /// If a future reqwest exempts loopback from proxies, the first assertion
    /// becomes obsolete and should be deleted — `.no_proxy()` itself must stay,
    /// because this crate cannot pin every consumer's reqwest patch level.
    #[tokio::test]
    #[serial]
    async fn loopback_client_ignores_exported_http_proxy() {
        let addr = stub_server().await;
        let url = format!("http://{addr}/health");
        let proxy = format!("http://{}", dead_addr());

        let (leaky, guarded) = with_http_proxy(&proxy, || {
            let leaky = reqwest::Client::builder()
                .connect_timeout(Duration::from_millis(500))
                .timeout(Duration::from_millis(500))
                .build()
                .expect("bare client builds");
            let guarded = loopback_client().expect("loopback client builds");
            (leaky, guarded)
        });

        let leaked = leaky.get(&url).send().await;
        let reached = guarded.get(&url).send().await;

        assert!(
            leaked.is_err(),
            "a client WITHOUT .no_proxy() must be diverted through HTTP_PROXY — \
             that diversion IS the #4392 mechanism; got {leaked:?}"
        );
        assert!(
            reached.is_ok_and(|r| r.status().is_success()),
            "loopback_client() must reach a loopback peer with HTTP_PROXY exported"
        );
    }

    /// Why (#4392): the blocking half of the entry point serves the
    /// index-registration and readiness paths, which run off-reactor. It carries
    /// the same defect and needs the same proof.
    /// What: the async test's shape, on `reqwest::blocking`, driven from a
    /// `spawn_blocking` so the blocking client never runs on a reactor thread.
    /// Test: This is the test.
    #[cfg(feature = "blocking-http")]
    #[tokio::test]
    #[serial]
    async fn blocking_loopback_client_ignores_exported_http_proxy() {
        let addr = stub_server().await;
        let url = format!("http://{addr}/health");
        let proxy = format!("http://{}", dead_addr());

        let (leaked, reached) = tokio::task::spawn_blocking(move || {
            with_http_proxy(&proxy, || {
                let leaky = reqwest::blocking::Client::builder()
                    .connect_timeout(Duration::from_millis(500))
                    .timeout(Duration::from_millis(500))
                    .build()
                    .expect("bare blocking client builds");
                let guarded = blocking_loopback_client_builder()
                    .connect_timeout(LOOPBACK_CONNECT_TIMEOUT)
                    .timeout(LOOPBACK_REQUEST_TIMEOUT)
                    .build()
                    .expect("blocking loopback client builds");
                (
                    leaky.get(&url).send().is_err(),
                    guarded
                        .get(&url)
                        .send()
                        .is_ok_and(|r| r.status().is_success()),
                )
            })
        })
        .await
        .expect("blocking probe thread");

        assert!(
            leaked,
            "a blocking client WITHOUT .no_proxy() must be diverted through HTTP_PROXY"
        );
        assert!(
            reached,
            "the blocking loopback builder must reach a loopback peer with HTTP_PROXY exported"
        );
    }

    /// Why: the standard-bounds convenience form must construct.
    /// What: builds it and drops it.
    /// Test: This is the test.
    #[test]
    fn loopback_client_builds() {
        drop(loopback_client().expect("loopback client builds"));
    }
}
