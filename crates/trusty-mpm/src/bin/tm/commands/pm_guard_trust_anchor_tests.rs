//! Unit tests for the #8878 trust-anchor floor (`pm_guard_trust_anchor.rs`).

use super::*;
use serde_json::json;
use trusty_mpm::core::session_profile::SupervisorConfig;

/// A scratch home holding the config anchor, and a working directory.
struct Fixture {
    _dir: tempfile::TempDir,
    home: PathBuf,
    cwd: PathBuf,
    anchor: PathBuf,
    project: PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonical tempdir");
    let home = root.join("home");
    std::fs::create_dir_all(home.join(ANCHOR_ROOT)).expect("mkdir anchor root");
    let anchor = home.join(ANCHOR_ROOT).join(CONFIG_ANCHOR);
    std::fs::write(&anchor, "[supervisor]\n").expect("write anchor");
    let cwd = root.join("work");
    std::fs::create_dir_all(&cwd).expect("mkdir work");
    std::fs::write(cwd.join("a.txt"), "a\n").expect("write a.txt");
    std::fs::write(cwd.join(CONFIG_ANCHOR), "copy\n").expect("write copy source");
    let project = root.join("architect");
    std::fs::create_dir_all(&project).expect("mkdir architect");
    std::fs::write(
        project.join(".trusty-mpm.toml"),
        "profile = \"supervisor\"\n",
    )
    .expect("write project profile");
    Fixture {
        _dir: dir,
        home,
        cwd,
        anchor,
        project,
    }
}

/// A PM session: a known home, no supervisor stamp.
fn pm_env(fx: &Fixture) -> HookEnv {
    HookEnv {
        home: Some(fx.home.clone()),
        ..HookEnv::default()
    }
}

/// The fully granted Architect: stamp, launch directory and allowlist agree,
/// and the hook's parent is the `claude` tm launched and recorded.
fn architect_env(fx: &Fixture) -> HookEnv {
    launch_record(fx, ARCHITECT, &fx.project);
    HookEnv {
        claude: table(architect_table()),
        ..spoof_env(fx)
    }
}

/// The environment half of the Architect, with no process behind it.
fn spoof_env(fx: &Fixture) -> HookEnv {
    HookEnv {
        home: Some(fx.home.clone()),
        stamp: Some("supervisor".into()),
        project_dir: Some(fx.project.clone().into_os_string()),
        ..HookEnv::default()
    }
}

/// The `claude` `tm fleet init` launched: its PID and start time.
const ARCHITECT: ClaudeProcess = ClaudeProcess {
    pid: 60,
    start_time: 600,
};

/// One fake process-table row: `(pid, parent, is claude)`.
type Row = (u32, Option<u32>, bool);

/// The hook (PID 100) run directly by the Architect's `claude` (PID 60).
fn architect_table() -> Vec<Row> {
    vec![(100, Some(60), false), (60, Some(1), true)]
}

/// The walk from the hook, PID 100, over `rows`, as PR 1's
/// `nearest_claude_in`: a PID's start time is `pid * 10`, and a PID absent
/// from `rows` is a table read error.
fn table(rows: Vec<Row>) -> ClaudeLookup {
    ClaudeLookup::new(move || {
        let find = |pid: u32| {
            rows.iter()
                .find(|row| row.0 == pid)
                .copied()
                .ok_or_else(|| format!("no pid {pid}"))
        };
        twin_arming::nearest_claude_in(
            100,
            |pid| {
                find(pid).map(|row| twin_arming::ProcessFacts {
                    parent: row.1,
                    start_time: u64::from(pid) * 10,
                })
            },
            |pid| find(pid).map(|row| row.2),
        )
    })
}

/// Write the launch record tm writes for `claude` started in `project`.
fn launch_record(fx: &Fixture, claude: ClaudeProcess, project: &Path) -> PathBuf {
    let record = trusty_mpm::core::twin_identity::ArmingRecord {
        pid: claude.pid,
        start_time: claude.start_time,
        project_dir: project.to_path_buf(),
        armed_at: String::new(),
    };
    architect_launch::ARCHITECT_RECORDS
        .write(&fx.home.join(ANCHOR_ROOT), &record)
        .expect("write launch record")
}

