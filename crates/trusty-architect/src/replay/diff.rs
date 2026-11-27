//! Replaying a recording against a target and diffing the four channels.
//!
//! Why: #12026 AC2. A port passes its gate only when stdout, stderr, the exit
//! code and every written file match the recording byte for byte; a one-byte
//! change in any of them must be reported, never passed.
//! What: [`replay`] lays the recorded input files into a fresh sandbox, runs
//! the target with the recorded argv, stdin and env, scrubs the output with
//! the same path tokens the recorder used, and returns one [`Mismatch`] per
//! differing channel. A `.py` target runs under the recorder's open probe, so
//! a Python self-replay sees the same interpreter frames. A recording with
//! fakes replays with the same stubs first on `PATH`, and its call list is
//! one more channel (#12028). A recording with a clock replays with its
//! instant in [`super::CLOCK_ENV`], which a `.py` target's probe freezes on
//! and a Rust replay entry point reads (#12028). Written files that
//! [`super::skip_written_file`] names are compared on neither side (#12087).
//! Test: `replay_reports_a_one_byte_change_in_each_channel`,
//! `replay_of_an_unchanged_program_is_clean`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use super::fakes::{FakeDir, calls_diff};
use super::record::{Python, run_scrubber};
use super::sandbox::{Sandbox, changes};
use super::scrub::leaks;
use super::{CLOCK_ENV, Recording, ReplayError, skip_written_file};

/// Longest excerpt, in bytes, of a differing line shown in a [`Mismatch`].
const EXCERPT: usize = 100;

/// The program a recording is replayed against.
#[derive(Debug, Clone)]
pub struct Target {
    /// The binary, or a `.py` script (run with `python3` under the probe).
    pub program: PathBuf,
    /// Arguments placed before the recorded argv, e.g. a subcommand.
    pub prefix: Vec<String>,
}

/// One replayed channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Channel {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
    /// The exit code.
    Exit,
    /// A written file, by relative path.
    File(String),
    /// The calls to fake executables, in order (#12028).
    FakeCalls,
}

/// A channel that differs from the recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch {
    /// Which channel.
    pub channel: Channel,
    /// Where and how it differs; an excerpt whose line would leak is withheld.
    pub detail: String,
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.channel {
            Channel::File(p) => write!(f, "file {p}: {}", self.detail),
            other => write!(f, "{other:?}: {}", self.detail),
        }
    }
}

