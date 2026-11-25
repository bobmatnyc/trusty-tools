//! The consent surface: the helper dialog, the one-time codes and the strike counters.
//!
//! Why: the operator mints; an agent cannot mint for itself. A grant exists
//! only after the operator confirms a native dialog shown by a separate
//! helper process and types the code it shows into the CLI's `/dev/tty`
//! (plan section 6).
//! What: [`ConsentDialog`] is the dialog seam; [`OsascriptDialog`] is the real
//! one (`osascript display dialog` in a child process) and is built only by
//! the `trusty-computer broker` bin, so no test ever shows a dialog.
//! [`ask`] runs a dialog on its own thread and answers [`DialogOutcome::TimedOut`]
//! when it does not return in time, whatever the dialog does. [`Strikes`]
//! counts bad codes: five inside 60 s lock the path out. [`six_digits`] and
//! [`grant_code`] draw from `/dev/urandom`.
//! Test: `tests/consent_fail_closed.rs`.

use std::collections::VecDeque;
use std::io::{self, Read};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

/// Bad codes allowed inside [`STRIKE_WINDOW`] before a lockout.
pub const STRIKE_LIMIT: usize = 5;
/// The strike window, seconds.
pub const STRIKE_WINDOW: f64 = 60.0;
/// How long a dialog code may be entered after the dialog was confirmed, seconds.
pub const DIALOG_CODE_TTL: f64 = 120.0;
/// How long a `tc-` grant code may be redeemed, seconds.
pub const GRANT_CODE_TTL: f64 = 300.0;
/// How long the dialog waits for the operator.
pub const DIALOG_TIMEOUT: Duration = Duration::from_secs(60);

/// What a dialog asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// Mint a grant for one app.
    Grant,
    /// Clear a persisted lockout (#11460).
    Unlock,
}

/// What the dialog shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Prompt {
    pub kind: PromptKind,
    /// The resolved bundle id (never the requester's claim alone).
    pub bundle_id: String,
    /// The app's display name, as LaunchServices reports it for `pid`.
    pub app_name: String,
    pub pid: u32,
    pub tools: Vec<String>,
    pub ttl_secs: u64,
    /// The six-digit code the operator types into the CLI.
    pub code: String,
}

impl Prompt {
    /// The dialog text: the pid, the resolved app name and bundle for a grant.
    pub fn text(&self) -> String {
        match self.kind {
            PromptKind::Grant => format!(
                "Allow an agent to drive {} ({}, pid {}) for {} min?\nTools: {}\nCode: {}",
                self.app_name,
                self.bundle_id,
                self.pid,
                self.ttl_secs / 60,
                self.tools.join(", "),
                self.code
            ),
            PromptKind::Unlock => format!(
                "Clear the trusty-computer lockout and allow new grants?\nCode: {}",
                self.code
            ),
        }
    }
}

/// How a dialog ended.
#[derive(Debug, Clone, PartialEq)]
pub enum DialogOutcome {
    Confirmed,
    Denied,
    TimedOut,
    /// The helper could not run; refused like a denial.
    Failed(String),
}

/// The helper dialog seam.
pub trait ConsentDialog: Send + Sync {
    /// Show `prompt` and wait up to `timeout` for the operator.
    fn confirm(&self, prompt: &Prompt, timeout: Duration) -> DialogOutcome;
}

/// Run `dialog` on its own thread; no answer inside `timeout` is `TimedOut`.
///
/// Why: the broker must not trust a dialog to honour its own timeout.
/// Test: `dialog_timeout_refuses`.
pub fn ask(dialog: Arc<dyn ConsentDialog>, prompt: Prompt, timeout: Duration) -> DialogOutcome {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(dialog.confirm(&prompt, timeout));
    });
    rx.recv_timeout(timeout).unwrap_or(DialogOutcome::TimedOut)
}

/// The real dialog: `osascript display dialog` in a child process.
///
/// The bundle id reaches the script only after [`valid_bundle_id`] passed,
/// so it carries no quote or backslash.
#[derive(Debug, Default)]
pub struct OsascriptDialog;

impl ConsentDialog for OsascriptDialog {
    fn confirm(&self, prompt: &Prompt, timeout: Duration) -> DialogOutcome {
        let text = prompt.text().replace(['"', '\\'], "");
        let script = format!(
            "display dialog \"{text}\" with title \"trusty-computer\" buttons {{\"Deny\", \"Allow\"}} \
             default button \"Deny\" cancel button \"Deny\" giving up after {}",
            timeout.as_secs().max(1)
        );
        let out = Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(script)
            .stdin(Stdio::null())
            .output();
        match out {
            Err(e) => DialogOutcome::Failed(e.to_string()),
            Ok(o) if !o.status.success() => DialogOutcome::Denied,
            Ok(o) => {
                let said = String::from_utf8_lossy(&o.stdout);
                if said.contains("gave up:true") {
                    DialogOutcome::TimedOut
                } else if said.contains("button returned:Allow") {
                    DialogOutcome::Confirmed
                } else {
                    DialogOutcome::Denied
                }
            }
        }
    }
}

/// A bundle id the dialog can show: `[A-Za-z0-9.-]`, 1 to 255 bytes.
pub fn valid_bundle_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 255
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

/// Bad-code times inside the strike window.
#[derive(Debug, Default)]
pub struct Strikes {
    times: VecDeque<f64>,
}

impl Strikes {
    fn prune(&mut self, now: f64) {
        while self.times.front().is_some_and(|t| now - t >= STRIKE_WINDOW) {
            self.times.pop_front();
        }
    }

    /// True while [`STRIKE_LIMIT`] bad codes fall inside the window.
    pub fn locked(&mut self, now: f64) -> bool {
        self.prune(now);
        self.times.len() >= STRIKE_LIMIT
    }

    /// Record one bad code.
    pub fn strike(&mut self, now: f64) {
        self.prune(now);
        self.times.push_back(now);
    }
}

fn random<const N: usize>() -> io::Result<[u8; N]> {
    let mut bytes = [0u8; N];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// A uniform six-digit code.
pub fn six_digits() -> io::Result<String> {
    loop {
        let n = u32::from_le_bytes(random::<4>()?);
        // Rejection sampling: drop the top partial range, so no digit is favoured.
        if n < 4_294_000_000 {
            return Ok(format!("{:06}", n % 1_000_000));
        }
    }
}

/// A one-use grant code: `tc-` plus 120 random bits.
pub fn grant_code() -> io::Result<String> {
    Ok(format!("tc-{}", hex::encode(random::<15>()?)))
}

/// A challenge id for a confirmed dialog.
pub fn challenge_id() -> io::Result<String> {
    Ok(format!("ch-{}", hex::encode(random::<12>()?)))
}

/// A uuid4 string, for an MCP call that names no `request_id`.
pub fn uuid4() -> io::Result<String> {
    let mut b = random::<16>()?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex::encode(b);
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    ))
}

/// Compare two codes without an early exit.
pub fn same_code(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}