fn allowlist(fx: &Fixture) -> MpmConfig {
    MpmConfig {
        supervisor: SupervisorConfig {
            projects: vec![fx.project.clone()],
            ..SupervisorConfig::default()
        },
        ..MpmConfig::default()
    }
}

/// A main-thread payload for `tool`.
fn payload(fx: &Fixture, tool: &str, input: Value) -> Value {
    json!({
        "session_id": "s-1",
        "cwd": fx.cwd.display().to_string(),
        "tool_name": tool,
        "tool_input": input,
    })
}

fn pm_verdict(fx: &Fixture, tool: &str, input: Value) -> Option<String> {
    evaluate(&payload(fx, tool, input), &pm_env(fx), MpmConfig::default)
}

fn pm_bash(fx: &Fixture, command: &str) -> Option<String> {
    pm_verdict(fx, "Bash", json!({ "command": command }))
}

fn write_input(path: &Path) -> Value {
    json!({ "file_path": path.display().to_string(), "content": "x" })
}

#[test]
fn an_edit_tool_write_to_the_anchor_is_denied() {
    let fx = fixture();
    for tool in EDIT_TOOLS {
        let reason = pm_verdict(&fx, tool, write_input(&fx.anchor)).expect(tool);
        assert!(reason.contains("#8878"), "{tool}: {reason}");
        assert!(reason.contains("resolves to a trust anchor"), "{tool}");
    }
    // Through a symlink and through a hard link: the bytes land on the anchor.
    let soft = fx.cwd.join("soft.toml");
    std::os::unix::fs::symlink(&fx.anchor, &soft).expect("symlink");
    assert!(pm_verdict(&fx, "Write", write_input(&soft)).is_some());
    let hard = fx.cwd.join("hard.toml");
    std::fs::hard_link(&fx.anchor, &hard).expect("hard link");
    assert!(pm_verdict(&fx, "Write", write_input(&hard)).is_some());
    // `~` in an edit tool's path names the home too.
    let tilde = json!({ "file_path": "~/.trusty-mpm/config.toml", "content": "x" });
    assert!(pm_verdict(&fx, "Write", tilde).is_some());
}

#[test]
fn bash_write_shapes_onto_the_anchor_are_denied() {
    let fx = fixture();
    for command in [
        "echo x > ~/.trusty-mpm/config.toml",
        "echo x >> \"$HOME/.trusty-mpm/config.toml\"",
        "echo x | tee -a ~/.trusty-mpm/config.toml",
        "cp a.txt ~/.trusty-mpm/config.toml",
        "cp config.toml ~/.trusty-mpm/",
        "cp -t ~/.trusty-mpm config.toml",
        "cp -R src/ ~/.trusty-mpm",
        "sudo cp a.txt ${HOME}/.trusty-mpm/config.toml",
        "mv a.txt ~/.trusty-mpm/config.toml",
        "ln -s /tmp/evil ~/.trusty-mpm/config.toml",
        "ln -sf a.txt ~/.trusty-mpm/config.toml",
        "install -m 600 a.txt ~/.trusty-mpm/config.toml",
        "sed -i '' 's/a/b/' ~/.trusty-mpm/config.toml",
        "sed -i.bak -e 's/a/b/' ~/.trusty-mpm/config.toml",
        "sed --in-place 's/a/b/' ~/.trusty-mpm/config.toml",
        "cd ~/.trusty-mpm && echo x > config.toml",
        "cp a.txt ~/.trusty-mpm/*.toml",
        "cp a.txt $DIR/config.toml",
        "echo x > $OUT",
        // The documented limit: after a `cd`, `.` could be `~/.trusty-mpm`.
        "cd /tmp && cp a.txt .",
    ] {
        let reason = pm_bash(&fx, command).unwrap_or_else(|| panic!("allowed: {command}"));
        assert!(reason.contains("#8878"), "{command}: {reason}");
    }
}

