//! Tests for the CLI and fleet launch spec (#8308).
//!
//! Why: the spec replaces a shell line, so each builder must carry what that
//! line carried, and the typed line must stay short with no secret in it.
//! What: hermetic — specs are written into each test's own `TempDir`, and tmux
//! is a fake script.
//! Test: this file.

use std::path::{Path, PathBuf};

use super::{CliLaunch, CliLaunchError};
use crate::runtime::launch_spec::LaunchSpec;

/// The env assignments as `K=V` words, for substring assertions.
fn env_words(spec: &LaunchSpec) -> Vec<String> {
    spec.env_set
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect()
}

/// A fake `tmux` that appends its argv to `<dir>/argv.log` and exits `exit`.
fn fake_tmux(dir: &Path, exit: u8) -> (String, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let log = dir.join("argv.log");
    let bin = dir.join("fake-tmux");
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> '{}'\necho 'fake failure' >&2\nexit {exit}\n",
        log.display()
    );
    std::fs::write(&bin, script).expect("fake tmux");
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    (bin.display().to_string(), log)
}

/// The files left in `dir`, sorted.
fn entries(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    out.sort();
    out
}

/// #8308: the isolated spec carries every variable and flag of the
/// `build_claude_command_with_configured` line, in the same order, and none of
/// its shell quoting.
#[test]
fn isolated_spec_matches_the_shell_line_it_replaces() {
    let env = vec![("TRUSTY_INDEX".to_owned(), "repo".to_owned())];
    let launch = CliLaunch {
        model: Some("opus"),
        prompt_file: Some(Path::new("/tmp/p.txt")),
        config_dir: Some(Path::new("/h/cfg dir")),
        oauth_token: Some("tok"),
        mcp_env: &env,
        scoped_mcp: Some(Path::new("/h/mcp.json")),
        alternate_screen: false,
    };
    let spec = super::isolated_spec(Path::new("/work"), &launch);
    let line = crate::core::model_inject::build_claude_command_with_configured(
        launch.model,
        launch.prompt_file,
        launch.config_dir,
        launch.oauth_token,
        launch.mcp_env,
        launch.scoped_mcp,
        launch.alternate_screen,
    );

    assert_eq!(spec.cwd, Path::new("/work"));
    assert_eq!(spec.program, "claude");
    assert_eq!(
        spec.env_unset,
        crate::core::claude_env_scrub::parse_env_unset_vars(&line),
        "the same scrub, in the same order: {line}"
    );
    assert_eq!(
        env_words(&spec),
        [
            "CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=1",
            "CLAUDE_CONFIG_DIR=/h/cfg dir",
            "CLAUDE_CODE_OAUTH_TOKEN=tok",
            "TRUSTY_INDEX=repo",
        ]
    );
    assert_eq!(
        spec.args,
        [
            "--model",
            "opus",
            "--append-system-prompt-file",
            "/tmp/p.txt",
            "--setting-sources",
            "user,project,local",
            "--mcp-config",
            "/h/mcp.json",
            "--dangerously-skip-permissions",
        ]
    );
    // A CLI launch has no daemon handshake: no session export, no sentinel.
    assert!(spec.session_id.is_empty() && spec.launch_id.is_empty());
}

/// #8308: the in-place and client specs carry the same scrub and renderer as
/// their shell lines, and only the flags those lines carried.
#[test]
fn inplace_and_client_specs_match_the_lines_they_replace() {
    for alternate_screen in [true, false] {
        let want = crate::core::alt_screen::configured_env(alternate_screen);
        let inplace = super::inplace_spec(Path::new("/w"), alternate_screen);
        let line =
            crate::core::model_inject::build_inplace_session_command_configured(alternate_screen);
        assert_eq!(
            inplace.env_unset,
            crate::core::claude_env_scrub::parse_env_unset_vars(&line)
        );
        assert_eq!(inplace.env_set, want);
        assert_eq!(inplace.args, ["--dangerously-skip-permissions"]);

        let client = super::client_spec(Path::new("/w"), Some(Path::new("/p")), alternate_screen);
        assert_eq!(client.env_unset, inplace.env_unset);
        assert_eq!(client.env_set, want);
        assert_eq!(client.args, ["--append-system-prompt-file", "/p"]);
        assert!(
            super::client_spec(Path::new("/w"), None, alternate_screen)
                .args
                .is_empty()
        );
    }
}

