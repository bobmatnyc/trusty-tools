//! Recording one script run on a sandbox copy of its inputs.
//!
//! Why: #12026 AC1. A slice gate needs the script's whole observable
//! contract, captured without touching live state and without letting the
//! parent's environment (tokens, paths) into the recording.
//! What: [`record`] seeds a [`Sandbox`], runs the program under a cleared
//! environment holding only the allowlist, and builds a [`Recording`] that is
//! scrubbed and [`check`]ed before it is returned. A `.py` program runs under
//! [`PROBE`], a Python audit hook that logs every `open` and directory
//! listing (an `os.open` write flag counts as a write, not a read), so
//! `reads` names the input files the script used and an open outside the
//! sandbox (live state) is refused. A missing probe log is an error.
//! Test: `record_captures_argv_stdin_env_files_and_writes`,
//! `record_keeps_env_outside_the_allowlist_out`,
//! `record_refuses_a_read_outside_the_sandbox`,
//! `record_counts_an_os_open_write_as_a_write_not_a_read`,
//! `record_refuses_a_run_whose_probe_log_is_missing`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::sandbox::{Sandbox, Snapshot, changes, rel_key};
use super::{ROOT_TOKEN, Recording, ReplayError, Scrubber, VERSION, check};

/// The most seed bytes one recording copies; a fixture is text, not a tree.
const SEED_LIMIT: u64 = 16 * 1024 * 1024;

/// The open probe. It drops its own variable before the script runs, so the
/// script's environment is the allowlist alone.
const PROBE: &str = r#"import os, runpy, sys
_log = open(os.environ.pop("TRUSTY_REPLAY_PROBE"), "a", encoding="utf-8")
_log.write("probe\tstart\n")
_W = os.O_WRONLY | os.O_RDWR | os.O_APPEND | os.O_CREAT | os.O_TRUNC
for _p in sys.path[1:]:
    _log.write("path\t" + os.path.abspath(_p) + "\n")
def _hook(ev, args):
    if ev == "open" and isinstance(args[0], (str, bytes)):
        if isinstance(args[1], str):
            mode = args[1]
        else:
            mode = "w" if isinstance(args[2], int) and args[2] & _W else "r"
        _log.write(mode + "\t" + os.path.abspath(os.fsdecode(args[0])) + "\n")
    elif ev in ("os.listdir", "os.scandir") and isinstance(args[0], (str, bytes, type(None))):
        _log.write("list\t" + os.path.abspath(os.fsdecode(args[0] or ".")) + "\n")
    else:
        return
    _log.flush()
sys.addaudithook(_hook)
_script = os.path.abspath(sys.argv[1])
sys.argv = sys.argv[1:]
sys.path[0] = os.path.dirname(_script)
runpy.run_path(_script, run_name="__main__")
"#;

/// One allowlist entry: a fixed value (may hold `{ROOT}`) or the parent's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvRule {
    /// `NAME=VALUE`.
    Set(String, String),
    /// `NAME`: the parent's value, when it has one.
    Pass(String),
}

/// What to record.
#[derive(Debug, Clone)]
pub struct RecordSpec {
    /// The script or binary.
    pub program: PathBuf,
    /// Arguments after the program; `{ROOT}` is the sandbox root.
    pub argv: Vec<String>,
    /// Bytes for stdin.
    pub stdin: String,
    /// The environment allowlist.
    pub env: Vec<EnvRule>,
    /// Live files or directories copied in first: `(source, relative dest)`.
    pub seeds: Vec<(PathBuf, String)>,
    /// Fleet session names to scrub and refuse.
    pub sessions: Vec<String>,
    /// The run's deadline.
    pub timeout: Duration,
}

/// The Python probe for a `.py` program: the interpreter, its arguments and
/// the directory holding the probe and its log (removed on drop).
pub(super) struct Python {
    pub(super) interpreter: PathBuf,
    pub(super) prefix: Vec<String>,
    pub(super) dir: Sandbox,
    pub(super) log: PathBuf,
}

