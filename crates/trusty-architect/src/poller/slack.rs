//! The watchdog's direct push to the owner: a Slack DM from the MPM app bot
//! (#9779, owner ruling D5t).
//!
//! Why: a failure the Architect has not cleared in 30 min must reach the
//! owner without the Architect's help. The crate stays a leaf with no HTTP
//! client dependency, so the send goes through `curl`.
//! What: [`SlackDm`] posts `chat.postMessage` with the recipient's user id as
//! the channel. It reads the token from [`TOKEN_VAR`] at send time and hands
//! it to curl on stdin as a config line, so it never appears in argv, a log
//! line or a [`PushError`]. A missing token, a timeout, a curl failure, a
//! non-200 status and a Slack `ok: false` are each a [`PushError`] kind.
//! Test: `slack_dm_never_leaks_the_token`, `slack_dm_ok_reply_is_sent`
//! (tests/push_slack.rs).

use std::io;
use std::process::Command;
use std::time::Duration;

use serde_json::{Value, json};

use super::steps::scripts::ChildOutput;
use crate::process::{RunFailure, run_bounded_input};

/// Environment variable holding the MPM app's bot token, read at send time.
pub const TOKEN_VAR: &str = "SLACK_MPM_BOT_OAUTH_TOKEN";
/// Slack's message endpoint.
pub const SLACK_POST_URL: &str = "https://slack.com/api/chat.postMessage";
/// Bound on one send.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// Sends one push message to `recipient`.
pub trait PushSender {
    /// Sends `text` to `recipient` (a Slack user id).
    ///
    /// # Errors
    /// A [`PushError`]; its text never holds the token.
    fn send(&self, recipient: &str, text: &str) -> Result<(), PushError>;
}

/// Why a push was not delivered. No variant carries the token.
#[derive(Debug, thiserror::Error)]
pub enum PushError {
    /// The token variable is unset or empty.
    #[error("{TOKEN_VAR} is unset or empty")]
    MissingToken,
    /// The token holds characters a curl config line cannot carry.
    #[error("{TOKEN_VAR} holds characters that are not allowed")]
    BadToken,
    /// The send did not finish within its bound.
    #[error("send timed out")]
    Timeout,
    /// Slack answered with a status other than 200.
    #[error("HTTP status {0}")]
    Http(u16),
    /// Slack answered `ok: false` with this error code.
    #[error("Slack refused the message: {0}")]
    Slack(String),
    /// curl could not run or its reply could not be read.
    #[error("transport: {0}")]
    Transport(String),
}

impl PushError {
    /// The alert fingerprint of this failure kind.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::MissingToken => "token",
            Self::BadToken => "token-format",
            Self::Timeout => "timeout",
            Self::Http(_) => "http",
            Self::Slack(_) => "slack",
            Self::Transport(_) => "transport",
        }
    }
}

/// Runs curl with bytes on stdin; the seam the tests fake.
pub trait CurlRun {
    /// Runs `argv` with `stdin`, bounded by `timeout`.
    ///
    /// # Errors
    /// The child could not start, was not waited on, or timed out
    /// ([`io::ErrorKind::TimedOut`]).
    fn run(&self, argv: &[String], stdin: &[u8], timeout: Duration) -> io::Result<ChildOutput>;
}

/// [`CurlRun`] over `std::process`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemCurl;

impl CurlRun for SystemCurl {
    fn run(&self, argv: &[String], stdin: &[u8], timeout: Duration) -> io::Result<ChildOutput> {
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| io::Error::other("empty argv"))?;
        let mut cmd = Command::new(program);
        cmd.args(args);
        let done = run_bounded_input(cmd, Some(stdin.to_vec()), timeout).map_err(|f| match f {
            RunFailure::Spawn(e) | RunFailure::Wait(e) => e,
            RunFailure::Timeout => io::Error::new(io::ErrorKind::TimedOut, "timed out"),
        })?;
        Ok(ChildOutput {
            code: done.status.code(),
            stdout: String::from_utf8_lossy(&done.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&done.stderr).into_owned(),
        })
    }
}

/// The token from [`TOKEN_VAR`], read now.
pub fn env_token() -> Option<String> {
    std::env::var(TOKEN_VAR).ok()
}

/// A Slack DM sender; see the module docs.
pub struct SlackDm<'a> {
    /// Runs curl.
    pub curl: &'a dyn CurlRun,
    /// Reads the token at send time ([`env_token`] in production).
    pub token: &'a dyn Fn() -> Option<String>,
    /// The endpoint ([`SLACK_POST_URL`] in production).
    pub url: String,
    /// Bound on one send.
    pub timeout: Duration,
}

impl PushSender for SlackDm<'_> {
    fn send(&self, recipient: &str, text: &str) -> Result<(), PushError> {
        let token = (self.token)()
            .filter(|t| !t.is_empty())
            .ok_or(PushError::MissingToken)?;
        if !token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(PushError::BadToken);
        }
        let body = json!({"channel": recipient, "text": text}).to_string();
        let secs = self.timeout.as_secs().max(1).to_string();
        let argv: Vec<String> = [
            "curl",
            "-sS",
            "--max-time",
            &secs,
            "--config",
            "-",
            "-X",
            "POST",
            "-H",
            "Content-Type: application/json; charset=utf-8",
            "--data-binary",
            &body,
            "-w",
            "\n%{http_code}",
            &self.url,
        ]
        .map(str::to_owned)
        .to_vec();
        let stdin = format!("header = \"Authorization: Bearer {token}\"\n");
        // A little over curl's own bound, so curl reports the timeout itself.
        let bound = self.timeout + Duration::from_secs(2);
        let out = match self.curl.run(&argv, stdin.as_bytes(), bound) {
            Ok(out) => out,
            Err(e) if e.kind() == io::ErrorKind::TimedOut => return Err(PushError::Timeout),
            Err(e) => return Err(PushError::Transport(redact(&e.to_string(), &token))),
        };
        parse_reply(&out).map_err(|e| match e {
            PushError::Transport(m) => PushError::Transport(redact(&m, &token)),
            PushError::Slack(m) => PushError::Slack(redact(&m, &token)),
            other => other,
        })
    }
}

fn redact(text: &str, token: &str) -> String {
    text.replace(token, "[redacted]")
}

/// Reads curl's output: the reply body, a newline, then the HTTP status.
fn parse_reply(out: &ChildOutput) -> Result<(), PushError> {
    match out.code {
        Some(0) => {}
        Some(28) => return Err(PushError::Timeout), // curl: operation timed out
        Some(c) => return Err(PushError::Transport(format!("curl exit {c}"))),
        None => return Err(PushError::Transport("curl killed by a signal".to_owned())),
    }
    let (body, status) = out
        .stdout
        .rsplit_once('\n')
        .ok_or_else(|| PushError::Transport("reply has no status line".to_owned()))?;
    let status: u16 = status
        .trim()
        .parse()
        .map_err(|_| PushError::Transport("unreadable status".to_owned()))?;
    if status != 200 {
        return Err(PushError::Http(status));
    }
    let reply: Value = serde_json::from_str(body)
        .map_err(|_| PushError::Transport("reply is not JSON".to_owned()))?;
    if reply.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(());
    }
    let code = reply
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    Err(PushError::Slack(code.to_owned()))
}
