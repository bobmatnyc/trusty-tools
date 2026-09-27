//! Read one kuzu-memory store through kuzu-memory's own Python (#277).
//!
//! Why: the owner ruled out a native `kuzu` crate — the store is read by
//! shelling out to the interpreter kuzu-memory is installed under, which
//! already carries the `kuzu` module. kuzu-memory's `memory export` command is
//! NOT used: it opens the database read-write and, on an older store, runs
//! `ALTER TABLE` migrations and writes a backup before exporting. `export.py`
//! opens the database with `read_only=True` instead and emits Memory rows in
//! the same shape as kuzu-memory's `export_memories_to_json`, plus the Entity
//! nodes and the MENTIONS / RELATES_TO edges that export does not cover.
//! What: [`export_store`] writes `export.py` into a fresh temp directory, runs
//! it through a [`CommandRunner`], parses the JSON it wrote, and drops the temp
//! directory on every path, so nothing is left behind even on a dry run.
//! [`resolve_python`] finds the interpreter from `--python` or from the
//! shebang of `kuzu-memory` on `PATH`.
//! Test: `bridge_parses_real_export_shape`, `bridge_leaves_no_temp_output`,
//! `bridge_failure_arms_are_typed_errors`, `resolve_python_reads_the_shebang`.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::KuzuImportError;

/// The export script, run as `python export.py <memories.db> <out.json>`.
pub const EXPORT_SCRIPT: &str = include_str!("export.py");

/// The `format` value `export.py` stamps on its output.
pub const EXPORT_FORMAT: &str = "trusty-kuzu-export/1";

/// Columns a Memory row must carry for the import to mean anything.
pub const REQUIRED_MEMORY_COLUMNS: &[&str] = &["id", "content"];

/// What a finished child process reported.
#[derive(Debug, Clone)]
pub struct RunOutput {
    pub success: bool,
    pub code: Option<i32>,
    pub stderr: String,
}

/// The seam between the bridge and a real process.
///
/// Why: every failure arm (no interpreter, non-zero exit, malformed output)
/// must be testable without Python or a Kuzu database.
/// What: run `program` with `args` and report the exit and stderr.
/// Test: the fake runners in `kuzu_import::tests`.
pub trait CommandRunner {
    fn run(&self, program: &Path, args: &[OsString]) -> std::io::Result<RunOutput>;
}

/// [`CommandRunner`] over `std::process::Command`, stdin and stdout closed,
/// killed once it outlives `timeout`.
///
/// Why (#277 L1): a wedged interpreter (a kuzu lock wait, a hung import) must
/// not block the run forever. The bound is generous by default because a
/// large store exports for a while.
/// What: spawns the child, drains stderr on a thread so a full pipe cannot
/// stall it, and polls `try_wait` until the deadline; past it the child is
/// killed and reaped and the run fails with `ErrorKind::TimedOut`.
/// Test: `system_runner_kills_a_child_past_its_timeout`.
#[derive(Debug, Clone, Copy)]
pub struct SystemRunner {
    pub timeout: Duration,
}

impl SystemRunner {
    /// A runner that kills its child after `timeout`.
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl Default for SystemRunner {
    fn default() -> Self {
        Self::new(Duration::from_secs(super::DEFAULT_BRIDGE_TIMEOUT_SECS))
    }
}

/// How often a running child is polled for exit.
const POLL: Duration = Duration::from_millis(50);

impl CommandRunner for SystemRunner {
    fn run(&self, program: &Path, args: &[OsString]) -> std::io::Result<RunOutput> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut pipe = child.stderr.take();
        let reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(p) = pipe.as_mut() {
                let _ = p.read_to_end(&mut buf);
            }
            String::from_utf8_lossy(&buf).into_owned()
        });
        let deadline = Instant::now() + self.timeout;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("killed after {}s", self.timeout.as_secs()),
                ));
            }
            std::thread::sleep(POLL);
        };
        Ok(RunOutput {
            success: status.success(),
            code: status.code(),
            stderr: reader.join().unwrap_or_default(),
        })
    }
}