impl Python {
    /// `Some` when `program` is a `.py` script.
    pub(super) fn for_program(
        program: &Path,
        parent_env: &BTreeMap<String, String>,
    ) -> Result<Option<Self>, ReplayError> {
        if program.extension().and_then(|e| e.to_str()) != Some("py") {
            return Ok(None);
        }
        let interpreter = parent_env
            .get("PATH")
            .into_iter()
            .flat_map(|p| p.split(':'))
            .map(|d| Path::new(d).join("python3"))
            .find(|p| p.is_file())
            .ok_or(ReplayError::NoPython)?;
        let dir = Sandbox::new("probe")?;
        let probe = dir.root().join("probe.py");
        std::fs::write(&probe, PROBE).map_err(|source| ReplayError::Io {
            what: "write probe".into(),
            source,
        })?;
        let script = std::path::absolute(program).map_err(|source| ReplayError::Io {
            what: "resolve script".into(),
            source,
        })?;
        let prefix = vec![
            "-B".into(),
            probe.to_string_lossy().into_owned(),
            script.to_string_lossy().into_owned(),
        ];
        let log = dir.root().join("probe.log");
        Ok(Some(Self {
            interpreter,
            prefix,
            dir,
            log,
        }))
    }
}

/// The scrubber for a run in `sb` of `program`, with the probe dir if any.
pub(super) fn run_scrubber(
    sb: &Sandbox,
    program: &Path,
    python: Option<&Python>,
    parent_env: &BTreeMap<String, String>,
) -> Scrubber {
    let mut s = Scrubber::new().path(sb.root(), ROOT_TOKEN);
    // A script's own directory shows only in its tracebacks; a binary's
    // directory (`/bin`) would also match `PATH` text, so it is left alone.
    if let Some(py) = python {
        s = s.path(py.dir.root(), "{PROBE}");
        if let Some(dir) = std::path::absolute(program)
            .ok()
            .as_deref()
            .and_then(Path::parent)
        {
            s = s.path(dir, "{PROG}");
        }
    }
    home_rules(s, parent_env)
}

/// `s` plus the parent's `HOME`, and `/Users/<name>` and `/home/<name>` for
/// its last component (the same owner on a macOS or a Linux host), as `{HOME}`.
fn home_rules(mut s: Scrubber, parent_env: &BTreeMap<String, String>) -> Scrubber {
    let Some(home) = parent_env.get("HOME").map(Path::new) else {
        return s;
    };
    if let Some(name) = home.file_name().and_then(|n| n.to_str()) {
        for base in ["/Users", "/home"] {
            s = s.path(&Path::new(base).join(name), "{HOME}");
        }
    }
    s.path(home, "{HOME}")
}

fn text(bytes: Vec<u8>, field: &str) -> Result<String, ReplayError> {
    String::from_utf8(bytes).map_err(|_| ReplayError::NotUtf8(field.to_owned()))
}

