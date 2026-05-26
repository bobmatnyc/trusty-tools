//! Auto-discovery of Claude Code sessions, in tmux and in native processes.
//!
//! Why: `GET /sessions` only reports daemon-managed sessions, but operators run
//! `claude`, `claude-code`, `claude-mpm`, or `tm` in tmux panes the daemon never
//! created — and, more commonly, in native Terminal.app windows that have no
//! tmux at all. Those sessions were invisible until manually `/adopt`-ed.
//! Scanning tmux *and* the process table at startup (and on demand) brings them
//! under oversight automatically.
//! What: [`discover_claude_sessions`] runs `tmux list-panes -a`;
//! [`discover_native_processes`] runs `ps aux` and matches `claude`/`claude-code`
//! processes, resolving each one's working directory via `lsof`.
//! [`discover_all`] runs both. [`is_claude_command`] / [`is_claude_process`] are
//! the pure predicates the scans key on.
//! Test: `cargo test -p trusty-mpm-daemon discovery` covers the predicates and
//! the line parsers without spawning tmux, ps, or lsof.

use std::collections::HashSet;
use std::process::Command;

use crate::core::session::{ControlModel, Session, SessionHost, SessionId, SessionStatus};

use super::state::DaemonState;
use super::tmux::TmuxDriver;

/// Process names that mark a tmux pane as running Claude Code.
///
/// Why: auto-discovery must recognise the handful of binaries an operator runs
/// a Claude Code session under; keeping the list in one place makes the
/// predicate auditable.
/// What: substrings matched case-insensitively against `pane_current_command`.
const CLAUDE_COMMANDS: &[&str] = &["claude", "claude-code", "claude-mpm", "tm"];

/// True when `command` names a Claude Code process worth adopting.
///
/// Why: the discovery scan must decide, per pane, whether it hosts Claude Code;
/// a pure predicate keeps that decision unit-testable.
/// What: case-insensitively matches `command` against [`CLAUDE_COMMANDS`] —
/// `claude`/`claude-code`/`claude-mpm` match as substrings, while `tm` must be
/// the whole command so it never matches unrelated binaries like `vim`.
/// Test: `is_claude_command_matches_known`, `is_claude_command_rejects_others`.
pub fn is_claude_command(command: &str) -> bool {
    let lower = command.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }
    // `tm` is short enough to appear inside unrelated names — require an exact
    // match for it, but allow substring matches for the longer, distinctive
    // `claude*` names.
    lower == "tm"
        || CLAUDE_COMMANDS
            .iter()
            .any(|c| *c != "tm" && lower.contains(c))
}

/// Parse one `tmux list-panes -a` line into `(session_name, pane_command)`.
///
/// Why: the scan formats panes as `#{session_name} #{pane_current_command}`;
/// isolating the split keeps [`discover_claude_sessions`] readable and lets the
/// parser be tested without tmux.
/// What: splits on the first whitespace run; returns `None` for an empty or
/// single-field line.
/// Test: `parse_pane_line_splits_fields`.
fn parse_pane_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    let (session, command) = trimmed.split_once(char::is_whitespace)?;
    let command = command.trim();
    if session.is_empty() || command.is_empty() {
        return None;
    }
    Some((session.to_string(), command.to_string()))
}

/// Outcome of one auto-discovery scan.
///
/// Why: the `POST /sessions/discover` handler and the Telegram/TUI `/discover`
/// commands report how many sessions the scan adopted; bundling the count with
/// the names lets callers log the specifics.
/// What: the number of newly-registered sessions and their tmux names.
/// Test: covered indirectly by `discover_claude_sessions` against a daemon with
/// no tmux (yields an empty result).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DiscoveryResult {
    /// Number of tmux sessions newly registered by the scan.
    pub adopted: usize,
    /// Friendly tmux names of the newly-registered sessions.
    pub sessions: Vec<String>,
}