#[test]
fn a_relative_path_resolves_against_the_hook_cwd() {
    let fx = fixture();
    let at_home = |command: &str| {
        let mut p = payload(&fx, "Bash", json!({ "command": command }));
        p["cwd"] = json!(fx.home.display().to_string());
        evaluate(&p, &pm_env(&fx), MpmConfig::default)
    };
    assert!(at_home("cp ../work/a.txt .trusty-mpm/config.toml").is_some());
    assert!(at_home("sed -i s/a/b/ ./.trusty-mpm/config.toml").is_some());
    assert!(at_home("echo x > .trusty-mpm/notes.md").is_none());
}

#[test]
fn anchor_verbs_reach_a_nested_body() {
    let fx = fixture();
    for command in [
        "bash -c 'cp a.txt ~/.trusty-mpm/config.toml'",
        "(mv a.txt ~/.trusty-mpm/config.toml)",
        "echo $(install a.txt ~/.trusty-mpm/config.toml)",
    ] {
        assert!(pm_bash(&fx, command).is_some(), "allowed: {command}");
    }
}

#[test]
fn reads_and_other_writes_stay_allowed() {
    let fx = fixture();
    for command in [
        "cat ~/.trusty-mpm/config.toml",
        "grep supervisor ~/.trusty-mpm/config.toml",
        "sed -n 1p ~/.trusty-mpm/config.toml",
        "ls ~/.trusty-mpm",
        "cp ~/.trusty-mpm/config.toml backup.toml",
        "cp a.txt ~/.trusty-mpm/notes.md",
        "cp a.txt ~/.trusty-mpm/",
        "mv ~/.trusty-mpm/sessions/s.json /tmp/s.json",
        "ln -s ~/.trusty-mpm/logs latest",
        "echo x > ~/.trusty-mpm/logs/tm.log",
        "echo x > notes.md",
        "sed -i 's/a.*/b/' a.txt",
        "sed -i '' -e 's/[0-9]/x/' a.txt",
        "echo x > \"$SP/out.log\"",
    ] {
        assert_eq!(pm_bash(&fx, command), None, "denied: {command}");
    }
    let read = json!({ "file_path": fx.anchor.display().to_string() });
    assert_eq!(pm_verdict(&fx, "Read", read), None);
}

#[test]
fn the_architect_main_thread_may_write_the_anchor() {
    let fx = fixture();
    let env = architect_env(&fx);
    let write = payload(&fx, "Write", write_input(&fx.anchor));
    assert_eq!(evaluate(&write, &env, || allowlist(&fx)), None);
    let cp = payload(
        &fx,
        "Bash",
        json!({ "command": "cp a.txt ~/.trusty-mpm/config.toml" }),
    );
    assert_eq!(evaluate(&cp, &env, || allowlist(&fx)), None);
}

#[test]
fn an_architect_subagent_is_denied() {
    let fx = fixture();
    let mut dispatched = payload(&fx, "Write", write_input(&fx.anchor));
    dispatched["agent_id"] = json!("agent-7");
    assert!(evaluate(&dispatched, &architect_env(&fx), || allowlist(&fx)).is_some());
    let nested = HookEnv {
        sub_agent: true,
        ..architect_env(&fx)
    };
    let write = payload(&fx, "Write", write_input(&fx.anchor));
    assert!(evaluate(&write, &nested, || allowlist(&fx)).is_some());
}

#[test]
fn an_unestablished_identity_is_denied() {
    let fx = fixture();
    // No `session_id`: the payload does not establish the main thread.
    let mut unknown = payload(&fx, "Write", write_input(&fx.anchor));
    unknown
        .as_object_mut()
        .expect("object payload")
        .remove("session_id");
    assert!(evaluate(&unknown, &architect_env(&fx), || allowlist(&fx)).is_some());
    let write = payload(&fx, "Write", write_input(&fx.anchor));
    // A stamp the allowlist does not back, and an allowlist with no stamp.
    assert!(evaluate(&write, &architect_env(&fx), MpmConfig::default).is_some());
    let unstamped = HookEnv {
        stamp: None,
        ..architect_env(&fx)
    };
    assert!(evaluate(&write, &unstamped, || allowlist(&fx)).is_some());
}