/// One Memory row, in the shape of kuzu-memory's `export_memories_to_json`.
///
/// Every field is optional because columns vary by kuzu-memory version; the
/// required ones are checked by [`validate_export`]. `embedding` is never
/// exported — trusty-memory embeds on write.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct KuzuMemoryRow {
    pub id: Option<String>,
    pub content: Option<String>,
    pub content_hash: Option<String>,
    pub created_at: Option<String>,
    pub memory_type: Option<String>,
    pub knowledge_type: Option<String>,
    pub importance: Option<f64>,
    pub source_type: Option<String>,
    pub project_tag: Option<String>,
    pub agent_id: Option<String>,
    pub user_id: Option<String>,
    pub session_id: Option<String>,
    pub metadata: Option<String>,
}

/// One Entity node.
#[derive(Debug, Clone, Deserialize)]
pub struct KuzuEntityRow {
    pub id: Option<String>,
    pub name: Option<String>,
    pub entity_type: Option<String>,
}

/// One MENTIONS edge (Memory -> Entity).
#[derive(Debug, Clone, Deserialize)]
pub struct KuzuMention {
    pub memory_id: Option<String>,
    pub entity_id: Option<String>,
    pub confidence: Option<f64>,
}

/// One RELATES_TO edge (Memory -> Memory).
#[derive(Debug, Clone, Deserialize)]
pub struct KuzuRelatesTo {
    pub from_id: Option<String>,
    pub to_id: Option<String>,
    pub relationship_type: Option<String>,
    pub strength: Option<f64>,
}

/// The whole document `export.py` writes.
#[derive(Debug, Clone, Deserialize)]
pub struct KuzuExport {
    pub format: String,
    #[serde(default)]
    pub memory_columns: Vec<String>,
    pub memories: Vec<KuzuMemoryRow>,
    #[serde(default)]
    pub entities: Vec<KuzuEntityRow>,
    #[serde(default)]
    pub mentions: Vec<KuzuMention>,
    #[serde(default)]
    pub relates_to: Vec<KuzuRelatesTo>,
    /// Row count per relationship table the import does not map (#277 LOW-2).
    #[serde(default)]
    pub other_edges: BTreeMap<String, usize>,
    /// The exception class when `export.py` could not count those tables.
    #[serde(default)]
    pub other_edges_error: Option<String>,
}

impl KuzuExport {
    /// Number of KG edges the export carries.
    pub fn edge_count(&self) -> usize {
        self.mentions.len() + self.relates_to.len()
    }
}

/// Export one store to a parsed [`KuzuExport`].
///
/// Why: the single read path; see the module doc for why it is not
/// `kuzu-memory memory export`.
/// What: temp dir -> write script -> run -> read `export.json` -> parse ->
/// [`validate_export`]. The temp dir is owned by this frame and removed when
/// it returns, success or failure.
/// Test: `bridge_parses_real_export_shape`, `bridge_leaves_no_temp_output`,
/// `bridge_failure_arms_are_typed_errors`.
pub fn export_store(
    runner: &dyn CommandRunner,
    python: &Path,
    db: &Path,
) -> Result<KuzuExport, KuzuImportError> {
    let tmp = tempfile::TempDir::with_prefix("trusty-kuzu-import-")
        .map_err(|e| KuzuImportError::Io(format!("create temp dir: {e}")))?;
    let script = tmp.path().join("export.py");
    let out = tmp.path().join("export.json");
    std::fs::write(&script, EXPORT_SCRIPT)
        .map_err(|e| KuzuImportError::Io(format!("write export script: {e}")))?;
    let args = [
        script.into_os_string(),
        db.as_os_str().to_os_string(),
        out.clone().into_os_string(),
    ];
    let run = runner.run(python, &args).map_err(|e| match e.kind() {
        // #277 L1: the runner killed a child that outlived its bound.
        std::io::ErrorKind::TimedOut => KuzuImportError::BridgeTimedOut(e.to_string()),
        _ => {
            KuzuImportError::InterpreterNotFound(format!("could not run {}: {e}", python.display()))
        }
    })?;
    if !run.success {
        return Err(KuzuImportError::BridgeFailed {
            code: run.code,
            stderr: tail(&run.stderr, 400),
        });
    }
    let raw = std::fs::read(&out).map_err(|e| {
        KuzuImportError::MalformedExport(format!("bridge exited 0 but wrote no output: {e}"))
    })?;
    let export: KuzuExport = serde_json::from_slice(&raw)
        .map_err(|e| KuzuImportError::MalformedExport(e.to_string()))?;
    validate_export(&export)?;
    Ok(export)
}

