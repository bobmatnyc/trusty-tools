//! Tests for the supervisor-twin identity (#8878, ruling D1).
//!
//! Each fail-closed test breaks exactly ONE condition and keeps every other
//! condition met, so a test turns red when the check for its own condition
//! is removed or made to pass on error.

use super::*;
use crate::core::twin_arming::{
    self, OsProbe, load_user_config_strict, read_record, record_path, write_record,
};
use serde_json::json;
use std::ffi::OsString;
use tempfile::TempDir;

/// The armed `claude` every positive fixture binds to.
const CLAUDE: ClaudeProcess = ClaudeProcess {
    pid: 4242,
    start_time: 1_790_000_000,
};

/// A supervisor project whose user config grants it twin mode.
struct Twin {
    project: TempDir,
}

impl Twin {
    fn new() -> Self {
        let project = TempDir::new().expect("tempdir");
        std::fs::write(
            project.path().join(".trusty-mpm.toml"),
            "profile = \"supervisor\"\n",
        )
        .expect("write project config");
        Self { project }
    }

    fn dir(&self) -> &Path {
        self.project.path()
    }

    /// Supervisor allowlist and twin grant both name the project.
    fn config(&self) -> MpmConfig {
        let mut config = MpmConfig::default();
        config.supervisor.projects = vec![self.dir().to_path_buf()];
        config.supervisor.twin.projects = vec![self.dir().to_path_buf()];
        config
    }

    fn record(&self) -> ArmingRecord {
        ArmingRecord {
            pid: CLAUDE.pid,
            start_time: CLAUDE.start_time,
            project_dir: self.dir().to_path_buf(),
            armed_at: "2026-09-28T00:00:00Z".into(),
        }
    }

    /// A probe for which every OS-side condition holds.
    fn probe(&self) -> Fake {
        Fake {
            config: Ok(self.config()),
            claude: Ok(Some(CLAUDE)),
            record: Ok(Some(self.record())),
        }
    }
}

/// A [`TwinProbe`] returning fixed answers.
struct Fake {
    config: Result<MpmConfig, String>,
    claude: Result<Option<ClaudeProcess>, String>,
    record: Result<Option<ArmingRecord>, String>,
}

impl TwinProbe for Fake {
    fn user_config(&self) -> Result<MpmConfig, String> {
        self.config.clone()
    }
    fn nearest_claude(&self) -> Result<Option<ClaudeProcess>, String> {
        self.claude.clone()
    }
    fn arming_record(&self, _pid: u32) -> Result<Option<ArmingRecord>, String> {
        // The content of `<pid>.json`, whatever PID it names inside.
        self.record.clone()
    }
}

/// A main-thread `PreToolUse` payload.
fn main_payload() -> Value {
    json!({ "session_id": "s-1", "tool_name": "Bash", "tool_input": {} })
}

/// The status of a main-thread call in `twin`'s supervisor session.
fn status(twin: &Twin, probe: &Fake) -> TwinStatus {
    status_of(&main_payload(), twin, probe, false)
}

fn status_of(payload: &Value, twin: &Twin, probe: &Fake, sub_agent_env: bool) -> TwinStatus {
    let ctx = HookContext {
        payload,
        profile_stamp: Some(OsStr::new(SUPERVISOR_PROFILE_ID)),
        project_dir: Some(twin.dir().as_os_str()),
        sub_agent_env,
    };
    resolve(&ctx, probe)
}

fn refused(reason: TwinRefusal) -> TwinStatus {
    TwinStatus::Inactive(reason)
}

#[test]
fn all_conditions_arm_twin_mode() {
    let twin = Twin::new();
    assert_eq!(status(&twin, &twin.probe()), TwinStatus::Active(CLAUDE));
}

