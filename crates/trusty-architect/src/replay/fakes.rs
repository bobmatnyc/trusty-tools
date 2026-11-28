//! Fake executables a recording declares, laid out as shell stubs (#12028).
//!
//! Why: P1 scripts call `gh`, `git`, `curl`, `tmux` and `rg`. The sandbox
//! `PATH` hides them, and a real network or fleet call would make a
//! recording irreproducible. A fake stores its behaviour in the recording, so
//! record and replay give the program the same answers on any host.
//! What: [`FakeDir`] writes one `/bin/sh` stub per declared fake into a
//! directory outside the sandbox root, plus a trap stub for each
//! [`GUARDED`] name the recording does not declare. Every stub appends its
//! argv to a call log (NUL-separated; an exec argument holds no NUL). A
//! declared stub answers with the first [`FakeAnswer`] whose `argv` matches
//! (or that has none); a trap prints an error and exits 127. The sandbox puts
//! the stub directory first on `PATH`.
//! Limit: only a `PATH` lookup reaches a stub. A program that runs a binary
//! by absolute path, or an undeclared name outside [`GUARDED`], reaches the
//! host binary.
//! Test: `tests/replay_fakes.rs`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::ReplayError;
use super::sandbox::Sandbox;
use super::scrub::leaks;

/// Names that get a trap stub when a recording declares fakes but not them:
/// network, VCS, terminal and fleet tools whose real run reads live state.
pub const GUARDED: [&str; 26] = [
    "aws",
    "brew",
    "cargo",
    "claude",
    "curl",
    "docker",
    "gcloud",
    "gh",
    "git",
    "kubectl",
    "nc",
    "npm",
    "npx",
    "open",
    "osascript",
    "pip",
    "pip3",
    "rg",
    "rsync",
    "scp",
    "security",
    "ssh",
    "tm",
    "tmux",
    "trusty-mpm",
    "wget",
];

/// Exit code of a trap stub and of a fake whose answers all miss.
const MISS: i32 = 127;

/// One canned answer of a fake executable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FakeAnswer {
    /// Arguments after the name that select this answer; `None` matches any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argv: Option<Vec<String>>,
    /// Bytes the fake writes to stdout.
    #[serde(default)]
    pub stdout: String,
    /// Bytes the fake writes to stderr.
    #[serde(default)]
    pub stderr: String,
    /// The fake's exit code, 0 to 255.
    #[serde(default)]
    pub exit: i32,
}

/// The fakes a recording declares and every call the recorded run made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fakes {
    /// Fake name to its answers, tried in order.
    pub stubs: BTreeMap<String, Vec<FakeAnswer>>,
    /// Each call in order: the name, then its arguments.
    pub calls: Vec<Vec<String>>,
}

impl Fakes {
    /// `self` with `f` applied to every string except the stub names.
    pub(super) fn map(&self, f: impl Fn(&str) -> String) -> Self {
        let answer = |a: &FakeAnswer| FakeAnswer {
            argv: a.argv.as_ref().map(|v| v.iter().map(|s| f(s)).collect()),
            stdout: f(&a.stdout),
            stderr: f(&a.stderr),
            exit: a.exit,
        };
        Self {
            stubs: self
                .stubs
                .iter()
                .map(|(k, v)| (k.clone(), v.iter().map(answer).collect()))
                .collect(),
            calls: self
                .calls
                .iter()
                .map(|c| c.iter().map(|s| f(s)).collect())
                .collect(),
        }
    }

    /// Every string with a `fakes#<n>` label, names and call argv included.
    pub(super) fn fields(&self) -> Vec<(String, &str)> {
        let mut out = Vec::new();
        for (name, answers) in &self.stubs {
            out.push(name.as_str());
            for a in answers {
                out.extend(a.argv.iter().flatten().map(String::as_str));
                out.push(&a.stdout);
                out.push(&a.stderr);
            }
        }
        out.extend(self.calls.iter().flatten().map(String::as_str));
        out.into_iter()
            .enumerate()
            .map(|(i, s)| (format!("fakes#{i}"), s))
            .collect()
    }
}

/// Refuses a fake name that is not a plain file name, a string a shell
/// single-quote cannot hold (NUL) or an exit code outside 0 to 255.
pub(super) fn validate(stubs: &BTreeMap<String, Vec<FakeAnswer>>) -> Result<(), ReplayError> {
    let bad = |m: String| Err(ReplayError::BadArg(m));
    for (name, answers) in stubs {
        let plain = name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'));
        if name.is_empty() || !plain || name.starts_with(['.', '-']) {
            return bad(format!("fake name {name:?} is not a plain file name"));
        }
        for a in answers {
            if !(0..=255).contains(&a.exit) {
                return bad(format!("fake {name}: exit {} is outside 0..=255", a.exit));
            }
            let strings = a.argv.iter().flatten().chain([&a.stdout, &a.stderr]);
            if strings.into_iter().any(|s| s.contains('\0')) {
                return bad(format!("fake {name}: a value holds a NUL byte"));
            }
        }
    }
    Ok(())
}

/// `s` as one `/bin/sh` single-quoted word.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The stub prologue: append `name`, the argument count and each argument,
/// NUL-terminated, to `log`.
fn prologue(name: &str, log: &str) -> String {
    format!(
        "#!/bin/sh\n{{ printf '%s\\000%s\\000' {} \"$#\"; for a in \"$@\"; do printf '%s\\000' \"$a\"; done; }} >> {}\n",
        quote(name),
        quote(log)
    )
}