#[test]
fn an_unknown_home_denies_every_write() {
    let fx = fixture();
    let env = HookEnv::default();
    let elsewhere = fx.cwd.join("notes.md");
    let write = payload(&fx, "Write", write_input(&elsewhere));
    let reason = evaluate(&write, &env, MpmConfig::default).expect("denied");
    assert!(reason.contains("home directory is unknown"), "{reason}");
    let redirect = payload(&fx, "Bash", json!({ "command": "echo x > notes.md" }));
    assert!(evaluate(&redirect, &env, MpmConfig::default).is_some());
    // A call that writes nothing is not write-shaped.
    let ls = payload(&fx, "Bash", json!({ "command": "ls -la" }));
    assert_eq!(evaluate(&ls, &env, MpmConfig::default), None);
}

#[test]
fn an_unresolvable_target_is_denied() {
    let fx = fixture();
    let looped = fx.cwd.join("loop.toml");
    std::os::unix::fs::symlink(&looped, &looped).expect("self symlink");
    let reason = pm_verdict(&fx, "Write", write_input(&looped)).expect("denied");
    assert!(reason.contains("does not resolve"), "{reason}");
    assert!(pm_bash(&fx, "echo x > loop.toml").is_some());
}

#[test]
fn a_dangling_symlink_onto_the_anchor_is_denied() {
    let fx = fixture();
    std::fs::remove_file(&fx.anchor).expect("remove anchor");
    let dangling = fx.cwd.join("dangling.toml");
    std::os::unix::fs::symlink(&fx.anchor, &dangling).expect("symlink");
    let reason = pm_verdict(&fx, "Write", write_input(&dangling)).expect("denied");
    assert!(reason.contains("resolves to a trust anchor"), "{reason}");
    // Creating the absent anchor is a write to it.
    assert!(pm_verdict(&fx, "Write", write_input(&fx.anchor)).is_some());
}

#[test]
fn a_live_armed_record_is_an_anchor() {
    let fx = fixture();
    let armed = fx.home.join(ANCHOR_ROOT).join(ARMED_DIR);
    std::fs::create_dir_all(&armed).expect("mkdir armed");
    // Not live yet: no record exists.
    assert_eq!(
        pm_verdict(&fx, "Write", write_input(&armed.join("2.json"))),
        None
    );
    std::fs::write(armed.join("1.json"), "{}").expect("write record");
    assert!(pm_verdict(&fx, "Write", write_input(&armed.join("1.json"))).is_some());
    assert!(pm_verdict(&fx, "Write", write_input(&armed.join("2.json"))).is_some());
    assert!(pm_bash(&fx, "cp a.txt ~/.trusty-mpm/twin/armed/").is_none());
}

#[test]
fn an_unplaceable_write_is_denied() {
    let fx = fixture();
    let reason = pm_bash(&fx, "echo $(cat > x").expect("denied");
    assert!(reason.contains("cannot tell whether"), "{reason}");
}

/// Run `command` as the PM from `cwd`.
fn pm_bash_in(fx: &Fixture, cwd: &Path, command: &str) -> Option<String> {
    let mut p = payload(fx, "Bash", json!({ "command": command }));
    p["cwd"] = json!(cwd.display().to_string());
    evaluate(&p, &pm_env(fx), MpmConfig::default)
}

/// Make the arming directory live with one record; returns the directory.
fn arm(fx: &Fixture) -> PathBuf {
    let armed = fx.home.join(ANCHOR_ROOT).join(ARMED_DIR);
    std::fs::create_dir_all(&armed).expect("mkdir armed");
    std::fs::write(armed.join("1.json"), "{}").expect("write record");
    armed
}

