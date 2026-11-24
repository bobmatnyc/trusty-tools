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
//! No signal is sent; the app may decline or ask to save.
//! Test: `tests/app_quit.rs`.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::clipboard_io::wait_until;
use crate::policy::{PolicyError, Scope};

/// Ask the granted app to quit.
pub const QUIT_TOOL: &str = "quit_app";
/// The grant name that admits [`QUIT_TOOL`].
pub const GRANT_FLAG: &str = "app_quit";
/// How long `osascript` may run; a save prompt can hold the reply.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// JXA run handler: address the app by pid, prove its bundle id, send quit.
const SCRIPT: &str = "function run(argv) {\
  var app = Application(Number(argv[0]));\
  if (app.id() !== argv[1]) { throw new Error('bundle id mismatch'); }\
  app.quit();\
}";

/// Asks one running app to quit.
pub trait Quitter: Send + Sync {
    /// Ask the app at `pid`, which must report `bundle_id`, to quit gracefully.
    fn quit(&self, pid: u32, bundle_id: &str) -> Result<(), String>;
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
    fn quit(&self, pid: u32, bundle_id: &str) -> Result<(), String> {
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
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("osascript: {e}"))?;
        wait_until(&mut child, "osascript", start, self.timeout)
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
/// Postcondition: `ok` only when the quitter accepted the request for this
/// `pid` and `bundle_id`; a failure is `quit_error`. The reply says the quit
/// was requested, not that the app exited: it may prompt or decline.
/// Test: `quit_runs_only_for_the_granted_app`, `quitter_failure_is_reported`.
pub fn run(q: &dyn Quitter, pid: u32, bundle_id: &str) -> (Value, Map<String, Value>) {
    let mut audit = Map::new();
    audit.insert("quit_bundle_id".into(), json!(bundle_id));
    let reply = match q.quit(pid, bundle_id) {
        Ok(()) => json!({"ok": true, "result": {"structuredContent": {
            "quit_requested": true, "pid": pid, "bundle_id": bundle_id}}}),
        Err(e) => json!({"ok": false, "code": "quit_error", "error": e}),
    };
    (reply, audit)
}