#[test]
fn a_subagent_call_is_not_twin() {
    let twin = Twin::new();
    let native = json!({ "session_id": "s-1", "agent_id": "agent-1" });
    assert_eq!(
        status_of(&native, &twin, &twin.probe(), false),
        refused(TwinRefusal::Subagent)
    );
    // An out-of-band trusty-agents subagent carries no `agent_id`.
    assert_eq!(
        status_of(&main_payload(), &twin, &twin.probe(), true),
        refused(TwinRefusal::Subagent)
    );
    // The hook entry point reads that marker from `CLAUDE_MPM_SUB_AGENT`.
    let dir = twin.dir().as_os_str().to_owned();
    let env = |name: &str| -> Option<OsString> {
        match name {
            "TRUSTY_MPM_SESSION_PROFILE" => Some(SUPERVISOR_PROFILE_ID.into()),
            "CLAUDE_PROJECT_DIR" => Some(dir.clone()),
            "CLAUDE_MPM_SUB_AGENT" => Some("1".into()),
            _ => None,
        }
    };
    assert_eq!(
        twin_arming::resolve_hook(&main_payload(), env, &twin.probe()),
        refused(TwinRefusal::Subagent)
    );
}

#[test]
fn an_unknown_thread_is_not_twin() {
    let twin = Twin::new();
    for payload in [
        json!(["not", "an", "object"]),
        json!({ "session_id": "s-1", "agent_id": "" }),
        json!({ "session_id": "s-1", "agent_id": null }),
        json!({ "session_id": "s-1", "agent_id": 7 }),
        json!({ "tool_name": "Bash" }),
        json!({ "session_id": "" }),
    ] {
        assert_eq!(
            status_of(&payload, &twin, &twin.probe(), false),
            refused(TwinRefusal::UnknownThread),
            "{payload}"
        );
    }
}

#[test]
fn a_session_without_the_supervisor_stamp_is_not_twin() {
    let twin = Twin::new();
    let payload = main_payload();
    for (stamp, dir) in [
        (None, Some(twin.dir().as_os_str())),
        (Some(OsStr::new("pm")), Some(twin.dir().as_os_str())),
        (Some(OsStr::new(SUPERVISOR_PROFILE_ID)), None),
        (
            Some(OsStr::new(SUPERVISOR_PROFILE_ID)),
            Some(OsStr::new("")),
        ),
    ] {
        let ctx = HookContext {
            payload: &payload,
            profile_stamp: stamp,
            project_dir: dir,
            sub_agent_env: false,
        };
        assert_eq!(
            resolve(&ctx, &twin.probe()),
            refused(TwinRefusal::NotSupervisor),
            "stamp {stamp:?}, dir {dir:?}"
        );
    }
}

#[test]
fn an_unreadable_user_config_is_not_twin() {
    let twin = Twin::new();
    let probe = Fake {
        config: Err("config.toml: permission denied".into()),
        ..twin.probe()
    };
    assert!(matches!(
        status(&twin, &probe),
        TwinStatus::Inactive(TwinRefusal::ConfigUnreadable(_))
    ));
}

#[test]
fn a_supervisor_without_the_twin_grant_is_not_twin() {
    let twin = Twin::new();
    let other = TempDir::new().expect("tempdir");
    for grant in [vec![], vec![other.path().to_path_buf()], vec!["rel".into()]] {
        let mut config = twin.config();
        config.supervisor.twin.projects = grant;
        let probe = Fake {
            config: Ok(config),
            ..twin.probe()
        };
        assert_eq!(status(&twin, &probe), refused(TwinRefusal::NoGrant));
    }
}

#[test]
fn a_twin_grant_without_the_supervisor_allowlist_is_not_twin() {
    let twin = Twin::new();
    let mut config = twin.config();
    config.supervisor.projects.clear();
    let probe = Fake {
        config: Ok(config),
        ..twin.probe()
    };
    assert_eq!(status(&twin, &probe), refused(TwinRefusal::NotSupervisor));
    // The project file must still ask for the supervisor profile.
    std::fs::write(twin.dir().join(".trusty-mpm.toml"), "profile = \"pm\"\n").expect("write");
    assert_eq!(
        status(&twin, &twin.probe()),
        refused(TwinRefusal::NotSupervisor)
    );
}

#[test]
fn a_process_table_error_is_not_twin() {
    let twin = Twin::new();
    let probe = Fake {
        claude: Err("the process table holds no entry for pid 9".into()),
        ..twin.probe()
    };
    assert!(matches!(
        status(&twin, &probe),
        TwinStatus::Inactive(TwinRefusal::ProcessTable(_))
    ));
}

