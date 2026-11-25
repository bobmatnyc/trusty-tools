//! Graceful quit of the granted app behind one grant flag (#11696 Phase D).
//!
//! Why: an agent that finished with a granted app sometimes has to close it.
//! The driver's own `kill_app` is `kill -9` of any pid and stays DENY; this
//! broker tool asks only the granted app, by pid and bundle id, to quit the
//! way its own Quit menu item would, so it can save or prompt first.
//! What: [`QUIT_TOOL`] is a broker tool with a FILTER row
//! ([`crate::policy::BROKER_RULES`]) checked by `pid_in_scope` and
//! [`quit_args`]: the call names exactly the granted `pid` and the granted
//! `bundle_id`. It needs the grant flag [`GRANT_FLAG`]; it is neither a
//! default tool nor grantable by its own name. The broker re-checks that the
//! pid is still the granted app (`check_same`) and then [`run`] asks a
//! [`Quitter`] to quit that pid. [`SystemQuitter`] sends the standard quit
//! Apple event through `osascript -l JavaScript`, addressed by pid, and
//! refuses when the app at that pid does not report the granted bundle id.
//! No signal is sent; the app may decline or ask to save. The script marks
//! the bundle-id check on stderr before it sends quit; a run still waiting
//! for the app's answer at [`TIMEOUT`] after that mark is
//! [`QuitOutcome::Pending`] (quit delivered, app not yet answered, typically
//! a save prompt), not an error.
//! Test: `tests/app_quit.rs`.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::policy::{PolicyError, Scope};

/// Ask the granted app to quit.
pub const QUIT_TOOL: &str = "quit_app";
/// The grant name that admits [`QUIT_TOOL`].
pub const GRANT_FLAG: &str = "app_quit";
/// How long `osascript` may run; a save prompt can hold the reply.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// The reply hint of a [`QuitOutcome::Pending`] quit.
pub const PENDING_HINT: &str = "the app received the quit request and has not answered; \
it may be showing a save prompt to the user";

/// The stderr line the script writes after the bundle-id check, before quit.
pub const CHECKED_MARK: &str = "trusty-quit-checked";

/// JXA run handler: address the app by pid, prove its bundle id, mark, send quit.
/// `console.log` writes to stderr under `osascript`.
const SCRIPT: &str = "function run(argv) {\
  var app = Application(Number(argv[0]));\
  if (app.id() !== argv[1]) { throw new Error('bundle id mismatch'); }\
  console.log('trusty-quit-checked');\
  app.quit();\
}";

/// What a delivered quit request led to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitOutcome {
    /// The app answered the quit request and accepted it.
    Accepted,
    /// The quit was sent after the bundle-id check, but the app had not
    /// answered by the deadline; it is typically showing a save prompt.
    Pending,
}

/// Asks one running app to quit.
pub trait Quitter: Send + Sync {
    /// Ask the app at `pid`, which must report `bundle_id`, to quit gracefully.
    fn quit(&self, pid: u32, bundle_id: &str) -> Result<QuitOutcome, String>;
}

/// The quit Apple event through `/usr/bin/osascript`.
pub struct SystemQuitter {
    /// The program, `/usr/bin/osascript` outside tests.
    pub program: String,
    /// The deadline of one run.
    pub timeout: Duration,
}

impl Default for SystemQuitter {
    fn default() -> SystemQuitter {
        SystemQuitter {
            program: "/usr/bin/osascript".into(),
            timeout: TIMEOUT,
        }
    }
}