/// Records `spec`, reading pass-through values from `parent_env`.
///
/// Why: see the module doc; `parent_env` is a parameter so a test can prove
/// a variable outside the allowlist never arrives.
/// What: seed, run, diff the sandbox, scrub, [`check`]. Returns the
/// scrubbed recording; nothing is written.
/// Test: `record_captures_argv_stdin_env_files_and_writes`,
/// `record_keeps_env_outside_the_allowlist_out`.
pub fn record(
    spec: &RecordSpec,
    parent_env: &BTreeMap<String, String>,
) -> Result<Recording, ReplayError> {
    // Inputs are scrubbed before the run, so the program sees exactly what is
    // recorded and a replay of the recorded inputs is faithful by construction.
    let input = home_rules(Scrubber::new(), parent_env).sessions(&spec.sessions);
    let sb = Sandbox::new("rec")?;
    let mut budget = SEED_LIMIT;
    for (src, rel) in &spec.seeds {
        sb.seed(src, rel, &mut budget)?;
    }
    for (rel, bytes) in sb.snapshot()? {
        sb.put(
            &rel,
            &input.text(&text(bytes, &format!("input file {rel}"))?),
        )?;
    }
    let before = sb.snapshot()?;
    let mut env = BTreeMap::new();
    for rule in &spec.env {
        let (k, v) = match rule {
            EnvRule::Set(k, v) => (k, Some(v)),
            EnvRule::Pass(k) => (k, parent_env.get(k)),
        };
        if let Some(v) = v {
            env.insert(k.clone(), input.text(v));
        }
    }
    let argv: Vec<String> = spec.argv.iter().map(|a| input.text(a)).collect();
    let stdin = input.text(&spec.stdin);
    let python = Python::for_program(&spec.program, parent_env)?;
    let mut run_env = env.clone();
    let (program, prefix) = match &python {
        Some(py) => {
            let log = py.log.to_string_lossy().into_owned();
            run_env.insert("TRUSTY_REPLAY_PROBE".into(), log);
            (py.interpreter.clone(), py.prefix.clone())
        }
        None => (spec.program.clone(), Vec::new()),
    };
    let fin = sb.run(&program, &prefix, &argv, &run_env, &stdin, spec.timeout)?;
    let exit = fin.status.code().ok_or(ReplayError::Signal)?;
    let after = sb.snapshot()?;
    let reads = match &python {
        Some(py) => Some(probe_reads(
            py,
            &spec.program,
            sb.root(),
            &before,
            parent_env,
        )?),
        None => None,
    };
    let mut files = BTreeMap::new();
    for (k, v) in before.clone() {
        files.insert(k.clone(), text(v, &format!("input file {k}"))?);
    }
    let mut writes = BTreeMap::new();
    for (k, v) in changes(&before, &after) {
        let content = v
            .map(|b| text(b, &format!("written file {k}")))
            .transpose()?;
        writes.insert(k, content);
    }
    let raw = Recording {
        version: VERSION,
        argv,
        stdin,
        env,
        files,
        reads,
        stdout: text(fin.stdout, "stdout")?,
        stderr: text(fin.stderr, "stderr")?,
        exit,
        writes,
    };
    let scrubber =
        run_scrubber(&sb, &spec.program, python.as_ref(), parent_env).sessions(&spec.sessions);
    let rec = scrubber.recording(raw);
    check(&rec, &spec.sessions)?;
    Ok(rec)
}

/// The sandbox input files the probe saw opened or listed; an error when the
/// script reached a file outside the sandbox that is not its own code.
fn probe_reads(
    py: &Python,
    program: &Path,
    root: &Path,
    before: &Snapshot,
    parent_env: &BTreeMap<String, String>,
) -> Result<Vec<String>, ReplayError> {
    // #12026: no log, or one without the probe's first line, means the reads
    // are unknown; an empty list would pass an outside read silently.
    let log = std::fs::read_to_string(&py.log)
        .ok()
        .filter(|l| l.starts_with("probe\tstart\n"))
        .ok_or(ReplayError::ProbeLog)?;
    let script_dir = std::path::absolute(program)
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let lib: Vec<PathBuf> = log
        .lines()
        .filter_map(|l| l.strip_prefix("path\t"))
        .map(PathBuf::from)
        .collect();
    let mut reads = BTreeSet::new();
    let mut external = Vec::new();
    for (kind, raw) in log.lines().filter_map(|l| l.split_once('\t')) {
        if kind == "path" || kind == "probe" {
            continue;
        }
        let p = std::fs::canonicalize(raw).unwrap_or_else(|_| PathBuf::from(raw));
        let write = kind.contains(['w', 'a', 'x', '+']);
        if p.starts_with(root) {
            let rel = rel_key(root, &p);
            if kind == "list" {
                let inside = |k: &&String| match k.rsplit_once('/') {
                    Some((dir, _)) => dir == rel,
                    None => rel.is_empty(),
                };
                reads.extend(before.keys().filter(inside).cloned());
            } else if !write && before.contains_key(&rel) {
                reads.insert(rel);
            }
            continue;
        }
        let own_code = script_dir.as_deref().is_some_and(|d| p.starts_with(d))
            && (kind == "list"
                || p.components().any(|c| c.as_os_str() == "__pycache__")
                || p.extension()
                    .is_some_and(|e| e == "py" || e == "pyc" || e == "so"));
        let library = !write && lib.iter().any(|l| p.starts_with(l));
        if !(own_code || library || p.starts_with("/dev")) {
            external.push(p);
        }
    }
    if let Some(first) = external.first() {
        let home = parent_env.get("HOME").map(PathBuf::from);
        let shown = match home.as_deref().and_then(|h| first.strip_prefix(h).ok()) {
            Some(rest) => format!("~/{}", rest.display()),
            None => first.display().to_string(),
        };
        return Err(ReplayError::External {
            count: external.len(),
            first: shown,
        });
    }
    Ok(reads.into_iter().collect())
}
