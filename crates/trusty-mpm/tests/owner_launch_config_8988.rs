//! A managed launch under an owner file carries the owner's MCP deny list
//! (#8988 PR 1b).
//!
//! Why an INTEGRATION test: these assertions must run against the code BEFORE
//! the fix, to prove they were red there. `ClaudeCodeAdapter`,
//! `RuntimeAdapter::spawn`/`spawn_resume` and `ManagedTmuxDriver` are public
//! library API that predates #8988, so the same file compiles on either side.
//! What: plants three user-scope servers in the managed config dir's
//! `.claude.json` and an owner file at `<repos_root>/acme/.trusty-mpm-owner.toml`,
//! drives the REAL spawn and resume paths against a recording tmux, reads the
//! launch spec the typed line names, and asserts on the exact contents of the
//! file its `--settings` token names. A third case proves the no-owner launch
//! carries no `--settings` at all. #8992: the `[skills] off` / `[tools] deny`
//! cases drive the same three callers (spawn, resume, in-place relaunch) for a
//! PM and for an allow-listed Architect, which gets the `[mcp]` part only.
//! Test: this file IS the test.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use trusty_mpm::runtime::{ClaudeCodeAdapter, RuntimeAdapter};
use trusty_mpm::session_manager::{ManagedError, ManagedTmuxDriver};

/// The owner file's basename, as a literal: the constant moves with the fix.
const OWNER_FILE: &str = ".trusty-mpm-owner.toml";

/// Records each launch line typed into the pane and answers the pre-launch
/// `echo` handshake like a shell (see `pane_launch_line_8233.rs`).
#[derive(Default)]
struct Recorder {
    typed: Mutex<Vec<String>>,
    printed: Mutex<String>,
}

impl Recorder {
    fn record(&self, text: &str) {
        if let Some(word) = text.strip_prefix("echo ") {
            let mut out = self.printed.lock().expect("recorder mutex");
            out.push_str(&word.replace(['"', '\''], ""));
            out.push('\n');
            return;
        }
        self.typed
            .lock()
            .expect("recorder mutex")
            .push(text.to_owned());
    }
}

impl ManagedTmuxDriver for Recorder {
    fn create_session(&self, _name: &str, _workdir: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn kill_session(&self, _name: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn send_line(&self, _name: &str, text: &str) -> Result<(), ManagedError> {
        self.record(text);
        Ok(())
    }
    fn send_line_to_pane(&self, _name: &str, _pane: &str, text: &str) -> Result<(), ManagedError> {
        self.record(text);
        Ok(())
    }
    fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
        Ok(self.printed.lock().expect("recorder mutex").clone())
    }
    fn capture_pane(&self, name: &str, _pane: &str, lines: usize) -> Result<String, ManagedError> {
        self.capture(name, lines)
    }
    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(Vec::new())
    }
}