impl Quitter for SystemQuitter {
    /// Run the script; see [`QuitOutcome`] for the two good endings.
    ///
    /// What: exit 0 is `Accepted`; a non-zero exit is an error (bundle id
    /// mismatch, no app at the pid, or the app declined). A run still going
    /// at the deadline is killed; it is `Pending` when stderr carried
    /// [`CHECKED_MARK`] (quit already sent) and an error otherwise.
    /// Test: `system_quitter_timeout_after_check_is_pending`,
    /// `system_quitter_timeout_before_check_is_an_error`.
    fn quit(&self, pid: u32, bundle_id: &str) -> Result<QuitOutcome, String> {
        let start = Instant::now();
        // Arguments go through argv, never into the script text.
        let mut child = Command::new(&self.program)
            .args([
                "-l",
                "JavaScript",
                "-e",
                SCRIPT,
                &pid.to_string(),
                bundle_id,
            ])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("osascript: {e}"))?;
        let stderr = child.stderr.take().ok_or("osascript: no stderr")?;
        // Reads to EOF, which comes at exit or kill; answers whether the mark was seen.
        let reader = std::thread::spawn(move || {
            BufReader::new(stderr)
                .lines()
                .map_while(Result::ok)
                .any(|l| l.trim() == CHECKED_MARK)
        });
        let status = loop {
            match child.try_wait().map_err(|e| format!("osascript: {e}"))? {
                Some(status) => break Some(status),
                None if start.elapsed() >= self.timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        };
        let checked = reader.join().unwrap_or(false);
        match status {
            Some(s) if s.success() => Ok(QuitOutcome::Accepted),
            Some(s) => Err(format!("osascript: {s}")),
            None if checked => Ok(QuitOutcome::Pending),
            None => Err(format!(
                "osascript: timed out after {} ms before the bundle id was checked",
                self.timeout.as_millis()
            )),
        }
    }
}

/// FILTER check of [`QUIT_TOOL`]: exactly `pid` and the granted `bundle_id`.
///
/// Why: the bundle id is named so a call cannot quit "whatever runs at this
/// pid"; it must also be the app the operator granted.
/// Test: `quit_args_are_checked`.
pub fn quit_args(args: &Map<String, Value>, scope: &Scope) -> Result<(), PolicyError> {
    if let Some(extra) = args
        .keys()
        .find(|k| !matches!(k.as_str(), "pid" | "bundle_id"))
    {
        return Err(PolicyError {
            code: "quit_arg",
            detail: format!("{extra} is not allowed"),
        });
    }
    let bundle = args.get("bundle_id");
    if bundle
        .and_then(Value::as_str)
        .is_some_and(|b| scope.bundle_ids.contains(b))
    {
        return Ok(());
    }
    let shown = bundle.map_or_else(|| "None".to_owned(), Value::to_string);
    Err(PolicyError {
        code: "app_out_of_scope",
        detail: format!("bundle_id {shown} is not the granted app"),
    })
}

/// The broker reply of a checked quit, and its audit fields.
///
/// Postcondition: `ok` only when the quit was sent to this `pid` and
/// `bundle_id`; a failure is `quit_error`. `status` is `accepted` when the
/// app answered, or `pending` when it had not answered by the deadline (a
/// save prompt, typically); `pending` carries a `hint`. Neither says the
/// app has exited.
/// Test: `quit_runs_only_for_the_granted_app`, `pending_quit_is_not_an_error`,
/// `quitter_failure_is_reported`.
pub fn run(q: &dyn Quitter, pid: u32, bundle_id: &str) -> (Value, Map<String, Value>) {
    let mut audit = Map::new();
    audit.insert("quit_bundle_id".into(), json!(bundle_id));
    let status = match q.quit(pid, bundle_id) {
        Ok(QuitOutcome::Accepted) => "accepted",
        Ok(QuitOutcome::Pending) => "pending",
        Err(e) => {
            audit.insert("quit_status".into(), json!("error"));
            return (
                json!({"ok": false, "code": "quit_error", "error": e}),
                audit,
            );
        }
    };
    audit.insert("quit_status".into(), json!(status));
    let mut sc = json!({"quit_requested": true, "status": status,
        "pid": pid, "bundle_id": bundle_id});
    if status == "pending" {
        sc["hint"] = json!(PENDING_HINT);
    }
    let reply = json!({"ok": true, "result": {"structuredContent": sc}});
    (reply, audit)
}