/// The stub for declared fake `name`; `expand` resolves `{ROOT}`.
fn stub(name: &str, answers: &[FakeAnswer], log: &str, expand: &dyn Fn(&str) -> String) -> String {
    let mut s = prologue(name, log);
    for a in answers {
        let test = match &a.argv {
            None => "true".to_owned(),
            Some(argv) => {
                let mut t = vec![format!("[ \"$#\" -eq {} ]", argv.len())];
                for (i, want) in argv.iter().enumerate() {
                    t.push(format!("[ \"${{{}}}\" = {} ]", i + 1, quote(&expand(want))));
                }
                t.join(" && ")
            }
        };
        s.push_str(&format!(
            "if {test}; then\nprintf '%s' {}\nprintf '%s' {} >&2\nexit {}\nfi\n",
            quote(&expand(&a.stdout)),
            quote(&expand(&a.stderr)),
            a.exit
        ));
    }
    let miss = format!("fake {name}: no answer matches these arguments");
    s.push_str(&format!(
        "printf '%s\\n' {} >&2\nexit {MISS}\n",
        quote(&miss)
    ));
    s
}

/// The trap for undeclared [`GUARDED`] name `name`.
fn trap(name: &str, log: &str) -> String {
    let msg = format!("fake {name}: undeclared executable; declare a fake for it");
    format!(
        "{}printf '%s\\n' {} >&2\nexit {MISS}\n",
        prologue(name, log),
        quote(&msg)
    )
}

/// A directory of stubs and their call log, removed on drop.
pub(super) struct FakeDir {
    dir: Sandbox,
    bin: PathBuf,
    log: PathBuf,
}

impl FakeDir {
    /// Writes the stubs for `stubs` and the traps for every other
    /// [`GUARDED`] name; `expand` resolves `{ROOT}` in answers.
    pub(super) fn new(
        stubs: &BTreeMap<String, Vec<FakeAnswer>>,
        expand: &dyn Fn(&str) -> String,
    ) -> Result<Self, ReplayError> {
        use std::os::unix::fs::PermissionsExt;
        validate(stubs)?;
        let io = |what: &'static str| {
            move |source| ReplayError::Io {
                what: what.into(),
                source,
            }
        };
        let dir = Sandbox::new("fakes")?;
        let bin = dir.root().join("bin");
        let log = dir.root().join("calls.log");
        std::fs::create_dir(&bin).map_err(io("create fake dir"))?;
        let log_s = log.to_string_lossy().into_owned();
        let traps = GUARDED.iter().filter(|g| !stubs.contains_key(**g));
        let scripts = stubs
            .iter()
            .map(|(n, a)| (n.as_str(), stub(n, a, &log_s, expand)))
            .chain(traps.map(|n| (*n, trap(n, &log_s))));
        for (name, script) in scripts {
            let path = bin.join(name);
            std::fs::write(&path, script).map_err(io("write fake"))?;
            let mode = std::fs::Permissions::from_mode(0o755);
            std::fs::set_permissions(&path, mode).map_err(io("chmod fake"))?;
        }
        Ok(Self { dir, bin, log })
    }

    /// The directory the sandbox puts first on `PATH`.
    pub(super) fn bin(&self) -> &Path {
        &self.bin
    }

    /// The directory root, scrubbed as `{FAKES}`.
    pub(super) fn root(&self) -> &Path {
        self.dir.root()
    }

    /// Every call the stubs logged, in order; an empty list when none ran.
    pub(super) fn calls(&self) -> Result<Vec<Vec<String>>, ReplayError> {
        let bytes = match std::fs::read(&self.log) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(ReplayError::Io {
                    what: "read fake call log".into(),
                    source,
                });
            }
        };
        parse_log(&bytes)
    }
}

/// Splits a call log: per call, the name, the argument count, the arguments.
fn parse_log(bytes: &[u8]) -> Result<Vec<Vec<String>>, ReplayError> {
    let damaged = || ReplayError::NotUtf8("the fake call log".into());
    let mut words = bytes.split(|b| *b == 0);
    let mut calls = Vec::new();
    let mut next = || words.next().map(|w| String::from_utf8(w.to_vec()));
    while let Some(name) = next() {
        let name = name.map_err(|_| damaged())?;
        if name.is_empty() {
            break; // the empty slice after the final NUL
        }
        let argc: usize = next()
            .and_then(Result::ok)
            .and_then(|n| n.parse().ok())
            .ok_or_else(damaged)?;
        let mut call = vec![name];
        for _ in 0..argc {
            call.push(next().ok_or_else(damaged)?.map_err(|_| damaged())?);
        }
        calls.push(call);
    }
    Ok(calls)
}

/// `call` for a report line: debug-quoted, withheld when it would leak.
fn shown(call: Option<&Vec<String>>) -> String {
    match call {
        None => "no call".into(),
        Some(c) if c.iter().any(|a| leaks(a)) => "<withheld>".into(),
        Some(c) => format!("{c:?}"),
    }
}

/// `None` when `got` equals `want`, else the first differing call; a call to
/// a name `stubs` does not declare is named as undeclared.
pub(super) fn calls_diff(
    want: &[Vec<String>],
    got: &[Vec<String>],
    stubs: &BTreeMap<String, Vec<FakeAnswer>>,
) -> Option<String> {
    let at = (0..want.len().max(got.len())).find(|&i| want.get(i) != got.get(i))?;
    let head = format!("call {at} ({} vs {} calls)", want.len(), got.len());
    match got.get(at).and_then(|c| c.first()) {
        Some(name) if !stubs.contains_key(name) => {
            Some(format!("{head}: undeclared executable {name:?}"))
        }
        _ => Some(format!(
            "{head}: want {}, got {}",
            shown(want.get(at)),
            shown(got.get(at))
        )),
    }
}