/// Restores `HOME`, `PATH` and `TRUSTY_MPM_REPOS_ROOT` when a case ends.
///
/// `env_serial` runs one test at a time, so no other test observes the rewrite.
struct EnvGuard {
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl EnvGuard {
    fn install(home: &Path, bin: &Path, repos_root: &Path) -> Self {
        let names = ["HOME", "PATH", "TRUSTY_MPM_REPOS_ROOT"];
        let saved = names.iter().map(|n| (*n, std::env::var_os(n))).collect();
        let mut entries = vec![bin.to_path_buf()];
        if let Some(p) = std::env::var_os("PATH") {
            entries.extend(std::env::split_paths(&p));
        }
        let path = std::env::join_paths(entries).expect("join PATH");
        // SAFETY: `env_serial` forces one test thread; Drop restores all three.
        unsafe {
            std::env::set_var("HOME", home);
            std::env::set_var("PATH", path);
            std::env::set_var("TRUSTY_MPM_REPOS_ROOT", repos_root);
        }
        Self { saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (name, value) in self.saved.drain(..) {
            // SAFETY: as in `install`.
            unsafe {
                match value {
                    Some(v) => std::env::set_var(name, v),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}

/// A scratch home: a fake `claude`, a project at `projects/acme/widget`, and a
/// managed `.claude.json` holding three user-scope servers.
struct Fixture {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    repos_root: PathBuf,
    project: PathBuf,
    _env: EnvGuard,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let home = tmp.path().canonicalize().expect("canonical tempdir");
        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).expect("mkdir bin");
        let exe = bin.join("claude");
        std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").expect("write fake claude");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let repos_root = home.join("projects");
        let project = repos_root.join("acme").join("widget");
        std::fs::create_dir_all(&project).expect("mkdir project");
        // The `.claude.json` the managed launch actually reads.
        let config_dir = home.join(".trusty-tools/trusty-mpm/claude-config");
        std::fs::create_dir_all(&config_dir).expect("mkdir config dir");
        std::fs::write(
            config_dir.join(".claude.json"),
            r#"{"mcpServers":{"heygen":{"type":"http","url":"https://h.invalid"},
                "apex":{"type":"http","url":"https://a.invalid"},
                "trusty-search":{"type":"stdio","command":"trusty-search"}}}"#,
        )
        .expect("write .claude.json");
        let env = EnvGuard::install(&home, &bin, &repos_root);
        Self {
            _tmp: tmp,
            home,
            repos_root,
            project,
            _env: env,
        }
    }

    fn owner_file(&self, body: &str) {
        std::fs::write(self.repos_root.join("acme").join(OWNER_FILE), body)
            .expect("write owner file");
    }

    fn adapter(&self, tmux: &Arc<Recorder>) -> ClaudeCodeAdapter {
        ClaudeCodeAdapter::new(tmux.clone(), &self.home.join(".trusty-mpm"))
    }

    /// Make the project an Architect (#8453): it asks for the supervisor
    /// profile and the operator's user config allow-lists it.
    fn make_architect(&self) {
        std::fs::write(
            self.project.join(".trusty-mpm.toml"),
            "profile = \"supervisor\"\n",
        )
        .expect("write project config");
        let root = self.home.join(".trusty-mpm");
        std::fs::create_dir_all(&root).expect("mkdir framework root");
        std::fs::write(
            root.join("config.toml"),
            format!(
                "[supervisor]\nprojects = [{:?}]\n",
                self.project.display().to_string()
            ),
        )
        .expect("write user config");
    }

    /// The argv of one fresh spawn in the project.
    fn spawn_args(&self) -> Vec<String> {
        let tmux = Arc::new(Recorder::default());
        self.adapter(&tmux)
            .spawn(
                "tm-acme-01",
                &self.project,
                "t",
                "c342df68-3e15-4b50-93fc-f24bcbae76c2",
                &[],
            )
            .expect("the spawn path must run with claude on PATH");
        launched_args(&tmux)
    }

    /// The argv of one resume in the project.
    fn resume_args(&self) -> Vec<String> {
        let tmux = Arc::new(Recorder::default());
        self.adapter(&tmux)
            .spawn_resume(
                "tm-acme-01",
                Some("%7"),
                &self.project,
                "t",
                None,
                "c342df68-3e15-4b50-93fc-f24bcbae76c2",
                &[],
            )
            .expect("the resume path must run with claude on PATH");
        launched_args(&tmux)
    }

    /// The argv of one in-place relaunch in the project.
    fn inplace_args(&self) -> Vec<String> {
        let fw = trusty_mpm::core::paths::FrameworkPaths::under(&self.home);
        trusty_mpm::runtime::build_inplace_resume_command_under(&fw, &self.project, None)
            .expect("the relaunch must build with claude on PATH")
            .args
    }
}

/// #8992: the owner file every Architect case uses — all three tables set.
const ALL_TABLES: &str = "[mcp]\ndeny = [\"heygen\"]\n\
                          [skills]\noff = [\"xlsx\"]\n\
                          [tools]\ndeny = [\"NotebookEdit\"]\n";

/// The value after each `--disallowedTools` token in `args`.
fn disallowed_tools(args: &[String]) -> Vec<String> {
    args.windows(2)
        .filter(|w| w[0] == "--disallowedTools")
        .map(|w| w[1].clone())
        .collect()
}

/// The argv of the one launch spec the recorder saw typed.
fn launched_args(tmux: &Recorder) -> Vec<String> {
    let typed = tmux.typed.lock().expect("recorder mutex").clone();
    let line = typed
        .iter()
        .find(|l| l.contains("--launch-spec"))
        .unwrap_or_else(|| panic!("no launch line was typed: {typed:?}"));
    let path = line
        .rsplit("--launch-spec ")
        .next()
        .expect("a spec path")
        .trim()
        .trim_matches('\'');
    let spec: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).expect("read the launch spec"))
            .expect("the launch spec is JSON");
    spec["args"]
        .as_array()
        .expect("spec args")
        .iter()
        .map(|a| a.as_str().expect("string arg").to_owned())
        .collect()
}

