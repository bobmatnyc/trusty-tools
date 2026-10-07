//! Where `tm content update` reads published content releases from (#8378 PR-C).
//!
//! Why: the update logic must be testable offline, with a release that is
//! missing, tampered with or unreachable, so the network sits behind a trait.
//! What: [`ReleaseSource`] — fetch one release asset, list the `content-v*`
//! tags, read one release. [`GithubReleases`] is the production
//! implementation over GitHub's release-download URLs, its
//! `git/matching-refs` endpoint and `releases/tags/{tag}`. #9396:
//! [`Retrying`] retries a 5xx with backoff, a 403/429 names the rate limit
//! and the token, and the token falls back to `gh auth token`; [`github_source`]
//! is the production source with all three.
//! Test: `github_source_reads_a_404_as_absent`,
//! `github_source_lists_every_content_tag_in_one_request`,
//! `github_source_reports_an_unreachable_host`.

use std::io::Read;
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use trusty_common::content::TAG_PREFIX;

/// The repository that publishes `content-v*` releases (ADR-0064 decision 4).
pub const CONTENT_REPO: &str = "bobmatnyc/trusty-tools";

/// Largest API response read; one ref is about 400 bytes, so this holds
/// about 10 000 `content-v*` tags. A longer listing is refused, not truncated.
pub(crate) const MAX_LISTING_BYTES: u64 = 4 * 1024 * 1024;

/// A request that did not produce an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchError {
    /// The URL requested.
    pub url: String,
    /// Why it failed.
    pub reason: String,
    /// The HTTP status, when the server answered one (#9396).
    pub status: Option<u16>,
}

impl FetchError {
    /// Whether the server answered a 5xx, which [`Retrying`] retries (#9396).
    pub fn is_server_error(&self) -> bool {
        self.status.is_some_and(|code| (500..600).contains(&code))
    }
}

/// One `content-v*` release as `releases/tags/{tag}` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// The release's tag.
    pub tag: String,
    /// An unpublished draft.
    pub draft: bool,
    /// Marked as a pre-release.
    pub prerelease: bool,
}

/// A source of published content releases.
pub trait ReleaseSource {
    /// The URL [`ReleaseSource::asset`] reads for `file` of `tag`.
    fn asset_url(&self, tag: &str, file: &str) -> String;
    /// Fetches release asset `file` of `tag`, at most `max_bytes`. `Ok(None)`
    /// when the release or the asset does not exist.
    fn asset(&self, tag: &str, file: &str, max_bytes: u64) -> Result<Option<Vec<u8>>, FetchError>;
    /// Every git tag starting `content-v`, in no particular order. A listing
    /// that cannot be read in full is an error; an empty one is `Ok`.
    fn content_tags(&self) -> Result<Vec<String>, FetchError>;
    /// The release of `tag`. A tag with no release, or whose release is a
    /// draft (GitHub answers 404 for one), is an error.
    fn release(&self, tag: &str) -> Result<Release, FetchError>;
}

/// GitHub releases of [`CONTENT_REPO`].
///
/// Why: `content-release.yml` publishes each bundle and its sidecar as
/// release assets. The whole `/releases` listing is megabytes against a
/// 60 requests/hour unauthenticated limit, so the newest tag is found by
/// listing only `content-v*` refs, and only that tag's release is read (#9036).
/// A tag alone is not a release (it may be a draft or pre-release), so the
/// chosen tag's release is still read before anything is installed (#8389).
/// What: assets from `<download_base>/<tag>/<file>`; tags from one unpaged
/// read of `<api_base>/git/matching-refs/tags/content-v`, which GitHub answers
/// in full (it ignores `per_page`/`page`), bounded by `MAX_LISTING_BYTES`;
/// one release from `<api_base>/releases/tags/<tag>`, which answers 404 for a
/// draft.
/// API calls send `Authorization: Bearer <token>` when a token is set; the
/// token is marked sensitive and redacted from `Debug`. Bounded timeouts;
/// every body is read through a size cap.
/// Test: `github_source_reads_a_404_as_absent`,
/// `github_source_lists_every_content_tag_in_one_request`,
/// `github_source_reads_a_non_404_error_status_as_a_failure`,
/// `github_source_refuses_a_tag_listing_that_is_not_json`,
/// `github_source_refuses_a_tag_listing_over_its_size_cap`,
/// `github_source_refuses_a_malformed_or_mismatched_answer`,
/// `github_source_reads_a_404_tag_listing_as_a_failure`,
/// `github_source_reads_a_rate_limited_or_5xx_response_as_a_failure`,
/// `update_finds_the_newest_release_in_two_api_requests`,
/// `github_source_sends_the_token_only_when_one_is_set`,
/// `a_token_never_appears_in_debug_or_error_output`.
#[derive(Clone)]
pub struct GithubReleases {
    client: reqwest::blocking::Client,
    download_base: String,
    api_base: String,
    token: Option<(String, TokenOrigin)>,
}