#[test]
fn a_call_with_no_claude_ancestor_is_not_twin() {
    let twin = Twin::new();
    let probe = Fake {
        claude: Ok(None),
        ..twin.probe()
    };
    assert_eq!(
        status(&twin, &probe),
        refused(TwinRefusal::NoClaudeAncestor)
    );
}

#[test]
fn an_unarmed_session_is_not_twin() {
    let twin = Twin::new();
    let probe = Fake {
        record: Ok(None),
        ..twin.probe()
    };
    assert_eq!(status(&twin, &probe), refused(TwinRefusal::NotArmed));
}

#[test]
fn an_unreadable_arming_record_is_not_twin() {
    let twin = Twin::new();
    let probe = Fake {
        record: Err("4242.json: expected value at line 1".into()),
        ..twin.probe()
    };
    assert!(matches!(
        status(&twin, &probe),
        TwinStatus::Inactive(TwinRefusal::ArmingUnreadable(_))
    ));
}

#[test]
fn a_reused_pid_does_not_inherit_the_arming() {
    let twin = Twin::new();
    // Same PID, different start time: the OS reused the armed claude's PID.
    let later = ArmingRecord {
        start_time: CLAUDE.start_time - 1,
        ..twin.record()
    };
    // A record whose own PID disagrees with the file it was read from.
    let other_pid = ArmingRecord {
        pid: CLAUDE.pid + 1,
        ..twin.record()
    };
    for record in [later, other_pid] {
        let probe = Fake {
            record: Ok(Some(record)),
            ..twin.probe()
        };
        assert_eq!(status(&twin, &probe), refused(TwinRefusal::BindingMismatch));
    }
}

#[test]
fn a_record_armed_for_another_project_is_not_twin() {
    let twin = Twin::new();
    let other = TempDir::new().expect("tempdir");
    for project_dir in [other.path().to_path_buf(), twin.dir().join("gone")] {
        let probe = Fake {
            record: Ok(Some(ArmingRecord {
                project_dir,
                ..twin.record()
            })),
            ..twin.probe()
        };
        assert_eq!(status(&twin, &probe), refused(TwinRefusal::ProjectMismatch));
    }
}

#[test]
fn unrestricted_and_disable_hooks_do_not_imply_twin_mode() {
    let twin = Twin::new();
    // Every OS-side condition holds; only the two bypass variables are set.
    let env = |name: &str| -> Option<OsString> {
        matches!(
            name,
            "TRUSTY_MPM_PM_UNRESTRICTED" | "TRUSTY_MPM_DISABLE_HOOKS"
        )
        .then(|| "1".into())
    };
    assert_eq!(
        twin_arming::resolve_hook(&main_payload(), env, &twin.probe()),
        refused(TwinRefusal::NotSupervisor)
    );
    // With the stamp and launch directory added, the bypass variables change
    // nothing: the identity is the one the D1 conditions give.
    let dir = twin.dir().as_os_str().to_owned();
    let full = |name: &str| -> Option<OsString> {
        match name {
            "TRUSTY_MPM_SESSION_PROFILE" => Some(SUPERVISOR_PROFILE_ID.into()),
            "CLAUDE_PROJECT_DIR" => Some(dir.clone()),
            other => env(other),
        }
    };
    let unarmed = Fake {
        record: Ok(None),
        ..twin.probe()
    };
    assert_eq!(
        twin_arming::resolve_hook(&main_payload(), full, &unarmed),
        refused(TwinRefusal::NotArmed)
    );
}