#[test]
fn a_link_or_rename_source_on_an_anchor_is_denied() {
    let fx = fixture();
    for command in [
        "ln -s ~/.trusty-mpm/config.toml ~/s; cat e >> ~/s",
        "mv ~/.trusty-mpm ~/o && printf x > ~/o/config.toml && mv ~/o ~/.trusty-mpm",
        "ln -s ~/.trusty-mpm/config.toml link.toml",
        "ln ~/.trusty-mpm/config.toml hard.toml",
        "ln -t . ~/.trusty-mpm/config.toml",
        "mv ~/.trusty-mpm/config.toml old.toml",
        "mv -t /tmp ~/.trusty-mpm",
        "cp -l ~/.trusty-mpm/config.toml hard.toml",
        "cp -s ~/.trusty-mpm/config.toml soft.toml",
        "cp --symbolic-link ~/.trusty-mpm/config.toml soft.toml",
        // A symlink's text is read from the link's own directory.
        "ln -s .trusty-mpm/config.toml ../home/s",
        "cd /tmp && mv .trusty-mpm o",
    ] {
        let reason = pm_bash(&fx, command).unwrap_or_else(|| panic!("allowed: {command}"));
        assert!(reason.contains("#8878"), "{command}: {reason}");
    }
}

#[test]
fn bsd_install_s_and_sed_l_keep_their_operands() {
    let fx = fixture();
    for command in [
        "install -S a.txt ~/.trusty-mpm/config.toml",
        "install -SC a.txt ~/.trusty-mpm/config.toml",
        "sed -l -i '' s/a/b/ ~/.trusty-mpm/config.toml",
        "sed -li '' s/a/b/ ~/.trusty-mpm/config.toml",
    ] {
        assert!(pm_bash(&fx, command).is_some(), "allowed: {command}");
    }
    // GNU reads a `-i` word after the script as a file, not a suffix.
    let at_home = pm_bash_in(&fx, &fx.home, "sed -e s/a/b/ -i .trusty-mpm/config.toml");
    assert!(
        at_home.is_some(),
        "a suffix-shaped file operand was dropped"
    );
    assert_eq!(pm_bash(&fx, "sed -l -n p a.txt"), None);
}

#[test]
fn anchor_names_match_without_case() {
    let fx = fixture();
    std::fs::remove_file(&fx.anchor).expect("remove anchor");
    let root = fx.home.join(ANCHOR_ROOT);
    for path in [
        root.join("Config.TOML"),
        fx.home.join(".TRUSTY-MPM/config.toml"),
    ] {
        assert!(
            pm_verdict(&fx, "Write", write_input(&path)).is_some(),
            "allowed: {}",
            path.display()
        );
    }
    assert!(pm_bash(&fx, "cd /tmp && echo x > Config.TOML").is_some());
    let armed = arm(&fx);
    assert!(pm_verdict(&fx, "Write", write_input(&armed.join("2.JSON"))).is_some());
    assert!(pm_bash(&fx, "cd /tmp && echo x > r.JSON").is_some());
}

#[test]
fn a_q2_verb_through_xargs_is_denied() {
    let fx = fixture();
    for command in [
        "echo ~/.trusty-mpm/config.toml | xargs cp a.txt",
        "echo config.toml | xargs cp -t ~/.trusty-mpm",
        "echo config.toml | xargs install -t ~/.trusty-mpm",
        "echo ~/.trusty-mpm/config.toml | xargs sed -i s/a/b/",
        "echo ~/.trusty-mpm | xargs -I% mv % ~/o",
        "echo ~/.trusty-mpm/config.toml | xargs -I{} ln -s {} ~/s",
        "echo ~/.trusty-mpm | xargs mv -t /tmp",
        "ls | xargs -J % cp a.txt %",
    ] {
        let reason = pm_bash(&fx, command).unwrap_or_else(|| panic!("allowed: {command}"));
        assert!(reason.contains("#8878"), "{command}: {reason}");
    }
    std::fs::create_dir_all(fx.cwd.join("out")).expect("mkdir out");
    for command in [
        "ls | xargs cp -t out",
        "ls | xargs sed -n p",
        "ls | xargs grep x",
    ] {
        assert_eq!(pm_bash(&fx, command), None, "denied: {command}");
    }
    // An entry of the `-t` directory already linked onto the anchor.
    std::os::unix::fs::symlink(&fx.anchor, fx.cwd.join("out/c.toml")).expect("symlink");
    assert!(pm_bash(&fx, "ls | xargs cp -t out").is_some());
}