impl std::fmt::Debug for GithubReleases {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubReleases")
            .field("download_base", &self.download_base)
            .field("api_base", &self.api_base)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}

/// The GitHub token: `GITHUB_TOKEN`, else `GH_TOKEN` (the order
/// `trusty-installer` reads), else `gh auth token` (#9396).
fn github_token() -> Option<(String, TokenOrigin)> {
    github_token_with(|name| std::env::var(name).ok(), run_gh_auth_token)
}

/// Where the API token came from, which decides what a 401 does (#9396).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenOrigin {
    /// Passed to [`GithubReleases::with_token`]; a 401 is an error.
    Given,
    /// The named environment variable; a 401 is an error naming it.
    Var(&'static str),
    /// `gh auth token`; a 401 is retried once unauthenticated.
    Gh,
}

/// How long `gh auth token` may run before it is killed (#9396).
const GH_TIMEOUT: Duration = Duration::from_secs(5);

/// The argv after `gh`: the token for github.com only, never the host
/// `GH_HOST` or the active login names (#9396).
pub(crate) const GH_AUTH_TOKEN_ARGS: [&str; 4] = ["auth", "token", "--hostname", "github.com"];

/// What `gh auth token` printed, and whether it exited 0.
#[derive(Debug)]
pub(crate) struct GhOutput {
    /// Whether `gh` exited 0.
    pub(crate) success: bool,
    /// Its standard output: the token.
    pub(crate) stdout: Vec<u8>,
}

/// The token from the environment, else from `gh`.
///
/// Why: an unauthenticated caller shares GitHub's 60 requests/hour limit, and
/// an operator logged in with `gh` already holds a token (#9396).
/// What: [`github_token_from`] first; only when it finds none, `gh` runs. A
/// `gh` that is missing, fails, times out or prints nothing leaves the source
/// unauthenticated, never an error. Neither the token nor `gh`'s output is
/// logged. The token comes back with its [`TokenOrigin`].
/// Test: `the_token_falls_back_to_gh_auth_token`.
pub(crate) fn github_token_with(
    var: impl Fn(&str) -> Option<String>,
    gh: impl FnOnce() -> std::io::Result<GhOutput>,
) -> Option<(String, TokenOrigin)> {
    if let Some((name, token)) = github_token_from(var) {
        return Some((token, TokenOrigin::Var(name)));
    }
    match gh() {
        Ok(out) if out.success => String::from_utf8(out.stdout)
            .ok()
            .and_then(normalize_token)
            .map(|token| (token, TokenOrigin::Gh)),
        Ok(_) => {
            tracing::debug!("`gh auth token` exited non-zero; reading GitHub unauthenticated");
            None
        }
        Err(e) => {
            tracing::debug!(kind = ?e.kind(), "`gh auth token` did not run; reading GitHub unauthenticated");
            None
        }
    }
}