/// Scan existing tmux sessions and register any running Claude Code.
///
/// Why: sessions the daemon did not create are invisible to `GET /sessions`
/// until adopted; running this at startup (and on demand via the API) keeps the
/// registry honest without operator intervention.
/// What: runs `tmux list-panes -a -F "#{session_name} #{pane_current_command}"`,
/// and for every pane whose command satisfies [`is_claude_command`], registers
/// a [`Session`] for the owning tmux session — unless one is already registered
/// under that `tmux_name`. tmux being absent yields an empty [`DiscoveryResult`]
/// rather than an error.
/// Test: `is_claude_command_*` cover the predicate; the tmux-absent path is
/// exercised by `discover_with_no_tmux_is_empty`.
pub fn discover_claude_sessions(state: &DaemonState) -> DiscoveryResult {
    let driver = match TmuxDriver::discover() {
        Ok(driver) => driver,
        Err(_) => {
            tracing::info!("tmux unavailable; session auto-discovery skipped");
            return DiscoveryResult::default();
        }
    };

    let raw = match driver.list_claude_panes() {
        Ok(raw) => raw,
        Err(e) => {
            tracing::warn!("tmux pane listing failed during discovery: {e}");
            return DiscoveryResult::default();
        }
    };

    // Already-registered tmux names — never register the same session twice.
    let registered: HashSet<String> = state
        .list_sessions()
        .into_iter()
        .map(|s| s.tmux_name)
        .collect();

    let mut result = DiscoveryResult::default();
    let mut seen: HashSet<String> = HashSet::new();
    for line in raw.lines() {
        let Some((session_name, command)) = parse_pane_line(line) else {
            continue;
        };
        if !is_claude_command(&command) {
            continue;
        }
        if registered.contains(&session_name) || !seen.insert(session_name.clone()) {
            continue;
        }
        // Register a tmux-hosted session under the discovered tmux name. The
        // workdir is unknown from the pane listing, so it is left empty — a
        // later snapshot or hook event can enrich it.
        let mut session = Session::new(SessionId::new(), String::new(), ControlModel::Tmux, None);
        session.tmux_name = session_name.clone();
        session.status = SessionStatus::Active;
        session.origin = SessionHost::Tmux;
        state.register_session(session);
        tracing::info!("auto-discovered Claude Code tmux session: {session_name}");
        result.adopted += 1;
        result.sessions.push(session_name);
    }
    result
}

/// True when a `ps aux` command line is a Claude Code CLI session worth adopting.
///
/// Why: native discovery must distinguish the actual `claude` CLI process from
/// the dozens of unrelated processes whose command line merely mentions
/// "claude" — MCP servers (`python3 -m claude_mpm...`), the desktop app
/// (`/Applications/Claude.app/...`), trusty-mpm itself, or a piped `grep claude`.
/// A pure predicate keeps that decision unit-testable without spawning `ps`.
/// What: matches solely on the *executable basename* — the last path component
/// of the first whitespace-separated token — which must be exactly `claude` or
/// `claude-code`. This is both precise and robust: `/usr/local/bin/claude
/// --system-prompt-file …/.claude-mpm/PM.md` matches (the `.claude-mpm` *path*
/// no longer falsely disqualifies a real session), while `node /opt/claude/cli.js`,
/// `python3 -m claude_mpm…`, `grep claude`, and `uv tool … claude-mpm` do not —
/// their executables are `node`/`python3`/`grep`/`uv`, not `claude`.
/// Test: `is_claude_process_matches`, `is_claude_process_rejects_noise`.
pub fn is_claude_process(cmdline: &str) -> bool {
    let lower = cmdline.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }
    let exe = lower.split_whitespace().next().unwrap_or_default();
    let basename = exe.rsplit('/').next().unwrap_or(exe);
    basename == "claude" || basename == "claude-code"
}