#[test]
fn a_malformed_user_config_is_an_error_not_a_default() {
    let root = TempDir::new().expect("tempdir");
    let project = TempDir::new().expect("tempdir");
    // Absent: no grant, and no error.
    let absent = load_user_config_strict(root.path()).expect("absent is not an error");
    assert_eq!(absent.supervisor, Default::default());
    let grant = format!(
        "[supervisor]\nprojects = [{p:?}]\n\n[supervisor.twin]\nprojects = [{p:?}]\n",
        p = project.path()
    );
    std::fs::write(root.path().join("config.toml"), &grant).expect("write");
    let parsed = load_user_config_strict(root.path()).expect("a valid grant parses");
    assert_eq!(
        parsed.supervisor.twin.projects,
        vec![project.path().to_path_buf()]
    );
    // The same grant after a syntax error: an error, never the defaults.
    std::fs::write(root.path().join("config.toml"), format!("{grant}\n[oops")).expect("write");
    assert!(load_user_config_strict(root.path()).is_err());
    // Unreadable (a directory where the file should be): an error.
    std::fs::remove_file(root.path().join("config.toml")).expect("rm");
    std::fs::create_dir(root.path().join("config.toml")).expect("mkdir");
    assert!(load_user_config_strict(root.path()).is_err());
    // The OS probe reports it through the same loader.
    let probe = OsProbe {
        root: Some(root.path().to_path_buf()),
        start_pid: std::process::id(),
    };
    assert!(probe.user_config().is_err());
    let homeless = OsProbe {
        root: None,
        ..probe
    };
    assert!(homeless.user_config().is_err());
    assert!(homeless.arming_record(CLAUDE.pid).is_err());
}

#[test]
fn an_arming_record_round_trips() {
    let root = TempDir::new().expect("tempdir");
    let twin = Twin::new();
    assert_eq!(read_record(root.path(), CLAUDE.pid), Ok(None));
    let path = write_record(root.path(), &twin.record()).expect("write");
    assert_eq!(path, record_path(root.path(), CLAUDE.pid));
    assert_eq!(
        read_record(root.path(), CLAUDE.pid),
        Ok(Some(twin.record()))
    );
    // Garbage and unknown fields are errors, not "not armed".
    std::fs::write(&path, b"not json").expect("write");
    assert!(read_record(root.path(), CLAUDE.pid).is_err());
    let mut extra = serde_json::to_value(twin.record()).expect("json");
    extra["twin"] = json!(true);
    std::fs::write(&path, extra.to_string()).expect("write");
    assert!(read_record(root.path(), CLAUDE.pid).is_err());
}

#[cfg(unix)]
#[test]
fn a_writable_by_others_arming_record_is_refused() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = TempDir::new().expect("tempdir");
    let twin = Twin::new();
    let path = write_record(root.path(), &twin.record()).expect("write");
    let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
    assert_eq!(mode & 0o077, 0, "the record is written 0600, got {mode:o}");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).expect("chmod");
    assert!(read_record(root.path(), CLAUDE.pid).is_err());
}

