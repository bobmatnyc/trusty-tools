//! Which running processes are Claude Code, read so the #7771 registry proof
//! can see a running Claude process that wrote no registry entry.
//!
//! Why: Claude Code's `<config dir>/sessions/<pid>.json` registry proves a
//! session ended only when every running Claude Code process has an entry. An
//! older Claude Code, or a background-job worker, runs with none, and the
//! session it runs then read as ended (#7771 critic, HIGH). A name match
//! (`pgrep -x claude`) cannot find them: the native binary is installed as
//! `versions/<version>`, so the kernel may name the process `2.1.284`.
//! What: [`list_processes`] reads each process's pid, name, executable and
//! argv from the process table. [`identify`] classifies one process from those
//! facts alone, so its tests never read the real table.
//! Test: `claude_process_identify_reads_the_measured_shapes`.

use std::path::{Path, PathBuf};

/// One process as the process table lists it (#7771).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ProcessInfo {
    /// The process id.
    pub(crate) pid: u32,
    /// The kernel's name for it — `claude`, or the version string.
    pub(crate) name: String,
    /// Its executable, when the table could read it.
    pub(crate) exe: Option<PathBuf>,
    /// Its argv; empty when the table could not read it.
    pub(crate) cmd: Vec<String>,
}

/// What one process is, for the registry's completeness check (#7771).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Identity {
    /// Claude Code that may run a session, so it must be registered.
    Session,
    /// Claude Code's binary re-executed as a tool it embeds; it runs no session.
    EmbeddedTool,
    /// Not Claude Code.
    Other,
    /// Named like Claude Code, but neither its executable nor its argv could
    /// be read, so what it is cannot be settled.
    Unreadable,
}

/// Tools the native binary runs by re-executing itself under the tool's name
/// (measured on 2026-09-28: `ugrep`, with the `versions/<v>` executable).
const EMBEDDED_TOOLS: &[&str] = &["ugrep", "rg", "bfs"];

/// JavaScript runtimes an npm install of Claude Code runs under.
const RUNTIMES: &[&str] = &["node", "bun"];

/// Classify one process from its table facts (#7771).
///
/// What: `Session` when the executable is Claude Code's (a file named
/// `claude`, or `claude/versions/<v>`), or the script it runs — argv[0], or
/// argv[1] under `node`/`bun` — is a `claude` launcher or the npm
/// `@anthropic-ai/claude-code` package; unless the Claude executable runs
/// under an embedded tool's argv[0]. `Unreadable` for a process with no
/// readable executable or argv whose name is `claude`, a version string, or a
/// JavaScript runtime. `Other` otherwise, which includes the Claude desktop
/// app (`Claude`, `Claude Helper`) and a `grep claude`.
/// Test: `claude_process_identify_reads_the_measured_shapes`.
pub(crate) fn identify(p: &ProcessInfo) -> Identity {
    let binary = p.exe.as_deref().is_some_and(is_claude_binary);
    let argv0 = p.cmd.first().map(|a| basename(a));
    if binary && argv0.is_some_and(|a| EMBEDDED_TOOLS.contains(&a)) {
        return Identity::EmbeddedTool;
    }
    // The script a JavaScript runtime runs is argv[1]; otherwise argv[0].
    let script = match argv0 {
        Some(a) if RUNTIMES.contains(&a) => p.cmd.get(1).map(String::as_str),
        _ => p.cmd.first().map(String::as_str),
    };
    let launcher =
        script.is_some_and(|s| basename(s) == "claude" || s.contains("@anthropic-ai/claude-code/"));
    if binary || launcher {
        return Identity::Session;
    }
    if p.exe.is_none() && p.cmd.is_empty() && claude_shaped(&p.name) {
        return Identity::Unreadable;
    }
    Identity::Other
}

/// A file named `claude`, or one in a `claude/versions` dir.
fn is_claude_binary(exe: &Path) -> bool {
    let named = |p: Option<&Path>, n: &str| p.and_then(Path::file_name).is_some_and(|f| f == n);
    named(Some(exe), "claude")
        || (named(exe.parent(), "versions") && named(exe.parent().and_then(Path::parent), "claude"))
}

/// The last `/`-separated part of an argv word.
fn basename(arg: &str) -> &str {
    arg.rsplit('/').next().unwrap_or(arg)
}

/// A process name Claude Code may run under: `claude`, a version string such
/// as `2.1.284`, or a JavaScript runtime an npm install runs in.
fn claude_shaped(name: &str) -> bool {
    let version = name.split('.').count() >= 3
        && name
            .split('.')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    version || name == "claude" || RUNTIMES.contains(&name)
}

/// Every process in the table, with its executable and argv (#7771).
///
/// What: one full `sysinfo` refresh. An empty table is an error, since this
/// process itself runs.
pub(crate) fn list_processes() -> Result<Vec<ProcessInfo>, String> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_exe(UpdateKind::Always)
            .with_cmd(UpdateKind::Always),
    );
    if sys.processes().is_empty() {
        return Err("the process table listed no process".into());
    }
    Ok(sys
        .processes()
        .iter()
        .map(|(pid, p)| ProcessInfo {
            pid: pid.as_u32(),
            name: p.name().to_string_lossy().into_owned(),
            exe: p.exe().map(Path::to_path_buf),
            cmd: p
                .cmd()
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
        })
        .collect())
}