/// Parse one `ps aux` line into `(pid, command_line)`.
///
/// Why: `ps aux` columns are `USER PID %CPU %MEM VSZ RSS TT STAT STARTED TIME
/// COMMAND`; isolating the split keeps [`discover_native_processes`] readable
/// and lets the parser be tested without `ps`.
/// What: takes field 2 as the pid and everything from field 11 onward as the
/// command line; returns `None` for the header row or a malformed line.
/// Test: `parse_ps_line_extracts_pid_and_command`.
fn parse_ps_line(line: &str) -> Option<(u32, String)> {
    let mut fields = line.split_whitespace();
    let _user = fields.next()?;
    let pid: u32 = fields.next()?.parse().ok()?;
    // Skip %CPU %MEM VSZ RSS TT STAT STARTED TIME (8 columns) to reach COMMAND.
    for _ in 0..8 {
        fields.next()?;
    }
    let command: String = fields.collect::<Vec<_>>().join(" ");
    if command.is_empty() {
        return None;
    }
    Some((pid, command))
}

/// Parse `lsof -Ffpn` field output into a `pid -> cwd` map.
///
/// Why: `lsof -F` emits one field per line — `p<pid>` starts a process block,
/// `f<fd>` names a file descriptor, and `n<path>` carries that descriptor's
/// path. Even with `-d cwd`, `lsof` still reports the `txt` (executable)
/// descriptor, so the parser must record only the `n` line that follows an
/// `fcwd` marker — otherwise it would mistake the executable path for the cwd.
/// A pure parser keeps this unit-testable without spawning `lsof`.
/// What: walks the lines, tracking the current `p<pid>` and whether the most
/// recent `f` field was `cwd`; records `n<path>` only while inside an `fcwd`
/// block.
/// Test: `parse_lsof_cwds_maps_pid_to_path`.
fn parse_lsof_cwds(text: &str) -> std::collections::HashMap<u32, String> {
    let mut map = std::collections::HashMap::new();
    let mut current: Option<u32> = None;
    let mut in_cwd_fd = false;
    for line in text.lines() {
        if let Some(pid) = line.strip_prefix('p').and_then(|p| p.parse::<u32>().ok()) {
            current = Some(pid);
            in_cwd_fd = false;
        } else if let Some(fd) = line.strip_prefix('f') {
            in_cwd_fd = fd == "cwd";
        } else if let (Some(path), Some(pid), true) = (
            line.strip_prefix('n').filter(|p| !p.is_empty()),
            current,
            in_cwd_fd,
        ) {
            map.entry(pid).or_insert_with(|| path.to_string());
        }
    }
    map
}

/// How many pids to pass to one `lsof -p` invocation.
///
/// Why: macOS `lsof` mis-handles a very long `-p` comma list — past a few dozen
/// pids it stops honouring later entries and reports a wrong (often the first
/// process's) working directory for the rest. Chunking keeps every `-p` list
/// short enough for `lsof` to parse correctly, while still being far cheaper
/// than one `lsof` per pid.
const LSOF_PID_CHUNK: usize = 32;

/// Resolve a single chunk of pids' working directories via one `lsof` call.
///
/// Why: factored out of [`process_cwds`] so the chunking loop stays readable.
/// What: runs `lsof -a -p <pid1>,...,<pidN> -d cwd -Ffpn` for the chunk and
/// returns a `pid -> cwd` map. The `-a` flag ANDs the `-p` and `-d` selections
/// (`lsof` ORs them by default); the `f` field lets [`parse_lsof_cwds`] tell the
/// `cwd` descriptor from the `txt` one. `lsof` being unavailable yields an empty
/// map.
fn process_cwds_chunk(pids: &[u32]) -> std::collections::HashMap<u32, String> {
    let pid_list = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    match Command::new("lsof")
        .args(["-a", "-p", &pid_list, "-d", "cwd", "-Ffpn"])
        .output()
    {
        // `lsof` exits non-zero when some pids have vanished, but still prints
        // the rows it could read — so parse stdout regardless of exit status.
        Ok(out) => parse_lsof_cwds(&String::from_utf8_lossy(&out.stdout)),
        Err(_) => {
            tracing::info!("`lsof` unavailable; native session workdirs unknown");
            std::collections::HashMap::new()
        }
    }
}

