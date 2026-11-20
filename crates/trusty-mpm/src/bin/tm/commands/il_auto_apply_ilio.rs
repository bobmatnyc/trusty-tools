//! The `ilio.py apply` path of IL auto-apply (G0b gap ruling).
//!
//! Why: the Rust writer covers `next`, `close` and `act` only; the Architect
//! ruled that a block carrying any other applied field goes, whole, to the
//! existing `ilio.py apply` rather than a port.
//! What: [`ilio_script`] finds `<architect dir>/scripts/ilio.py`
//! (`$ILIO_SCRIPT`, else `architect_store::architect_dir`). [`run_ilio`]
//! runs `python3 <script> apply` with no shell, the block's fence alone on
//! stdin (written from a thread, inside the deadline) and
//! `ILIO_PROJECT`/`DATG_SESSION`/`ILIO_WHO` naming the store, and kills it
//! at the timeout. Every failure is a rule code; none is ever "applied".
//! Test: `ilio_failures_are_never_applied`,
//! `a_block_with_new_runs_ilio_once_with_the_block_on_stdin`,
//! `a_hung_ilio_with_a_large_body_times_out_inside_the_deadline`.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long one `ilio.py apply` may run. With the 1.5 s lock wait it totals
/// 3.5 s, inside the installed `Stop` hook's 5 s kill.
pub(crate) const ILIO_TIMEOUT: Duration = Duration::from_secs(2);

/// `ilio.py`: `$ILIO_SCRIPT`, else `<architect dir>/scripts/ilio.py`.
///
/// Test: `ilio_script_comes_from_the_env`.
pub(crate) fn ilio_script(
    env: &dyn Fn(&str) -> Option<String>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(p) = env("ILIO_SCRIPT").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let dir = trusty_mpm::core::architect_store::architect_dir(env, home)?;
    Some(dir.join("scripts").join("ilio.py"))
}

/// What the store and actor are for one ilio run.
pub(crate) struct IlioCall<'a> {
    pub(crate) script: Option<&'a Path>,
    pub(crate) project: &'a Path,
    pub(crate) session: &'a str,
    pub(crate) who: &'a str,
    pub(crate) timeout: Duration,
}

/// Runs `ilio.py apply` on `text`. `Ok` only on exit 0; `Err((code, why))`
/// otherwise: `ILIO-MISSING`, `ILIO-TIMEOUT`, ilio's own `IL-REJECT` code,
/// or `ILIO-EXIT`.
pub(crate) fn run_ilio(call: &IlioCall<'_>, text: &str) -> Result<String, (String, String)> {
    let fail = |c: &str, why: String| Err((c.to_owned(), why));
    let Some(script) = call.script.filter(|p| p.is_file()) else {
        let at = call
            .script
            .map_or("no path".into(), |p| p.display().to_string());
        return fail("ILIO-MISSING", format!("ilio.py not found: {at}"));
    };
    let mut cmd = Command::new("python3");
    cmd.arg(script)
        .arg("apply")
        .env("ILIO_PROJECT", call.project)
        .env("ILIO_WHO", call.who)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !call.session.is_empty() {
        cmd.env("DATG_SESSION", call.session);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return fail("ILIO-MISSING", format!("cannot run python3: {e}")),
    };
    // G0b critic: only the fence ilio parses, written from a thread so a
    // child that never reads cannot hold the hook past the deadline. The
    // kill below closes the pipe and ends the write; a write error shows up
    // as ilio's own exit status.
    let fence = trusty_architect::ledger::il::fence_body(text)
        .map_or_else(String::new, |b| format!("```il\n{b}```\n"));
    let feed = child.stdin.take().map(|mut stdin| {
        std::thread::spawn(move || {
            let _ = stdin.write_all(fence.as_bytes());
        })
    });
    let pipe = |r: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut r) = r {
                let _ = r.read_to_string(&mut s);
            }
            s
        })
    };
    let out = pipe(child.stdout.take().map(|s| Box::new(s) as _));
    let err = pipe(child.stderr.take().map(|s| Box::new(s) as _));
    let deadline = Instant::now() + call.timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            other => {
                let _ = child.kill();
                let _ = child.wait();
                // The kill closed the pipe: the writer thread ends.
                if let Some(feed) = feed {
                    let _ = feed.join();
                }
                if let Err(e) = other {
                    return fail("ILIO-EXIT", format!("ilio.py apply wait failed: {e}"));
                }
                let secs = call.timeout.as_secs_f32();
                return fail(
                    "ILIO-TIMEOUT",
                    format!("ilio.py apply killed after {secs}s"),
                );
            }
        }
    };
    if let Some(feed) = feed {
        let _ = feed.join();
    }
    let (out, err) = (
        out.join().unwrap_or_default(),
        err.join().unwrap_or_default(),
    );
    if status.success() {
        return Ok(out.trim().to_owned());
    }
    let code = out
        .trim()
        .strip_prefix("IL-REJECT ")
        .and_then(|r| r.split_whitespace().next())
        .unwrap_or("ILIO-EXIT")
        .to_owned();
    let why = format!("ilio.py apply exit {status}: {} {}", out.trim(), err.trim());
    Err((code, why.chars().take(400).collect()))
}