#[test]
fn a_blocked_arming_dir_fences_only_the_twin_tree() {
    let fx = fixture();
    // The blocking write, and writes of the arming directory itself.
    for command in [
        "echo x > ~/.trusty-mpm/twin",
        "install -d ~/.trusty-mpm/twin/armed",
        "mv a.txt ~/.trusty-mpm/twin",
    ] {
        assert!(pm_bash(&fx, command).is_some(), "allowed: {command}");
    }
    // Written anyway (outside the hook): only the twin tree stays fenced.
    std::fs::write(fx.home.join(ANCHOR_ROOT).join("twin"), "x").expect("block");
    assert_eq!(pm_bash(&fx, "echo x > notes.md"), None);
    assert_eq!(pm_bash(&fx, "echo x > ~/.trusty-mpm/logs.txt"), None);
    for command in [
        "echo x > ~/.trusty-mpm/twin/armed/1.json",
        "echo x > ~/.trusty-mpm/twin",
        "cd /tmp && echo x > r.json",
    ] {
        assert!(pm_bash(&fx, command).is_some(), "allowed: {command}");
    }
}

#[test]
fn cp_parents_judges_the_full_source_path() {
    let fx = fixture();
    let reason = pm_bash(&fx, "cp --parents .trusty-mpm/config.toml ~").expect("denied");
    assert!(reason.contains("#8878"), "{reason}");
    assert_eq!(pm_bash(&fx, "cp --parents src/a.txt ~"), None);
}

#[test]
fn an_unlocatable_anchor_denies_every_write() {
    let fx = fixture();
    std::fs::remove_file(&fx.anchor).expect("remove anchor");
    std::os::unix::fs::symlink(&fx.anchor, &fx.anchor).expect("self symlink");
    let reason = pm_bash(&fx, "echo x > notes.md").expect("denied");
    assert!(reason.contains("cannot locate the"), "{reason}");
    assert_eq!(pm_bash(&fx, "ls -la"), None);
}