/// The `gh auth token` command, not yet spawned.
///
/// Why: with `GH_HOST` set, or gh logged in only to an enterprise host, a
/// bare `gh auth token` prints that host's token, which would then go to
/// api.github.com as a bearer token (#9396).
/// What: [`GH_AUTH_TOKEN_ARGS`] (`--hostname github.com`) with `GH_HOST`
/// removed; stdin and stderr null, stdout piped.
/// Test: `gh_auth_token_asks_for_the_github_com_token_only`.
pub(crate) fn gh_auth_token_command() -> std::process::Command {
    let mut command = trusty_common::gh::GhCommand::new(GH_AUTH_TOKEN_ARGS)
        .env_remove("GH_HOST")
        .to_std_command();
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

/// Runs [`gh_auth_token_command`], killing it after [`GH_TIMEOUT`].
fn run_gh_auth_token() -> std::io::Result<GhOutput> {
    let mut child = gh_auth_token_command().spawn()?;
    let status = wait_bounded(&mut child, GH_TIMEOUT)?;
    let mut stdout = Vec::new();
    if let Some(out) = child.stdout.take() {
        out.take(4096).read_to_end(&mut stdout)?;
    }
    Ok(GhOutput {
        success: status.success(),
        stdout,
    })
}

/// The parts of a child process [`wait_bounded`] drives; tests fake it.
pub(crate) trait Reap {
    /// [`std::process::Child::try_wait`].
    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>>;
    /// [`std::process::Child::kill`].
    fn kill(&mut self) -> std::io::Result<()>;
    /// [`std::process::Child::wait`].
    fn wait(&mut self) -> std::io::Result<ExitStatus>;
}

impl Reap for std::process::Child {
    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        std::process::Child::try_wait(self)
    }

    fn kill(&mut self) -> std::io::Result<()> {
        std::process::Child::kill(self)
    }

    fn wait(&mut self) -> std::io::Result<ExitStatus> {
        std::process::Child::wait(self)
    }
}

/// Waits up to `timeout` for `child` to exit.
///
/// Why: a child that is never waited on is left behind as a zombie (#9396).
/// What: polls `try_wait` every 20 ms. An exit status is returned. A timeout
/// or a `try_wait` error kills and reaps the child, then returns the error.
/// Test: `every_exit_but_a_status_kills_and_reaps_the_child`.
pub(crate) fn wait_bounded(
    child: &mut impl Reap,
    timeout: Duration,
) -> std::io::Result<ExitStatus> {
    let deadline = Instant::now() + timeout;
    let failure = loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() >= deadline => {
                break std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "`gh auth token` did not finish",
                );
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => break e,
        }
    };
    // #9396: reap on every path that has no exit status yet.
    let _ = child.kill();
    let _ = child.wait();
    Err(failure)
}

/// The reason for a 403 or 429 answer (#9396).
///
/// Why: unauthenticated callers share 60 API requests an hour per IP, so a
/// bare "HTTP 403" leaves the operator guessing; the headers say which limit
/// and when it resets, and a token raises it.
/// What: names the status, the rate limit and every rate-limit header GitHub
/// sent (`x-ratelimit-remaining`, `retry-after`, `x-ratelimit-reset`), then
/// how to authenticate.
/// Test: `a_rate_limited_answer_names_the_limit_and_the_token`.
pub(crate) fn rate_limited_reason(
    status: &str,
    remaining: Option<&str>,
    retry_after: Option<&str>,
    reset: Option<&str>,
) -> String {
    let mut facts = Vec::new();
    if let Some(left) = remaining {
        facts.push(format!("x-ratelimit-remaining: {left}"));
    }
    if let Some(secs) = retry_after {
        facts.push(format!("retry-after: {secs} s"));
    }
    if let Some(at) = reset {
        facts.push(format!("x-ratelimit-reset: {at}"));
    }
    let facts = if facts.is_empty() {
        String::new()
    } else {
        format!(" ({})", facts.join(", "))
    };
    format!(
        "HTTP {status}: GitHub's API rate limit refused the request{facts}; set GITHUB_TOKEN \
         or GH_TOKEN, or run `gh auth login`, to read GitHub authenticated"
    )
}