/// Resolve many processes' working directories with chunked `lsof` calls.
///
/// Why: a native Claude Code process's workdir is the project it operates on,
/// but calling `lsof` once per pid is pathologically slow (Claude Code spawns
/// hundreds of `claude` subprocesses) and one `lsof` over a huge `-p` list is
/// unreliable on macOS. Chunking strikes the balance: a handful of `lsof` calls,
/// each with a short, correctly-parsed pid list.
/// What: splits `pids` into [`LSOF_PID_CHUNK`]-sized chunks, runs
/// [`process_cwds_chunk`] on each, and merges the results into one `pid -> cwd`
/// map. An empty `pids` slice yields an empty map.
/// Test: `parse_lsof_cwds_maps_pid_to_path` covers the per-chunk parsing.
fn process_cwds(pids: &[u32]) -> std::collections::HashMap<u32, String> {
    let mut map = std::collections::HashMap::new();
    for chunk in pids.chunks(LSOF_PID_CHUNK) {
        map.extend(process_cwds_chunk(chunk));
    }
    map
}

/// Last path component of a working directory, defaulting to `"session"`.
///
/// Why: the native session's friendly name is `<cwd-basename>-<pid>`; a missing
/// or root cwd must still yield a usable label.
/// What: returns the final non-empty path segment, or `"session"` as fallback.
/// Test: `cwd_basename_extracts_last_component`.
fn cwd_basename(cwd: &str) -> String {
    cwd.trim_end_matches('/')
        .rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or("session")
        .to_string()
}

/// Scan the OS process table and register native Claude Code processes.
///
/// Why: most Claude Code sessions run in native Terminal.app windows, not tmux;
/// `discover_claude_sessions` alone leaves them invisible. Scanning `ps aux`
/// brings them into `GET /sessions` like any other session.
/// What: runs `ps aux`, collects every pid satisfying [`is_claude_process`],
/// resolves all their working directories in one batched `lsof` call, and
/// registers one [`Session`] per distinct working directory, tagged
/// [`SessionHost::Native`] with `name = "<cwd-basename>-<pid>"`. Claude Code
/// spawns several `claude` subprocesses per session, so processes are
/// de-duplicated by working directory (the lowest pid wins). Directories already
/// registered as a session — by pid, by name, or by an existing native
/// session's workdir — are skipped. `ps` being unavailable yields an empty
/// [`DiscoveryResult`].
/// Test: `is_claude_process_*`, `parse_ps_line_extracts_pid_and_command`,
/// `parse_lsof_cwds_maps_pid_to_path`, and `cwd_basename_extracts_last_component`
/// cover the pure pieces.
pub fn discover_native_processes(state: &DaemonState) -> DiscoveryResult {
    let output = match Command::new("ps").arg("aux").output() {
        Ok(out) if out.status.success() => out,
        Ok(_) | Err(_) => {
            tracing::info!("`ps` unavailable; native process discovery skipped");
            return DiscoveryResult::default();
        }
    };
    let raw = String::from_utf8_lossy(&output.stdout);

    // Already-registered pids, friendly names, and native workdirs — never
    // register the same session twice across repeated scans.
    let existing = state.list_sessions();
    let registered_pids: HashSet<u32> = existing.iter().filter_map(|s| s.pid).collect();
    let registered_names: HashSet<String> = existing.iter().map(|s| s.tmux_name.clone()).collect();
    let registered_workdirs: HashSet<String> = existing
        .iter()
        .filter(|s| s.origin == SessionHost::Native && !s.workdir.is_empty())
        .map(|s| s.workdir.clone())
        .collect();

    // Pass 1: collect candidate pids (sorted, so the lowest pid wins per cwd).
    let mut candidate_pids: Vec<u32> = raw
        .lines()
        .filter_map(parse_ps_line)
        .filter(|(pid, cmdline)| is_claude_process(cmdline) && !registered_pids.contains(pid))
        .map(|(pid, _)| pid)
        .collect();
    candidate_pids.sort_unstable();
    candidate_pids.dedup();

    // Pass 2: resolve every candidate's working directory in one `lsof` call.
    let cwds = process_cwds(&candidate_pids);

    // Pass 3: register one native session per distinct working directory.
    let mut result = DiscoveryResult::default();
    let mut seen_workdirs: HashSet<String> = HashSet::new();
    for pid in candidate_pids {
        let cwd = cwds.get(&pid).cloned().unwrap_or_default();
        // Collapse the many `claude` subprocesses of one session: a directory
        // already claimed (this scan or a prior one) yields no new session.
        if !cwd.is_empty()
            && (registered_workdirs.contains(&cwd) || !seen_workdirs.insert(cwd.clone()))
        {
            continue;
        }
        let name = format!("{}-{}", cwd_basename(&cwd), pid);
        if registered_names.contains(&name) {
            continue;
        }
        // Native processes are not tmux-hosted; tag the control model `Pty`
        // (an OS-owned terminal) and the origin `Native`.
        let mut session = Session::new(SessionId::new(), cwd.clone(), ControlModel::Pty, None);
        session.tmux_name = name.clone();
        session.status = SessionStatus::Active;
        session.origin = SessionHost::Native;
        session.pid = Some(pid);
        state.register_session(session);
        tracing::info!("auto-discovered native Claude Code process: {name} (pid {pid})");
        result.adopted += 1;
        result.sessions.push(name);
    }
    result
}