/// One fake process-table row: `(pid, parent, name read)`.
type Row = (u32, Option<u32>, Result<bool, &'static str>);

/// [`twin_arming::nearest_claude_in`] over `rows`; a PID's start time is
/// `pid * 10`, and a PID absent from `rows` is a table read error.
fn walk(rows: &[Row], start: u32) -> Result<Option<ClaudeProcess>, String> {
    let find = |pid: u32| {
        rows.iter()
            .find(|row| row.0 == pid)
            .ok_or_else(|| format!("no pid {pid}"))
    };
    twin_arming::nearest_claude_in(
        start,
        |pid| {
            find(pid).map(|row| twin_arming::ProcessFacts {
                parent: row.1,
                start_time: u64::from(pid) * 10,
            })
        },
        |pid| find(pid).and_then(|row| row.2.map_err(str::to_owned)),
    )
}

#[test]
fn the_walk_stops_at_the_nearest_claude() {
    // hook 100 → nested claude 80 → sh 70 → armed claude 60.
    let nested: [Row; 5] = [
        (100, Some(80), Ok(false)),
        (80, Some(70), Ok(true)),
        (70, Some(60), Ok(false)),
        (60, Some(50), Ok(true)),
        (50, Some(1), Ok(false)),
    ];
    let nearest = ClaudeProcess {
        pid: 80,
        start_time: 800,
    };
    assert_eq!(walk(&nested, 100), Ok(Some(nearest)));
    // The hook is never its own `claude` ancestor, and PID 1 ends the walk.
    let alone: [Row; 2] = [(100, Some(90), Ok(true)), (90, Some(1), Ok(false))];
    assert_eq!(walk(&alone, 100), Ok(None));
}

/// A session started from the twin's Bash tool is not the twin, even when its
/// process is not named `claude` (npm installs run as `node`): the armed
/// `claude` counts only as the hook's own parent (#8878 review).
#[test]
fn a_session_nested_under_the_twin_is_not_twin() {
    // hook 100 → sh 90 → node 80 → bash 70 → armed claude 60.
    let via_shells: [Row; 5] = [
        (100, Some(90), Ok(false)),
        (90, Some(80), Ok(false)),
        (80, Some(70), Ok(false)),
        (70, Some(60), Ok(false)),
        (60, Some(1), Ok(true)),
    ];
    assert_eq!(walk(&via_shells, 100), Ok(None));
    // `exec node …` from the Bash tool: hook 100 → node 80 → armed claude 60.
    let execd: [Row; 3] = [
        (100, Some(80), Ok(false)),
        (80, Some(60), Ok(false)),
        (60, Some(1), Ok(true)),
    ];
    assert_eq!(walk(&execd, 100), Ok(None));
    // The twin's own hook: hook 100 → armed claude 60.
    let own: [Row; 2] = [(100, Some(60), Ok(false)), (60, Some(1), Ok(true))];
    let armed = ClaudeProcess {
        pid: 60,
        start_time: 600,
    };
    assert_eq!(walk(&own, 100), Ok(Some(armed)));
}

/// A start process the table no longer holds is an error, never "no claude
/// ancestor" (#8878 review).
#[cfg(unix)]
#[test]
fn a_reaped_process_has_no_claude_ancestor_it_is_an_error() {
    let mut child = std::process::Command::new("true")
        .spawn()
        .expect("spawn true");
    let pid = child.id();
    child.wait().expect("reap true");
    let got = twin_arming::nearest_claude_ancestor(pid);
    assert!(got.is_err(), "{got:?}");
}

#[test]
fn an_unidentifiable_ancestor_stops_the_walk() {
    // hook 100 → 90, whose name cannot be read → armed claude 60.
    let unnamed: [Row; 3] = [
        (100, Some(90), Ok(false)),
        (90, Some(60), Err("ps: no such process")),
        (60, Some(1), Ok(true)),
    ];
    assert!(walk(&unnamed, 100).is_err(), "{:?}", walk(&unnamed, 100));
    // hook 100 → 80, absent from the table.
    let orphaned: [Row; 2] = [(100, Some(80), Ok(false)), (60, Some(1), Ok(true))];
    assert!(walk(&orphaned, 100).is_err());
}

/// A real process tree: `fake claude (sh) → sleep`. The walk from `sleep` must
/// stop at the fake claude and report the start time `arm_claude` records.
#[cfg(unix)]
#[test]
fn the_nearest_claude_ancestor_is_found_with_its_start_time() {
    use std::process::{Command, Stdio};
    let dir = TempDir::new().expect("tempdir");
    let fake = dir.path().join("claude");
    // A symlink, not a copy: macOS kills an unsigned copy of a system binary.
    std::os::unix::fs::symlink("/bin/sh", &fake).expect("symlink");
    let mut claude = Command::new(&fake)
        .args(["-c", "sleep 30 & wait"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn fake claude");
    let claude_pid = claude.id();
    let child = (0..50).find_map(|_| {
        let out = Command::new("pgrep")
            .args(["-P", &claude_pid.to_string()])
            .output()
            .ok()?;
        let pid = String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|l| l.trim().parse::<u32>().ok());
        if pid.is_none() {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        pid
    });
    let found = child.map(twin_arming::nearest_claude_ancestor);
    let root = TempDir::new().expect("tempdir");
    let armed = twin_arming::arm_claude(root.path(), claude_pid, dir.path());
    let _ = claude.kill();
    let _ = claude.wait();

    let found = found
        .expect("the fake claude spawned a child")
        .expect("the table reads")
        .expect("the fake claude is an ancestor");
    let armed = armed.expect("arming reads the start time");
    assert_eq!(found.pid, claude_pid);
    assert_eq!(found.start_time, armed.start_time);
    assert_eq!(read_record(root.path(), claude_pid), Ok(Some(armed)));
}