/// A token trimmed of surrounding whitespace; empty or whitespace-only is unset.
fn normalize_token(token: String) -> Option<String> {
    let trimmed = token.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// The first set (non-blank, trimmed) of `GITHUB_TOKEN` and `GH_TOKEN` as
/// `var` reads them, with the variable's name (#9396).
/// #9036: an empty or whitespace-only `GITHUB_TOKEN` falls through to
/// `GH_TOKEN`; a blank `GH_TOKEN` is unset.
/// Test: `an_empty_github_token_falls_through_to_gh_token`.
pub(super) fn github_token_from(
    var: impl Fn(&str) -> Option<String>,
) -> Option<(&'static str, String)> {
    [trusty_common::env_vars::ENV_GITHUB_TOKEN, "GH_TOKEN"]
        .into_iter()
        .find_map(|name| var(name).and_then(normalize_token).map(|t| (name, t)))
}

/// The reason for a 401 to a token tm will not drop (#9396).
fn refused_token_reason(origin: TokenOrigin) -> String {
    match origin {
        TokenOrigin::Var(name) => format!(
            "HTTP 401 Unauthorized: GitHub refused the token in `{name}`, which is revoked or \
             expired; replace it, or unset `{name}` to read GitHub unauthenticated"
        ),
        TokenOrigin::Given | TokenOrigin::Gh => "HTTP 401 Unauthorized: GitHub refused the \
                                                 configured token, which is revoked or expired"
            .to_owned(),
    }
}

impl GithubReleases {
    /// The production source; authenticates with `GITHUB_TOKEN`, `GH_TOKEN`
    /// or `gh auth token`, whichever is found first (#9396).
    pub fn new() -> Result<Self, FetchError> {
        Ok(Self::with_bases(
            &format!("https://github.com/{CONTENT_REPO}/releases/download"),
            &format!("https://api.github.com/repos/{CONTENT_REPO}"),
        )?
        .with_token_from(github_token()))
    }

    /// A source reading from other base URLs, unauthenticated; tests point it
    /// at a local server.
    pub fn with_bases(download_base: &str, api_base: &str) -> Result<Self, FetchError> {
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .user_agent(concat!("trusty-mpm/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| FetchError {
                url: download_base.to_owned(),
                reason: e.to_string(),
                status: None,
            })?;
        Ok(Self {
            client,
            download_base: download_base.trim_end_matches('/').to_owned(),
            api_base: api_base.trim_end_matches('/').to_owned(),
            token: None,
        })
    }

    /// The same source sending `token` on API calls; `None`, an empty or a
    /// whitespace-only token stays unauthenticated (#9036).
    pub fn with_token(self, token: Option<String>) -> Self {
        self.with_token_from(token.map(|t| (t, TokenOrigin::Given)))
    }

    /// [`GithubReleases::with_token`] with the token's origin, which decides
    /// whether a 401 is retried unauthenticated (#9396).
    pub fn with_token_from(mut self, token: Option<(String, TokenOrigin)>) -> Self {
        self.token = token.and_then(|(t, origin)| normalize_token(t).map(|t| (t, origin)));
        self
    }

    /// Sends a GET, with the bearer token when `auth` and a token is set.
    fn send(&self, url: &str, auth: bool) -> Result<reqwest::blocking::Response, FetchError> {
        let fail = |reason: String| FetchError {
            url: url.to_owned(),
            reason,
            status: None,
        };
        let mut request = self.client.get(url);
        if let (true, Some((token, _))) = (auth, &self.token) {
            // #9036: the reason never carries the token or the header value.
            let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| fail("the GitHub token is not a valid header value".to_owned()))?;
            value.set_sensitive(true);
            request = request.header(reqwest::header::AUTHORIZATION, value);
        }
        request.send().map_err(|e| fail(e.to_string()))
    }

    /// A GET read through a size cap; `auth` adds the bearer token (API calls
    /// only, never asset downloads).
    ///
    /// Why: a token `gh` stored can be revoked or expire, and a public read
    /// must not fail on it; a token the operator exported must not be
    /// dropped without a word (#9396).
    /// What: a 404 is `Ok(None)`. A 401 to a `gh` token is retried once
    /// unauthenticated; a 401 to any other token is an error naming where it
    /// came from. A 403/429 names the rate limit.
    /// Test: `a_401_to_a_gh_token_is_retried_unauthenticated`,
    /// `a_401_to_an_env_token_is_an_error_naming_the_variable`.
    fn get(&self, url: &str, max_bytes: u64, auth: bool) -> Result<Option<Vec<u8>>, FetchError> {
        let fail = |reason: String| FetchError {
            url: url.to_owned(),
            reason,
            status: None,
        };
        let mut response = self.send(url, auth)?;
        if let (true, reqwest::StatusCode::UNAUTHORIZED, Some((_, origin))) =
            (auth, response.status(), &self.token)
        {
            if *origin != TokenOrigin::Gh {
                return Err(FetchError {
                    status: Some(401),
                    ..fail(refused_token_reason(*origin))
                });
            }
            tracing::warn!(
                "GitHub refused the `gh auth token` token (HTTP 401); reading unauthenticated"
            );
            response = self.send(url, false)?;
        }
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            let header = |name: &str| {
                response
                    .headers()
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned)
            };
            // #9396: a 403/429 is the rate limit; say so and name the token.
            let reason = if matches!(status.as_u16(), 403 | 429) {
                rate_limited_reason(
                    &status.to_string(),
                    header("x-ratelimit-remaining").as_deref(),
                    header("retry-after").as_deref(),
                    header("x-ratelimit-reset").as_deref(),
                )
            } else {
                format!("HTTP {status}")
            };
            return Err(FetchError {
                status: Some(status.as_u16()),
                ..fail(reason)
            });
        }
        let mut body = Vec::new();
        response
            .take(max_bytes.saturating_add(1))
            .read_to_end(&mut body)
            .map_err(|e| fail(e.to_string()))?;
        if body.len() as u64 > max_bytes {
            return Err(fail(format!("the response is over {max_bytes} bytes")));
        }
        Ok(Some(body))
    }

    /// An API read; the repository exists, so a 404 is a failure, not "none".
    fn api(&self, url: &str) -> Result<Vec<u8>, FetchError> {
        self.get(url, MAX_LISTING_BYTES, true)?
            .ok_or_else(|| FetchError {
                url: url.to_owned(),
                reason: "HTTP 404 Not Found".to_owned(),
                status: Some(404),
            })
    }
}