/// The parsed file the `--settings` token names, and its path.
fn settings_file(args: &[String]) -> (PathBuf, serde_json::Value) {
    let at = args
        .iter()
        .position(|a| a == "--settings")
        .unwrap_or_else(|| panic!("argv carries no --settings: {args:?}"));
    let path = PathBuf::from(&args[at + 1]);
    let body = std::fs::read(&path).expect("read the settings file");
    (
        path,
        serde_json::from_slice(&body).expect("the settings file is JSON"),
    )
}

/// #10127: an Architect's ONE `--settings` is inline JSON — the owner file's
/// `[mcp]` part merged with the lockdown deny and the connector switch.
fn assert_architect_payload(args: &[String]) {
    use trusty_mpm::core::architect_lockdown::{ARCHITECT_DENY_GLOBS, capitalised};
    let values: Vec<&String> = args
        .windows(2)
        .filter(|w| w[0] == "--settings")
        .map(|w| &w[1])
        .collect();
    assert_eq!(values.len(), 1, "one --settings: {args:?}");
    let body: serde_json::Value = serde_json::from_str(values[0]).expect("inline JSON");
    assert_eq!(
        body["deniedMcpServers"],
        serde_json::json!([{"serverName": "heygen"}])
    );
    assert_eq!(body["disableClaudeAiConnectors"], true);
    assert!(body.get("skillOverrides").is_none(), "{body}");
    let mut want: Vec<String> = ARCHITECT_DENY_GLOBS
        .iter()
        .flat_map(|g| [(*g).to_owned(), capitalised(g)])
        .collect();
    want.sort();
    want.dedup();
    assert_eq!(
        body["permissions"]["deny"],
        serde_json::json!(want),
        "{body}"
    );
}

/// #8988 closure condition: the owner's `allow` decides which user-scope
/// servers load; every other non-floor server is denied, and the floor
/// (`trusty-search`) is never named.
#[test]
#[cfg(unix)]
fn a_managed_spawn_under_an_owner_file_denies_the_unallowed_user_scope_servers() {
    let fx = Fixture::new();
    fx.owner_file("[mcp]\nallow = [\"apex\"]\n");
    let tmux = Arc::new(Recorder::default());

    fx.adapter(&tmux)
        .spawn(
            "tm-acme-01",
            &fx.project,
            "t",
            "c342df68-3e15-4b50-93fc-f24bcbae76c2",
            &[],
        )
        .expect("the spawn path must run with claude on PATH");

    let (path, settings) = settings_file(&launched_args(&tmux));
    assert_eq!(
        settings,
        serde_json::json!({"deniedMcpServers": [{"serverName": "heygen"}]})
    );
    assert!(
        path.starts_with(fx.home.join(".trusty-tools/trusty-mpm/session-settings")),
        "{}",
        path.display()
    );
    assert!(
        !path.starts_with(&fx.project),
        "never inside the repository"
    );
}

