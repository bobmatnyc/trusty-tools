//! Tests for the #9396 resilience of `tm content update`: 5xx retries, the
//! rate-limit reason, the `gh auth token` fallback, and the manual install
//! every network and sidecar error names.
//!
//! A child of `bundle_cache_tests` so it reuses that file's in-memory
//! [`FakeSource`] and loopback server; nothing here touches the network or
//! sleeps.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use super::*;
use crate::content::release_source::{
    GhOutput, MAX_RETRIES, Reap, TokenOrigin, gh_auth_token_command, github_token_with,
    rate_limited_reason, wait_bounded,
};

/// A source answering `failures` 503s on every call, then delegating.
struct Flaky {
    inner: FakeSource,
    failures: u32,
    status: u16,
    calls: AtomicU32,
}

impl Flaky {
    fn new(inner: FakeSource, failures: u32, status: u16) -> Self {
        Self {
            inner,
            failures,
            status,
            calls: AtomicU32::new(0),
        }
    }

    fn fail(&self, url: String) -> Result<(), FetchError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) < self.failures {
            return Err(FetchError {
                url,
                reason: format!("HTTP {} X", self.status),
                status: Some(self.status),
            });
        }
        Ok(())
    }
}

impl ReleaseSource for Flaky {
    fn asset_url(&self, tag: &str, file: &str) -> String {
        self.inner.asset_url(tag, file)
    }

    fn asset(&self, tag: &str, file: &str, max: u64) -> Result<Option<Vec<u8>>, FetchError> {
        self.fail(self.asset_url(tag, file))?;
        self.inner.asset(tag, file, max)
    }

    fn content_tags(&self) -> Result<Vec<String>, FetchError> {
        self.fail("fake://tags".into())?;
        self.inner.content_tags()
    }

    fn release(&self, tag: &str) -> Result<Release, FetchError> {
        self.inner.release(tag)
    }
}

/// A [`Retrying`] over `inner` whose sleeps are recorded, never taken.
fn retrying(inner: Flaky) -> (Retrying<Flaky>, Arc<Mutex<Vec<Duration>>>) {
    let slept = Arc::new(Mutex::new(Vec::new()));
    let log = slept.clone();
    let source = Retrying::with_sleeper(inner, move |d| log.lock().expect("log").push(d));
    (source, slept)
}

fn secs(slept: &Mutex<Vec<Duration>>) -> Vec<u64> {
    slept
        .lock()
        .expect("log")
        .iter()
        .map(Duration::as_secs)
        .collect()
}

/// #9396: a 5xx is retried with a doubling backoff until the source answers;
/// the update then pins the release.
#[test]
fn a_5xx_is_retried_until_it_succeeds() {
    let mut fake = FakeSource::default();
    fake.publish(A);
    let (source, slept) = retrying(Flaky::new(fake, 2, 503));
    let cache = tempfile::tempdir().unwrap();
    let outcome = update(cache.path(), &source, None).expect("retried to success");
    assert_eq!(outcome.tag, A);
    assert_eq!(secs(&slept), vec![1, 2], "two retries, backing off");
    resolves_to(cache.path(), A);
}

/// #9396: a 5xx that outlasts every retry fails the update, naming the
/// retries, `tm content update` and the manual install; nothing is pinned.
#[test]
fn a_persistent_5xx_names_the_manual_install() {
    let mut fake = FakeSource::default();
    fake.publish(A);
    let (source, slept) = retrying(Flaky::new(fake, u32::MAX, 502));
    let cache = tempfile::tempdir().unwrap();
    let err = update(cache.path(), &source, None).expect_err("never answers");
    assert!(matches!(err, CacheError::Network { .. }), "{err:?}");
    assert_eq!(secs(&slept), vec![1, 2, 4], "{MAX_RETRIES} retries");
    let msg = err.to_string();
    for needle in [
        "after 3 retries",
        "tm content update",
        "gh release download <tag> --repo bobmatnyc/trusty-tools",
        "tm content install --from <dir>/<tag>.tar.gz",
    ] {
        assert!(msg.contains(needle), "{needle} missing: {msg}");
    }
    assert!(!cache.path().join(LOCK_FILE_NAME).exists());

    // A bundle download that keeps failing names its concrete tag.
    let mut fake = FakeSource::default();
    fake.publish(A);
    let fails_assets = Flaky::new(fake, u32::MAX, 503);
    let err = super::fetch(&fails_assets, A, "x", 8, &Fallback::None).expect_err("503");
    let msg = err.to_string();
    assert!(
        msg.contains(&format!(
            "gh release download {A} --repo bobmatnyc/trusty-tools"
        )),
        "{msg}"
    );
    assert!(
        msg.contains(&format!("tm content install --from <dir>/{A}.tar.gz")),
        "{msg}"
    );
}

