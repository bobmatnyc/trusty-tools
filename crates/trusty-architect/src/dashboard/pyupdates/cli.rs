//! `dashboard_updates.py`'s `main`: `note` and the ingest pass (#12029 P2h).
//!
//! Why: the drop-in for `python3 scripts/dashboard_updates.py [note ...]`
//! must append the same note line, print the same JSON and exit the same way.
//! What: [`run`]: the store root is `$DASHBOARD_ROOT`, else
//! `<checkout>/local/owner-queue`; the inbox `$DASHBOARD_INBOX`, else
//! `<checkout>/inbox`. `note <task-id> "<text>" [--by <who>]` appends one
//! line first (bad usage: one stderr line, exit 2); then one [`ingest`] pass
//! prints `json.dumps` of its answer, exit 0. Any other argument is ignored,
//! as in the script. An exception the script does not catch prints its last
//! traceback line, exit 1.
//! Test: `dashboard_updates_recordings_replay_clean_against_the_port`,
//! `note_bad_usage_exits_2_and_writes_nothing`.

use std::path::PathBuf;
use std::sync::Arc;

use super::{SINCE_HOURS, add_note, ingest};
use crate::clock::Clock;
use crate::dashboard::store::py::{Options as StoreOptions, PyStore};
use crate::spec_drift::Outcome;

/// The usage line of `note`.
pub const NOTE_USAGE: &str = "usage: dashboard_updates.py note <task-id> \"<text>\" [--by <who>]\n";

/// What the caller supplies besides argv.
pub struct Options<'a> {
    /// The script's checkout: `local/owner-queue`, `inbox` and `schemas/dashboard` under it.
    pub checkout: PathBuf,
    /// The working directory relative paths resolve against.
    pub cwd: PathBuf,
    /// The process environment.
    pub env: &'a dyn Fn(&str) -> Option<String>,
    /// `datetime.now(timezone.utc)`.
    pub clock: Arc<dyn Clock + Send + Sync>,
}

fn out(stdout: String, stderr: String, code: i32) -> Outcome {
    Outcome {
        stdout,
        stderr,
        code,
    }
}

/// `note`'s arguments: `(task, text, by)`, or `None` for bad usage.
fn note_args(args: &[String]) -> Option<(String, String, String)> {
    let mut rest = args.to_vec();
    let mut by = "architect".to_owned();
    if let Some(i) = rest.iter().position(|a| a == "--by") {
        by = rest.get(i + 1).cloned().unwrap_or_default();
        rest.drain(i..(i + 2).min(rest.len()));
    }
    match rest.as_slice() {
        [task, text] if !by.is_empty() => Some((task.clone(), text.clone(), by)),
        _ => None,
    }
}

/// Why: the drop-in for the script's command line.
/// What: see the module doc.
/// Test: `dashboard_updates_recordings_replay_clean_against_the_port`.
pub fn run(args: &[String], o: &Options) -> Outcome {
    let env = |k: &str| (o.env)(k).filter(|v| !v.is_empty());
    let root =
        env("DASHBOARD_ROOT").map_or_else(|| o.checkout.join("local/owner-queue"), PathBuf::from);
    let inbox = env("DASHBOARD_INBOX").map_or_else(|| o.checkout.join("inbox"), PathBuf::from);
    let inbox = o.cwd.join(inbox);
    if args.first().is_some_and(|a| a == "note") {
        let Some((task, text, by)) = note_args(&args[1..]) else {
            return out(String::new(), NOTE_USAGE.into(), 2);
        };
        if let Err(f) = add_note(&task, &text, &by, &inbox, o.clock.now_unix()) {
            return out(String::new(), format!("{f}\n"), 1);
        }
    }
    let opts = StoreOptions {
        actor: Some("poller".into()),
        ..StoreOptions::default()
    };
    let schemas = o.checkout.join("schemas/dashboard");
    let root = root.to_string_lossy();
    let store = match PyStore::new(&root, opts, o.env, &o.cwd, schemas, o.clock.clone()) {
        Ok(s) => s,
        Err(e) => return out(String::new(), format!("{}\n", super::raised(&e)), 1),
    };
    let done = ingest(&store, &inbox, o.clock.now_unix(), SINCE_HOURS);
    let stderr = store.take_stderr();
    match done {
        Ok(r) => out(format!("{}\n", r.json()), stderr, 0),
        Err(f) => out(String::new(), format!("{stderr}{f}\n"), 1),
    }
}