/// The text around byte `at` of `s`, within its line, at most [`EXCERPT`]
/// bytes; withheld when the whole line holds a secret shape, a session token
/// or a local path.
fn excerpt(s: &str, at: usize) -> String {
    let floor = |mut i: usize| {
        i = i.min(s.len());
        while !s.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let at = floor(at);
    let line_start = s[..at].rfind('\n').map_or(0, |i| i + 1);
    let line_end = s[at..].find('\n').map_or(s.len(), |i| at + i);
    let start = floor(at.saturating_sub(EXCERPT / 2).max(line_start));
    let end = floor((start + EXCERPT).min(line_end));
    // #12026: test the whole line, not the window; a window can cut off the
    // `password:` keyword and show the value alone.
    if leaks(&s[line_start..line_end]) {
        return "<withheld>".into();
    }
    format!("{:?}", &s[start..end])
}

/// `None` when equal, else the first differing byte, its line, and the text
/// around it on both sides.
pub(super) fn text_diff(want: &str, got: &str) -> Option<String> {
    if want == got {
        return None;
    }
    let at = want
        .bytes()
        .zip(got.bytes())
        .position(|(a, b)| a != b)
        .unwrap_or(want.len().min(got.len()));
    let line_no = want.as_bytes()[..at]
        .iter()
        .filter(|b| **b == b'\n')
        .count()
        + 1;
    Some(format!(
        "differs at byte {at} (line {line_no}; {} vs {} bytes): want {}, got {}",
        want.len(),
        got.len(),
        excerpt(want, at),
        excerpt(got, at),
    ))
}

/// Replays `rec` against `target`; an empty list means every channel matched.
///
/// Why: the slice gate (see the module doc).
/// What: sandbox from `rec.files`, run, scrub, compare stdout, stderr, exit
/// and the written-file set. Output that is not UTF-8 is a mismatch, not an
/// error. `parent_env` supplies only `PATH` (to find `python3`) and `HOME`
/// (to scrub); neither reaches the target.
/// Test: `replay_reports_a_one_byte_change_in_each_channel`.
pub fn replay(
    rec: &Recording,
    target: &Target,
    parent_env: &BTreeMap<String, String>,
    timeout: Duration,
) -> Result<Vec<Mismatch>, ReplayError> {
    let sb = Sandbox::new("replay")?;
    for (rel, content) in &rec.files {
        sb.put(rel, content)?;
    }
    let before = sb.snapshot()?;
    let python = Python::for_program(&target.program, parent_env)?;
    let mut env = rec.env.clone();
    let (program, mut prefix) = match &python {
        Some(py) => {
            env.insert(
                "TRUSTY_REPLAY_PROBE".into(),
                py.log.to_string_lossy().into_owned(),
            );
            (py.interpreter.clone(), py.prefix.clone())
        }
        None => (target.program.clone(), Vec::new()),
    };
    prefix.extend(target.prefix.iter().cloned());
    if let Some(t) = rec.clock {
        env.insert(CLOCK_ENV.into(), t.to_string()); // See #12028
    }
    // See #12028: the recorded stubs, so the target meets the same answers.
    let fake_dir = match &rec.fakes {
        Some(f) => Some(FakeDir::new(&f.stubs, &|t| sb.expand(t))?),
        None => None,
    };
    let bin = fake_dir.as_ref().map(FakeDir::bin);
    let fin = sb.run(&program, &prefix, &rec.argv, &env, &rec.stdin, timeout, bin)?;
    let after = sb.snapshot()?;
    let scrub = run_scrubber(
        &sb,
        &target.program,
        python.as_ref(),
        fake_dir.as_ref(),
        parent_env,
    );
    let as_text = |b: Vec<u8>| String::from_utf8(b).map(|s| scrub.text(&s));

    let mut out = Vec::new();
    let mut push = |channel, detail| out.push(Mismatch { channel, detail });
    for (channel, want, got) in [
        (Channel::Stdout, &rec.stdout, fin.stdout),
        (Channel::Stderr, &rec.stderr, fin.stderr),
    ] {
        match as_text(got) {
            Ok(got) => {
                if let Some(d) = text_diff(want, &got) {
                    push(channel, d);
                }
            }
            Err(_) => push(channel, "output is not UTF-8".into()),
        }
    }
    match fin.status.code() {
        Some(code) if code == rec.exit => {}
        Some(code) => push(Channel::Exit, format!("want {}, got {code}", rec.exit)),
        None => push(Channel::Exit, format!("want {}, got a signal", rec.exit)),
    }
    if let (Some(want), Some(dir)) = (&rec.fakes, &fake_dir) {
        let got: Vec<Vec<String>> = dir
            .calls()?
            .iter()
            .map(|c| c.iter().map(|a| scrub.text(a)).collect())
            .collect();
        if let Some(d) = calls_diff(&want.calls, &got, &want.stubs) {
            push(Channel::FakeCalls, d);
        }
    }
    let got_writes: BTreeMap<String, Option<Result<String, _>>> = changes(&before, &after)
        .into_iter()
        .map(|(k, v)| (scrub.text(&k), v.map(as_text)))
        .collect();
    // #12087: the store lock and change logs are compared on neither side.
    let compared = |k: &&String| !skip_written_file(k.rsplit('/').next().unwrap_or(k));
    let keys: BTreeSet<&String> = rec
        .writes
        .keys()
        .chain(got_writes.keys())
        .filter(compared)
        .collect();
    for k in keys {
        let file = Channel::File(k.clone());
        match (rec.writes.get(k), got_writes.get(k)) {
            (Some(Some(want)), Some(Some(Ok(got)))) => {
                if let Some(d) = text_diff(want, got) {
                    push(file, d);
                }
            }
            (Some(None), Some(None)) => {}
            (_, Some(Some(Err(_)))) => push(file, "written content is not UTF-8".into()),
            (Some(Some(_)), Some(None)) => push(file, "want written, got deleted".into()),
            (Some(None), Some(Some(_))) => push(file, "want deleted, got written".into()),
            (Some(_), None) => push(file, "want written, got untouched".into()),
            (None, Some(_)) => push(file, "unexpected write".into()),
            (None, None) => {}
        }
    }
    Ok(out)
}