/// #9396: only a 5xx is retried; a 403 returns at once, with no sleep.
#[test]
fn a_4xx_is_never_retried() {
    let (source, slept) = retrying(Flaky::new(FakeSource::default(), 1, 403));
    let err = source.content_tags().expect_err("403");
    assert_eq!(err.status, Some(403));
    assert!(secs(&slept).is_empty(), "no retry");
}

/// #9396: a missing sidecar names the manual install for its tag and keeps
/// naming `tm content update`.
#[test]
fn a_missing_sidecar_names_the_manual_install() {
    let mut fake = FakeSource::default();
    fake.publish(A);
    fake.assets.remove(&format!("{A}/{A}.tar.gz.sha256"));
    let cache = tempfile::tempdir().unwrap();
    let msg = update(cache.path(), &fake, None)
        .expect_err("no sidecar")
        .to_string();
    for needle in [
        format!("gh release download {A} --repo bobmatnyc/trusty-tools"),
        format!("tm content install --from <dir>/{A}.tar.gz"),
        "tm content update".to_owned(),
    ] {
        assert!(msg.contains(&needle), "{needle} missing: {msg}");
    }
}

/// Serves one raw HTTP response to every request on a loopback port.
fn serve_raw(response: String) -> String {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.expect("accept");
            let mut reader = BufReader::new(&stream);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
            }
            let _ = stream.write_all(response.as_bytes());
        }
    });
    base
}

/// #9396: a 403 or 429 names GitHub's rate limit, the headers it sent, and
/// how to authenticate — through the source and through `update`.
#[test]
fn a_rate_limited_answer_names_the_limit_and_the_token() {
    for status in ["403 Forbidden", "429 Too Many Requests"] {
        let body = r#"{"message":"API rate limit exceeded"}"#;
        let base = serve_raw(format!(
            "HTTP/1.1 {status}\r\nx-ratelimit-remaining: 0\r\nretry-after: 60\r\n\
             x-ratelimit-reset: 1700000000\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ));
        let err = github(&base).content_tags().expect_err("rate limited");
        let cache = tempfile::tempdir().unwrap();
        let update_err = update(cache.path(), &github(&base), None).expect_err("rate limited");
        for msg in [err.reason.clone(), update_err.to_string()] {
            for needle in [
                status,
                "rate limit",
                "x-ratelimit-remaining: 0",
                "retry-after: 60 s",
                "x-ratelimit-reset: 1700000000",
                "GITHUB_TOKEN",
                "GH_TOKEN",
                "gh auth login",
            ] {
                assert!(msg.contains(needle), "{status}: {needle} missing: {msg}");
            }
        }
    }
    // Without headers the rate limit and the token are still named.
    let bare = rate_limited_reason("403 Forbidden", None, None, None);
    assert!(
        bare.contains("rate limit") && bare.contains("GITHUB_TOKEN"),
        "{bare}"
    );
}

/// #9396: with no token variable set, `gh auth token` supplies one; a `gh`
/// that is missing, fails or prints nothing leaves the source
/// unauthenticated; a set variable wins and `gh` never runs.
#[test]
fn the_token_falls_back_to_gh_auth_token() {
    let unset = |_: &str| None;
    let ran = |success: bool, out: &str| {
        let stdout = out.as_bytes().to_vec();
        move || Ok(GhOutput { success, stdout })
    };
    assert_eq!(
        github_token_with(unset, ran(true, "gho_FROMGH\n")),
        Some(("gho_FROMGH".to_owned(), TokenOrigin::Gh)),
        "present"
    );
    let missing = || Err(std::io::Error::from(std::io::ErrorKind::NotFound));
    assert_eq!(github_token_with(unset, missing), None, "absent");
    assert_eq!(
        github_token_with(unset, ran(false, "gho_IGNORED")),
        None,
        "failing"
    );
    assert_eq!(github_token_with(unset, ran(true, "  \n")), None, "empty");
    let env = |name: &str| (name == "GH_TOKEN").then(|| "env".to_owned());
    let never = || -> std::io::Result<GhOutput> { panic!("gh ran with a token variable set") };
    assert_eq!(
        github_token_with(env, never),
        Some(("env".to_owned(), TokenOrigin::Var("GH_TOKEN")))
    );
}

/// #9396: `gh auth token` asks for the github.com token by name and drops
/// `GH_HOST`, so an enterprise token never goes to api.github.com.
#[test]
fn gh_auth_token_asks_for_the_github_com_token_only() {
    let command = gh_auth_token_command();
    let args: Vec<String> = command
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(args, ["auth", "token", "--hostname", "github.com"]);
    let gh_host: Vec<_> = command
        .get_envs()
        .filter(|(key, _)| *key == "GH_HOST")
        .collect();
    assert_eq!(gh_host, [(std::ffi::OsStr::new("GH_HOST"), None)]);
}

/// A child that never exits, or whose `try_wait` fails; it records whether
/// it was killed and waited on.
#[derive(Default)]
struct FakeChild {
    try_wait_fails: bool,
    killed: bool,
    waited: bool,
}

impl Reap for FakeChild {
    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        if self.try_wait_fails {
            return Err(std::io::Error::other("try_wait failed"));
        }
        Ok(None)
    }

    fn kill(&mut self) -> std::io::Result<()> {
        self.killed = true;
        Ok(())
    }

    fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.waited = true;
        Err(std::io::Error::other("a fake has no exit status"))
    }
}