/// #8308: the widest launch any CLI or fleet site can make — a long home, the
/// relocated config dir, an OAuth-length token, a prompt under the macOS temp
/// dir, a deep managed path's MCP file, a supervisor's divert pins — types a
/// line under `MAX_PANE_COMMAND_BYTES` with no token in it. The same inputs
/// made a 1229-byte inline line.
#[test]
fn worst_case_cli_launch_line_is_short_and_carries_no_token() {
    use crate::core::session_profile::{SessionProfile, launch_env};
    let home = Path::new("/Users/an-operator-with-a-long-name");
    let mut managed = home.join("trusty-mpm-projects/an-operator-with-a-long-name");
    managed.push("an-uncomfortably-long-repository-name/dd0e2fb8-1111-2222-3333-444455556666");
    let config_dir = crate::core::trusty_tools_config::managed_claude_config_dir_at(home);
    let root = crate::core::paths::FrameworkPaths::under(home).crate_config_root();
    let mcp = crate::core::session_mcp_scope::session_mcp_path_at(&root, &managed);
    let prompt = PathBuf::from("/var/folders/qv/8k3p1x9n5cl7g2vy_0000gn/T")
        .join("trusty-mpm-system-prompt-dd0e2fb8-1111-2222-3333-444455556666.txt");
    let token = "sk-ant-oat01-".to_owned() + &"y".repeat(96);
    let env: Vec<(String, String)> = [
        (
            "TRUSTY_MEMORY_PALACE",
            "an-operator-with-a-long-name-an-uncomfortably-long-repository-name",
        ),
        ("TRUSTY_INDEX", "an-uncomfortably-long-repository-name"),
        ("TRUSTY_DIVERT_MIN_LINES", "4294967295"),
        ("TRUSTY_DIVERT_WORKER_MODEL", "claude-haiku-4-5-20251001"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .chain([launch_env(SessionProfile::Supervisor)])
    .collect();
    let launch = CliLaunch {
        model: Some("claude-opus-4-1-20250805"),
        prompt_file: Some(&prompt),
        config_dir: Some(&config_dir),
        oauth_token: Some(&token),
        mcp_env: &env,
        scoped_mcp: Some(&mcp),
        alternate_screen: false,
    };
    let spec = super::isolated_spec(&managed, &launch);
    assert!(env_words(&spec).contains(&format!("CLAUDE_CODE_OAUTH_TOKEN={token}")));

    let spec_dir = super::spec_dir(Some(home)).expect("a named home always resolves");
    let spec_path = spec_dir.join(format!("{}.json", uuid::Uuid::new_v4()));
    let wrapper = home.join(".cargo/bin/tm").display().to_string();
    let line = super::pane_line(&wrapper, &spec_path);
    // The pre-#8308 typed form, measured for the report only.
    let before = format!(
        "'{wrapper}' {} {}",
        crate::core::spawn_disclaim::PANE_DISCLAIM_SUBCOMMAND,
        crate::core::model_inject::build_claude_command_with_configured(
            launch.model,
            launch.prompt_file,
            launch.config_dir,
            launch.oauth_token,
            launch.mcp_env,
            launch.scoped_mcp,
            launch.alternate_screen,
        ),
    );
    eprintln!(
        "worst-case CLI launch line: {} bytes (inline form: {} bytes)",
        line.len(),
        before.len()
    );
    assert!(
        line.len() < crate::core::tmux::MAX_PANE_COMMAND_BYTES,
        "worst-case CLI launch line is {} bytes: {line}",
        line.len()
    );
    assert!(
        !line.contains(&token),
        "the token must not be typed: {line}"
    );
    assert!(!line.contains("sk-ant-"), "no token prefix in: {line}");
}

/// #8308: a named home keeps the spec under that home, never the process home.
#[test]
fn spec_dir_nests_under_the_named_home() {
    let dir = super::spec_dir(Some(Path::new("/h"))).expect("named home");
    assert_eq!(dir, Path::new("/h/.trusty-tools/trusty-mpm/launch-specs"));
}

/// #8308: delivery types one short line naming a spec that decodes to what was
/// sent, and presses Enter.
// Serial: the tmux spawn gate reads $HOME, which env tests repoint.
#[serial_test::serial]
#[test]
fn send_spec_launch_types_a_short_line_naming_a_readable_spec() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (bin, log) = fake_tmux(tmp.path(), 0);
    let spec_dir = tmp.path().join("specs");
    let spec = super::inplace_spec(tmp.path(), false);

    super::send_spec_launch_with(Some(&bin), "/w/tm", "tm-sess", &spec, &spec_dir)
        .expect("a short line is typed");

    let written = entries(&spec_dir);
    assert_eq!(written.len(), 1, "one spec: {written:?}");
    assert_eq!(LaunchSpec::verify(&written[0]).expect("readable"), spec);
    let argv = std::fs::read_to_string(&log).expect("tmux ran");
    let want = super::pane_line("/w/tm", &written[0]);
    assert!(argv.contains(&want), "want {want:?} in: {argv}");
    assert!(argv.contains("Enter"), "{argv}");
}

/// #8308: when tmux cannot type the line, nothing will consume the spec, so
/// the credential-bearing file is removed and the error carries tmux's stderr.
#[serial_test::serial]
#[test]
fn send_spec_launch_removes_the_spec_when_tmux_fails() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (bin, _) = fake_tmux(tmp.path(), 1);
    let spec_dir = tmp.path().join("specs");
    let spec = super::inplace_spec(tmp.path(), false);

    let err = super::send_spec_launch_with(Some(&bin), "/w/tm", "tm-sess", &spec, &spec_dir)
        .expect_err("a failing send-keys is an error");

    assert!(matches!(err, CliLaunchError::Send(_)), "{err:?}");
    assert!(err.to_string().contains("fake failure"), "{err}");
    assert!(entries(&spec_dir).is_empty(), "the spec must be removed");
}

/// #8308: a CLI spec has no managed session, so the child gets no
/// `TM_MANAGED_SESSION_ID` — the inline line it replaces exported none.
#[test]
fn a_cli_spec_exports_no_managed_session_id() {
    let cmd = super::inplace_spec(Path::new("/w"), false).to_command();
    let name = std::ffi::OsStr::new(crate::core::harness_root::MANAGED_SESSION_ID_ENV);
    assert!(
        cmd.get_envs().all(|(k, _)| k != name),
        "a CLI launch must not export the managed session id"
    );
}

/// #8308: a CLI spec has no launch id, so the shim writes no sentinel that
/// nothing reads and the reaper would later log as an abandoned launch.
#[test]
fn a_cli_spec_writes_no_started_sentinel() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = super::inplace_spec(tmp.path(), false)
        .write_in(tmp.path())
        .expect("write");
    LaunchSpec::mark_started(&path, "");
    assert_eq!(entries(tmp.path()), [path], "only the spec itself");
}