/// Run every discovery scan (tmux panes and native processes).
///
/// Why: startup and `POST /sessions/discover` must surface Claude Code wherever
/// it runs; a single entry point keeps both scans in lockstep.
/// What: runs [`discover_claude_sessions`] then [`discover_native_processes`]
/// and merges their [`DiscoveryResult`]s.
/// Test: covered indirectly — each scan has its own unit coverage, and the
/// merge is exercised by `discover_all_merges_results`.
pub fn discover_all(state: &DaemonState) -> DiscoveryResult {
    let mut result = discover_claude_sessions(state);
    let native = discover_native_processes(state);
    result.adopted += native.adopted;
    result.sessions.extend(native.sessions);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_claude_command_matches_known() {
        for cmd in [
            "claude",
            "claude-code",
            "claude-mpm",
            "tm",
            "Claude",
            "CLAUDE-CODE",
        ] {
            assert!(is_claude_command(cmd), "expected `{cmd}` to match");
        }
    }

    #[test]
    fn is_claude_command_rejects_others() {
        for cmd in ["bash", "zsh", "vim", "tmux", "node", "", "  "] {
            assert!(!is_claude_command(cmd), "expected `{cmd}` not to match");
        }
    }

    #[test]
    fn parse_pane_line_splits_fields() {
        assert_eq!(
            parse_pane_line("my-project claude"),
            Some(("my-project".to_string(), "claude".to_string())),
        );
        // Extra whitespace between fields is tolerated.
        assert_eq!(
            parse_pane_line("  proj   claude-code  "),
            Some(("proj".to_string(), "claude-code".to_string())),
        );
        // A line with no command field is rejected.
        assert_eq!(parse_pane_line("lonely"), None);
        assert_eq!(parse_pane_line(""), None);
    }

    #[test]
    fn discover_with_no_tmux_is_empty() {
        // In CI tmux is typically absent (or hosts no Claude panes); discovery
        // must return a well-formed empty result, never panic.
        let state = DaemonState::new();
        let result = discover_claude_sessions(&state);
        assert_eq!(result.adopted, result.sessions.len());
    }

    #[test]
    fn is_claude_process_matches() {
        // The executable basename must be exactly `claude` / `claude-code`.
        for cmd in [
            "claude",
            "claude-code",
            "/usr/local/bin/claude --resume",
            "/opt/homebrew/bin/claude-code",
            "CLAUDE --dangerously-skip-permissions",
            // Regression: a real PM session whose `--system-prompt-file` path
            // contains `.claude-mpm` must still match — the path must not
            // disqualify the session.
            "claude --dangerously-skip-permissions \
             --system-prompt-file /Users/bob/proj/.claude-mpm/PM_INSTRUCTIONS.md",
        ] {
            assert!(is_claude_process(cmd), "expected `{cmd}` to match");
        }
    }

    #[test]
    fn is_claude_process_rejects_noise() {
        for cmd in [
            // The executable is not `claude` — basename is grep/tm/node/etc.
            "grep claude",
            "tm daemon",
            "/usr/bin/trusty-mpm daemon",
            "python3 -m claude_mpm.mcp.messaging_server",
            "uv tool uvx --from claude-mpm claude-mpm",
            "/Applications/Claude.app/Contents/Helpers/chrome-native-host",
            "node /opt/claude-code/cli.js",
            "vim notes.txt",
            "",
            "  ",
        ] {
            assert!(!is_claude_process(cmd), "expected `{cmd}` not to match");
        }
    }

    #[test]
    fn parse_ps_line_extracts_pid_and_command() {
        let line = "bob  12345  0.1  0.5  4096  2048 s001  S  10:00AM  0:01.23 /usr/local/bin/claude --resume";
        assert_eq!(
            parse_ps_line(line),
            Some((12345, "/usr/local/bin/claude --resume".to_string())),
        );
        // The `ps aux` header row has no numeric pid and must be rejected.
        assert_eq!(
            parse_ps_line("USER PID %CPU %MEM VSZ RSS TT STAT STARTED TIME COMMAND"),
            None,
        );
        assert_eq!(parse_ps_line(""), None);
    }

    #[test]
    fn parse_lsof_cwds_maps_pid_to_path() {
        // `lsof -Ffpn` emits `p<pid>`, then `f<fd>` / `n<path>` pairs. Only the
        // path following an `fcwd` marker is the working directory; the `ftxt`
        // path (the executable) must be ignored.
        let out = "p123\nfcwd\nn/Users/bob/Projects/alpha\nftxt\nn/usr/local/bin/claude\n\
                   p456\nfcwd\nn/Users/bob/Projects/beta\n";
        let map = parse_lsof_cwds(out);
        assert_eq!(map.len(), 2);
        assert_eq!(
            map.get(&123).map(String::as_str),
            Some("/Users/bob/Projects/alpha"),
        );
        assert_eq!(
            map.get(&456).map(String::as_str),
            Some("/Users/bob/Projects/beta"),
        );
        // A process with only a `txt` descriptor contributes no cwd entry.
        assert!(parse_lsof_cwds("p789\nftxt\nn/usr/local/bin/claude\n").is_empty());
        assert!(parse_lsof_cwds("p789\n").is_empty());
        assert!(parse_lsof_cwds("").is_empty());
    }

    #[test]
    fn cwd_basename_extracts_last_component() {
        assert_eq!(cwd_basename("/Users/bob/Projects/trusty-mpm"), "trusty-mpm");
        assert_eq!(
            cwd_basename("/Users/bob/Projects/trusty-mpm/"),
            "trusty-mpm"
        );
        assert_eq!(cwd_basename(""), "session");
        assert_eq!(cwd_basename("/"), "session");
    }

    #[test]
    fn discover_native_with_no_processes_is_well_formed() {
        // `ps` is present in CI but is unlikely to host a Claude Code process;
        // the scan must return a well-formed result and never panic.
        let state = DaemonState::new();
        let result = discover_native_processes(&state);
        assert_eq!(result.adopted, result.sessions.len());
    }

    #[test]
    fn discover_all_merges_results() {
        let state = DaemonState::new();
        let result = discover_all(&state);
        assert_eq!(result.adopted, result.sessions.len());
    }
}