/// Reject an export the importer cannot map without guessing.
///
/// What: the format stamp must match [`EXPORT_FORMAT`], and every column in
/// [`REQUIRED_MEMORY_COLUMNS`] must be present — both in `memory_columns` and
/// on each row (a row with no `id` has no identity to be idempotent on).
/// Test: `bridge_failure_arms_are_typed_errors`.
pub fn validate_export(export: &KuzuExport) -> Result<(), KuzuImportError> {
    if export.format != EXPORT_FORMAT {
        return Err(KuzuImportError::MalformedExport(format!(
            "unknown export format {:?}",
            export.format
        )));
    }
    let missing: Vec<String> = REQUIRED_MEMORY_COLUMNS
        .iter()
        .filter(|c| !export.memory_columns.iter().any(|have| have == *c))
        .map(|c| (*c).to_string())
        .collect();
    if !missing.is_empty() {
        return Err(KuzuImportError::SchemaColumnsMissing(missing));
    }
    if export
        .memories
        .iter()
        .any(|m| m.id.as_deref().is_none_or(str::is_empty))
    {
        return Err(KuzuImportError::MalformedExport(
            "a Memory row has no id".to_string(),
        ));
    }
    Ok(())
}

/// Find the interpreter kuzu-memory runs under.
///
/// Why: the `kuzu` module lives in kuzu-memory's own environment (a pipx or
/// uv venv, usually), not in whatever `python3` is first on `PATH`.
/// What: `explicit` wins when given and must exist. Otherwise `kuzu-memory` is
/// looked up on `path_var` and its shebang names the interpreter; an
/// `#!/usr/bin/env python3` shebang is resolved on `path_var` too. Only an
/// interpreter whose file name starts with `python` is accepted.
/// Test: `resolve_python_reads_the_shebang`, `bridge_failure_arms_are_typed_errors`,
/// `non_python_shebang_is_refused_with_the_python_hint`.
pub fn resolve_python(
    explicit: Option<&Path>,
    path_var: Option<&OsStr>,
) -> Result<PathBuf, KuzuImportError> {
    if let Some(p) = explicit {
        if p.exists() {
            return Ok(p.to_path_buf());
        }
        return Err(KuzuImportError::InterpreterNotFound(format!(
            "--python {} does not exist",
            p.display()
        )));
    }
    let launcher = which("kuzu-memory", path_var).ok_or_else(|| {
        KuzuImportError::InterpreterNotFound(
            "kuzu-memory is not on PATH; install it or pass --python <interpreter \
             that can `import kuzu`>"
                .to_string(),
        )
    })?;
    let text = std::fs::read_to_string(&launcher).map_err(|e| {
        KuzuImportError::InterpreterNotFound(format!("read {}: {e}", launcher.display()))
    })?;
    let shebang = text
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("#!"))
        .map(str::trim)
        .ok_or_else(|| {
            KuzuImportError::InterpreterNotFound(format!(
                "{} has no shebang; pass --python",
                launcher.display()
            ))
        })?;
    let mut parts = shebang.split_whitespace();
    let first = parts.next().unwrap_or_default();
    let interpreter = if first.ends_with("/env") {
        let name = parts.find(|p| !p.starts_with('-')).unwrap_or("python3");
        which(name, path_var).ok_or_else(|| {
            KuzuImportError::InterpreterNotFound(format!("{name} (from kuzu-memory) not on PATH"))
        })?
    } else {
        PathBuf::from(first)
    };
    // #277 L2: a pip `#!/bin/sh` trampoline, or a shebang path with a space
    // that whitespace-splitting cut short, names no python; say so.
    let is_python = interpreter
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|n| n.starts_with("python"));
    if !is_python {
        return Err(KuzuImportError::InterpreterNotFound(format!(
            "{}'s shebang names {}, which is not a python interpreter; pass --python \
             <the interpreter kuzu-memory runs under>",
            launcher.display(),
            interpreter.display()
        )));
    }
    if interpreter.exists() {
        Ok(interpreter)
    } else {
        Err(KuzuImportError::InterpreterNotFound(format!(
            "kuzu-memory's interpreter {} does not exist",
            interpreter.display()
        )))
    }
}

fn which(name: &str, path_var: Option<&OsStr>) -> Option<PathBuf> {
    std::env::split_paths(path_var?)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// The last `max` characters of `s`, so an error line stays one line long.
fn tail(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    let count = trimmed.chars().count();
    trimmed.chars().skip(count.saturating_sub(max)).collect()
}