#[test]
fn an_unreadable_armed_dir_is_live() {
    use std::os::unix::fs::PermissionsExt;
    let fx = fixture();
    let armed = fx.home.join(ANCHOR_ROOT).join(ARMED_DIR);
    std::fs::create_dir_all(&armed).expect("mkdir armed");
    // Not live: an empty, readable directory lets a `*.json` name through.
    assert_eq!(pm_bash(&fx, "cd /tmp && echo x > r.json"), None);
    std::fs::set_permissions(&armed, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let verdict = pm_bash(&fx, "cd /tmp && echo x > r.json");
    std::fs::set_permissions(&armed, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    assert!(
        verdict.is_some(),
        "an unreadable arming dir must count as live"
    );
}

/// The critic's spoof (#8878, ruling A): the supervisor stamp, a matching
/// project and the allowlist, as `TRUSTY_MPM_SESSION_PROFILE=supervisor claude`
/// run from the Architect directory produces, but no tm launch record.
#[test]
fn a_supervisor_stamp_without_a_launch_record_is_denied() {
    let fx = fixture();
    let spoof = HookEnv {
        home: Some(fx.home.clone()),
        stamp: Some("supervisor".into()),
        project_dir: Some(fx.project.clone().into_os_string()),
        ..HookEnv::default()
    };
    let write = payload(&fx, "Write", write_input(&fx.anchor));
    assert!(evaluate(&write, &spoof, || allowlist(&fx)).is_some());
    // The same spoof run under a real `claude` that tm never recorded.
    let unrecorded = HookEnv {
        claude: table(architect_table()),
        ..spoof_env(&fx)
    };
    assert!(evaluate(&write, &unrecorded, || allowlist(&fx)).is_some());
}

/// A launch record admits exactly the process it names (#8878, ruling A).
#[test]
fn a_launch_record_binds_one_process() {
    let write_by = |fx: &Fixture, env: &HookEnv| {
        evaluate(&payload(fx, "Write", write_input(&fx.anchor)), env, || {
            allowlist(fx)
        })
    };
    // The hook under the Architect's `claude`, with `record` on disk.
    let under_architect = |record: ClaudeProcess, project: Option<&Path>| {
        let fx = fixture();
        launch_record(&fx, record, project.unwrap_or(&fx.project));
        let env = HookEnv {
            claude: table(architect_table()),
            ..spoof_env(&fx)
        };
        write_by(&fx, &env)
    };
    assert_eq!(under_architect(ARCHITECT, None), None, "the real Architect");
    let other = ClaudeProcess {
        pid: 61,
        start_time: 610,
    };
    assert!(under_architect(other, None).is_some(), "another PID");
    let reused = ClaudeProcess {
        start_time: 599,
        ..ARCHITECT
    };
    assert!(under_architect(reused, None).is_some(), "the PID reused");
    let elsewhere = std::env::temp_dir();
    assert!(
        under_architect(ARCHITECT, Some(&elsewhere)).is_some(),
        "another project"
    );

    // A dead PID: the table no longer holds the recorded `claude`.
    let fx = fixture();
    let dead = HookEnv {
        claude: table(vec![(100, Some(60), false)]),
        ..architect_env(&fx)
    };
    assert!(write_by(&fx, &dead).is_some(), "a dead PID");

    // A session started from the Architect's Bash tool: the recorded
    // `claude` is not the hook's parent.
    let nested = HookEnv {
        claude: table(vec![
            (100, Some(80), false),
            (80, Some(70), false),
            (70, Some(60), false),
            (60, Some(1), true),
        ]),
        ..architect_env(&fx)
    };
    assert!(write_by(&fx, &nested).is_some(), "a nested session");
}

#[test]
fn an_unreadable_launch_record_is_denied() {
    use std::os::unix::fs::PermissionsExt;
    let fx = fixture();
    let env = architect_env(&fx);
    let record = launch_record(&fx, ARCHITECT, &fx.project);
    std::fs::write(&record, "not json").expect("corrupt record");
    let write = payload(&fx, "Write", write_input(&fx.anchor));
    assert!(evaluate(&write, &env, || allowlist(&fx)).is_some());
    launch_record(&fx, ARCHITECT, &fx.project);
    std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let unreadable = evaluate(&write, &env, || allowlist(&fx));
    std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o666)).expect("chmod");
    assert!(unreadable.is_some(), "an unreadable record");
    assert!(
        evaluate(&write, &env, || allowlist(&fx)).is_some(),
        "a record other users can write"
    );
}

/// The launch records are an anchor: no PM may plant, move or edit one.
#[test]
fn the_launch_record_dir_is_an_anchor() {
    let fx = fixture();
    let dir = fx.home.join(ANCHOR_ROOT).join(ARCHITECT_DIR);
    // Absent directory: creating it or a record in it is a write to it.
    for command in [
        "echo x > ~/.trusty-mpm/architect-launch/100.architect",
        "cp a.txt ~/.trusty-mpm/architect-launch",
        "cd /tmp && echo x > 100.architect",
        "cd /tmp && echo x > 100.ARCHITECT",
    ] {
        assert!(pm_bash(&fx, command).is_some(), "allowed: {command}");
    }
    launch_record(&fx, ARCHITECT, &fx.project);
    assert!(pm_verdict(&fx, "Write", write_input(&dir.join("60.architect"))).is_some());
    assert!(pm_verdict(&fx, "Write", write_input(&dir.join("notes.md"))).is_some());
    for command in [
        "mv ~/.trusty-mpm/architect-launch/60.architect ~/.trusty-mpm/architect-launch/100.architect",
        "ln -s ~/.trusty-mpm/architect-launch/60.architect a.architect",
        "cp a.txt ~/.trusty-mpm/architect-launch/",
    ] {
        assert!(pm_bash(&fx, command).is_some(), "allowed: {command}");
    }
    // The distinct extension keeps other unplaceable writes open.
    assert_eq!(pm_bash(&fx, "cd /tmp && echo x > r.json"), None);
    assert_eq!(pm_bash(&fx, "echo x > notes.md"), None);
}