impl ReleaseSource for GithubReleases {
    fn asset_url(&self, tag: &str, file: &str) -> String {
        format!("{}/{tag}/{file}", self.download_base)
    }

    fn asset(&self, tag: &str, file: &str, max_bytes: u64) -> Result<Option<Vec<u8>>, FetchError> {
        self.get(&self.asset_url(tag, file), max_bytes, false)
    }

    fn content_tags(&self) -> Result<Vec<String>, FetchError> {
        #[derive(serde::Deserialize)]
        struct ApiRef {
            r#ref: String,
        }
        // #9036: matching-refs ignores `per_page` and `page` and sends no Link
        // header, so one unpaged read is the whole list; paging re-reads it.
        let url = format!("{}/git/matching-refs/tags/{TAG_PREFIX}", self.api_base);
        let refs: Vec<ApiRef> =
            serde_json::from_slice(&self.api(&url)?).map_err(|e| FetchError {
                url: url.clone(),
                reason: format!("unexpected response: {e}"),
                status: None,
            })?;
        refs.into_iter()
            .map(|entry| {
                // A ref outside refs/tags/ is not what was asked for: refuse it.
                entry
                    .r#ref
                    .strip_prefix("refs/tags/")
                    .map(str::to_owned)
                    .ok_or_else(|| FetchError {
                        url: url.clone(),
                        reason: format!("unexpected ref {:?}", entry.r#ref),
                        status: None,
                    })
            })
            .collect()
    }

    fn release(&self, tag: &str) -> Result<Release, FetchError> {
        #[derive(serde::Deserialize)]
        struct ApiRelease {
            tag_name: String,
            draft: bool,
            prerelease: bool,
        }
        let url = format!("{}/releases/tags/{tag}", self.api_base);
        let release: ApiRelease =
            serde_json::from_slice(&self.api(&url)?).map_err(|e| FetchError {
                url: url.clone(),
                reason: format!("unexpected response: {e}"),
                status: None,
            })?;
        if release.tag_name != tag {
            return Err(FetchError {
                url,
                reason: format!("asked for release {tag}, got {}", release.tag_name),
                status: None,
            });
        }
        Ok(Release {
            tag: release.tag_name,
            draft: release.draft,
            prerelease: release.prerelease,
        })
    }
}

/// How many times [`Retrying`] retries a 5xx after the first attempt (#9396).
pub const MAX_RETRIES: u32 = 3;

/// The wait before the first retry; each later one doubles it.
const FIRST_BACKOFF: Duration = Duration::from_secs(1);

/// The production source: [`GithubReleases`], retried on a 5xx (#9396).
pub fn github_source() -> Result<Retrying<GithubReleases>, FetchError> {
    Ok(Retrying::new(GithubReleases::new()?))
}

/// A [`ReleaseSource`] that retries a server error (5xx) with backoff.
///
/// Why: a GitHub 5xx is usually transient, and one failed request used to
/// fail `tm content update` and the first-use fetch outright (#9396).
/// What: each call runs up to `1 + MAX_RETRIES` times; only an answer with
/// a 5xx status is retried, after 1 s, 2 s, then 4 s. Any other error, a 404
/// read as "absent", and success return at once. The last 5xx is returned
/// with the retry count in its reason. The sleep is injected so tests never
/// wait.
/// Test: `a_5xx_is_retried_until_it_succeeds`,
/// `a_persistent_5xx_names_the_manual_install`, `a_4xx_is_never_retried`.
pub struct Retrying<S> {
    inner: S,
    sleep: Box<dyn Fn(Duration) + Send + Sync>,
}

impl<S: ReleaseSource> Retrying<S> {
    /// `inner`, sleeping on the calling thread between attempts.
    pub fn new(inner: S) -> Self {
        Self::with_sleeper(inner, std::thread::sleep)
    }