/// #9396: a timeout and a `try_wait` error both kill and reap the child, so
/// no `gh` is left behind.
#[test]
fn every_exit_but_a_status_kills_and_reaps_the_child() {
    for (try_wait_fails, kind) in [
        (true, std::io::ErrorKind::Other),
        (false, std::io::ErrorKind::TimedOut),
    ] {
        let mut child = FakeChild {
            try_wait_fails,
            ..FakeChild::default()
        };
        let err = wait_bounded(&mut child, Duration::ZERO).expect_err("no exit status");
        assert_eq!(err.kind(), kind, "try_wait_fails={try_wait_fails}");
        assert!(
            child.killed && child.waited,
            "try_wait_fails={try_wait_fails}: killed={} waited={}",
            child.killed,
            child.waited
        );
    }
}

/// Serves the content-tag listing on a loopback port, answering 401 to any
/// request that carries `Authorization`; logs whether each request did.
fn serve_token_refused() -> (String, Arc<Mutex<Vec<bool>>>) {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let listing = refs(&[A]);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.expect("accept");
            let mut reader = BufReader::new(&stream);
            let mut authed = false;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
                authed |= line.to_ascii_lowercase().starts_with("authorization:");
            }
            log.lock().expect("log").push(authed);
            let (status, body) = if authed {
                (
                    "401 Unauthorized",
                    r#"{"message":"Bad credentials"}"#.to_owned(),
                )
            } else {
                ("200 OK", listing.clone())
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (base, seen)
}

/// #9396: a revoked or expired token that `gh` stored does not fail a public
/// read: the 401 is retried once without the token, and the read succeeds.
#[test]
fn a_401_to_a_gh_token_is_retried_unauthenticated() {
    let (base, seen) = serve_token_refused();
    let src = github(&base).with_token_from(Some(("gho_REVOKED".to_owned(), TokenOrigin::Gh)));
    assert_eq!(src.content_tags().expect("read unauthenticated"), [A]);
    assert_eq!(*seen.lock().unwrap(), [true, false], "one retry, no token");
}

/// #9396: a 401 to an exported token is an error naming the variable; it is
/// never dropped silently, and never retried.
#[test]
fn a_401_to_an_env_token_is_an_error_naming_the_variable() {
    for name in ["GITHUB_TOKEN", "GH_TOKEN"] {
        let (base, seen) = serve_token_refused();
        let token = Some(("ghp_REVOKED".to_owned(), TokenOrigin::Var(name)));
        let err = github(&base)
            .with_token_from(token)
            .content_tags()
            .expect_err("refused");
        assert_eq!(err.status, Some(401), "{err:?}");
        assert!(err.reason.contains(&format!("`{name}`")), "{err:?}");
        assert!(!err.reason.contains("ghp_REVOKED"), "{err:?}");
        assert_eq!(*seen.lock().unwrap(), [true], "{name}: no retry");
    }
}