/// The resume path is a second caller; `allow = []` with the connectors off
/// denies every non-floor server and turns the claude.ai connectors off.
#[test]
#[cfg(unix)]
fn a_managed_resume_under_an_owner_file_carries_the_same_policy() {
    let fx = Fixture::new();
    fx.owner_file("[mcp]\nallow = []\nclaude_ai_connectors = false\n");
    let tmux = Arc::new(Recorder::default());

    fx.adapter(&tmux)
        .spawn_resume(
            "tm-acme-01",
            Some("%7"),
            &fx.project,
            "t",
            None,
            "c342df68-3e15-4b50-93fc-f24bcbae76c2",
            &[],
        )
        .expect("the resume path must run with claude on PATH");

    let (_, settings) = settings_file(&launched_args(&tmux));
    assert_eq!(
        settings,
        serde_json::json!({
            "deniedMcpServers": [{"serverName": "apex"}, {"serverName": "heygen"}],
            "disableClaudeAiConnectors": true
        })
    );
}

/// With no owner file the launch is byte-identical to today: no `--settings`,
/// and no settings file written. Green on both sides of the fix.
#[test]
#[cfg(unix)]
fn a_spawn_with_no_owner_file_carries_no_settings_flag() {
    let fx = Fixture::new();
    let tmux = Arc::new(Recorder::default());

    fx.adapter(&tmux)
        .spawn(
            "tm-acme-01",
            &fx.project,
            "t",
            "c342df68-3e15-4b50-93fc-f24bcbae76c2",
            &[],
        )
        .expect("the spawn path must run with claude on PATH");

    let args = launched_args(&tmux);
    assert!(!args.iter().any(|a| a == "--settings"), "{args:?}");
    assert!(
        !fx.home
            .join(".trusty-tools/trusty-mpm/session-settings")
            .exists()
    );
}

/// The in-place relaunch (bare `tm` inside a managed pane, exec'd by
/// `guided_inplace`) carries the file it provisioned, not a re-derived path.
#[test]
#[cfg(unix)]
fn an_inplace_relaunch_under_an_owner_file_carries_the_settings() {
    let fx = Fixture::new();
    fx.owner_file("[mcp]\ndeny = [\"heygen\"]\n");
    let fw = trusty_mpm::core::paths::FrameworkPaths::under(&fx.home);

    let resume = trusty_mpm::runtime::build_inplace_resume_command_under(&fw, &fx.project, None)
        .expect("the relaunch must build with claude on PATH");

    let (_, settings) = settings_file(&resume.args);
    assert_eq!(
        settings,
        serde_json::json!({"deniedMcpServers": [{"serverName": "heygen"}]})
    );
}

/// df-D0e through the launch path: deleting the owner file turns the layer off
/// AND retires its last-good record, so a later bad file has nothing to fall
/// back to and the launch carries no `--settings`.
#[test]
#[cfg(unix)]
fn a_deleted_owner_file_stays_off_when_a_bad_file_follows() {
    let fx = Fixture::new();
    let spawn_args = || {
        let tmux = Arc::new(Recorder::default());
        fx.adapter(&tmux)
            .spawn(
                "tm-acme-01",
                &fx.project,
                "t",
                "c342df68-3e15-4b50-93fc-f24bcbae76c2",
                &[],
            )
            .expect("spawn");
        launched_args(&tmux)
    };
    fx.owner_file("[mcp]\ndeny = [\"heygen\"]\n");
    assert!(spawn_args().iter().any(|a| a == "--settings"));

    std::fs::remove_file(fx.repos_root.join("acme").join(OWNER_FILE)).expect("delete");
    assert!(!spawn_args().iter().any(|a| a == "--settings"));

    fx.owner_file("[mcp\n");
    let args = spawn_args();
    assert!(
        !args.iter().any(|a| a == "--settings"),
        "a bad file after a delete must not revive the old policy: {args:?}"
    );
}