    /// `inner`, waiting between attempts with `sleep`.
    pub fn with_sleeper(inner: S, sleep: impl Fn(Duration) + Send + Sync + 'static) -> Self {
        Self {
            inner,
            sleep: Box::new(sleep),
        }
    }

    fn retry<T>(&self, op: impl Fn(&S) -> Result<T, FetchError>) -> Result<T, FetchError> {
        let mut delay = FIRST_BACKOFF;
        for _ in 0..MAX_RETRIES {
            match op(&self.inner) {
                Err(e) if e.is_server_error() => {
                    tracing::debug!(url = %e.url, reason = %e.reason, "retrying a server error");
                    (self.sleep)(delay);
                    delay = delay.saturating_mul(2);
                }
                other => return other,
            }
        }
        op(&self.inner).map_err(|mut e| {
            if e.is_server_error() {
                e.reason = format!("{} (after {MAX_RETRIES} retries)", e.reason);
            }
            e
        })
    }
}

impl<S: ReleaseSource> ReleaseSource for Retrying<S> {
    fn asset_url(&self, tag: &str, file: &str) -> String {
        self.inner.asset_url(tag, file)
    }

    fn asset(&self, tag: &str, file: &str, max_bytes: u64) -> Result<Option<Vec<u8>>, FetchError> {
        self.retry(|s| s.asset(tag, file, max_bytes))
    }

    fn content_tags(&self) -> Result<Vec<String>, FetchError> {
        self.retry(|s| s.content_tags())
    }

    fn release(&self, tag: &str) -> Result<Release, FetchError> {
        self.retry(|s| s.release(tag))
    }
}