/// #8992: a PM spawn under an owner `[skills] off` carries the skill as
/// `"off"` in `skillOverrides`.
#[test]
#[cfg(unix)]
fn a_pm_spawn_under_owner_skills_off_carries_skill_overrides() {
    let fx = Fixture::new();
    fx.owner_file("[skills]\noff = [\"xlsx\"]\n");
    let (_, settings) = settings_file(&fx.spawn_args());
    assert_eq!(
        settings,
        serde_json::json!({"skillOverrides": {"xlsx": "off"}})
    );
}

/// #8992: a PM spawn under an owner `[tools] deny` carries it as
/// `permissions.deny`, and the PM trim argv is untouched: still exactly one
/// `--disallowedTools Workflow,DesignSync`.
#[test]
#[cfg(unix)]
fn a_pm_spawn_under_owner_tools_deny_carries_permissions_deny() {
    let fx = Fixture::new();
    fx.owner_file("[tools]\ndeny = [\"NotebookEdit\"]\n");
    let args = fx.spawn_args();
    let (_, settings) = settings_file(&args);
    assert_eq!(
        settings,
        serde_json::json!({"permissions": {"deny": ["NotebookEdit"]}})
    );
    assert_eq!(disallowed_tools(&args), vec!["Workflow,DesignSync"]);
}

/// #8992 Q-B: an Architect spawn gets the `[mcp]` part exactly as in #8988 1b,
/// and neither the skills nor the tools part.
#[test]
#[cfg(unix)]
fn an_architect_spawn_gets_the_mcp_part_but_no_tools_or_skills() {
    let fx = Fixture::new();
    fx.make_architect();
    fx.owner_file(ALL_TABLES);
    let args = fx.spawn_args();
    // #10127: plus the Architect lockdown, in the same payload.
    assert_architect_payload(&args);
    assert!(disallowed_tools(&args).is_empty(), "{args:?}");
}

/// #8992: the resume path is a separate caller; a PM resume carries all three.
#[test]
#[cfg(unix)]
fn a_pm_resume_under_owner_skills_and_tools_carries_both() {
    let fx = Fixture::new();
    fx.owner_file(ALL_TABLES);
    let args = fx.resume_args();
    let (_, settings) = settings_file(&args);
    assert_eq!(
        settings,
        serde_json::json!({
            "deniedMcpServers": [{"serverName": "heygen"}],
            "permissions": {"deny": ["NotebookEdit"]},
            "skillOverrides": {"xlsx": "off"}
        })
    );
    assert_eq!(disallowed_tools(&args), vec!["Workflow,DesignSync"]);
}

/// #8992 Q-B on the resume path.
#[test]
#[cfg(unix)]
fn an_architect_resume_gets_the_mcp_part_only() {
    let fx = Fixture::new();
    fx.make_architect();
    fx.owner_file(ALL_TABLES);
    // #10127: plus the Architect lockdown, in the same payload.
    assert_architect_payload(&fx.resume_args());
}

/// #8992: the in-place relaunch is the third caller; a PM relaunch carries
/// all three.
#[test]
#[cfg(unix)]
fn an_inplace_pm_relaunch_carries_skills_and_tools() {
    let fx = Fixture::new();
    fx.owner_file(ALL_TABLES);
    let args = fx.inplace_args();
    let (_, settings) = settings_file(&args);
    assert_eq!(
        settings,
        serde_json::json!({
            "deniedMcpServers": [{"serverName": "heygen"}],
            "permissions": {"deny": ["NotebookEdit"]},
            "skillOverrides": {"xlsx": "off"}
        })
    );
    assert_eq!(disallowed_tools(&args), vec!["Workflow,DesignSync"]);
}

/// #8992 Q-B on the in-place relaunch.
#[test]
#[cfg(unix)]
fn an_inplace_architect_relaunch_gets_the_mcp_part_only() {
    let fx = Fixture::new();
    fx.make_architect();
    fx.owner_file(ALL_TABLES);
    // #10127: plus the Architect lockdown, in the same payload.
    assert_architect_payload(&fx.inplace_args());
}
