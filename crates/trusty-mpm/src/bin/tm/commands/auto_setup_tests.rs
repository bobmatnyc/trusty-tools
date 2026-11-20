//! Tests for `auto_setup` (G0b). Every path is a temp dir; `cargo` is a fake
//! shell script, so no real `~/.cargo`, `~/.claude*` or `~/.trusty*` is read.

use super::*;
use std::os::unix::fs::PermissionsExt;

/// A temp layout: cargo bin dir, config dir, install cwd.
fn env_in(tmp: &Path, cargo: Option<PathBuf>) -> Env {
    let cargo_bin = tmp.join("cargo-home/bin");
    std::fs::create_dir_all(&cargo_bin).unwrap();
    let install_cwd = tmp.join("cwd");
    std::fs::create_dir_all(&install_cwd).unwrap();
    Env {
        cargo_home: tmp.join("cargo-home"),
        install_timeout: Duration::from_secs(60),
        cargo_bin,
        cargo,
        config_dir: tmp.join("claude-config"),
        install_cwd,
    }
}

/// A fake cargo: `install trusty-browser` creates the binary; any other crate
/// exits 101, as an unpublished crate does. It records each call.
fn fake_cargo(tmp: &Path) -> PathBuf {
    let path = tmp.join("fake-cargo");
    let bin = tmp.join("cargo-home/bin");
    let log = tmp.join("cargo.log");
    let script = format!(
        "#!/bin/sh\necho \"$@ cwd=$(pwd)\" >> '{log}'\n\
         if [ \"$2\" = trusty-browser ]; then printf '#!/bin/sh\\n' > '{bin}/trusty-browser-mcp'; \
         chmod +x '{bin}/trusty-browser-mcp'; exit 0; fi\nexit 101\n",
        log = log.display(),
        bin = bin.display()
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Put both binaries in place without cargo.
fn place_bins(env: &Env) {
    for s in SERVERS {
        std::fs::write(env.cargo_bin.join(s.bin), "#!/bin/sh\n").unwrap();
    }
}

#[test]
fn fake_cargo_install_then_register() {
    let tmp = tempfile::tempdir().unwrap();
    let cargo = fake_cargo(tmp.path());
    let env = env_in(tmp.path(), Some(cargo));
    let out = ensure(&env);
    assert_eq!(out[0].install, Install::Installed);
    assert_eq!(out[0].register, Register::Added);
    let got = mcp_config::get_server(&env.config_dir, "trusty-browser-mcp")
        .unwrap()
        .unwrap();
    let want = env.cargo_bin.join("trusty-browser-mcp");
    assert_eq!(got["command"], want.to_string_lossy().as_ref());
    assert!(want.is_absolute());
    let log = std::fs::read_to_string(tmp.path().join("cargo.log")).unwrap();
    assert!(
        log.contains("install trusty-browser --locked --root"),
        "{log}"
    );
    assert!(
        log.contains("cargo-home cwd="),
        "--root is CARGO_HOME: {log}"
    );
    assert!(log.contains("cwd="), "{log}");
}

#[test]
fn cargo_install_failure_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let env = env_in(tmp.path(), Some(fake_cargo(tmp.path())));
    let out = ensure(&env);
    // The computer crate is unpublished: skipped, not registered, browser unaffected.
    assert!(matches!(&out[1].install, Install::Missing(w) if w.contains("failed")));
    assert_eq!(out[1].register, Register::Skipped);
    assert_eq!(out[0].register, Register::Added);
    assert!(
        mcp_config::get_server(&env.config_dir, "trusty-computer-mcp")
            .unwrap()
            .is_none()
    );
}

#[test]
fn missing_cargo_is_reported_and_registration_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let env = env_in(tmp.path(), None);
    let out = ensure(&env);
    for o in &out {
        assert_eq!(o.install, Install::Missing("no `cargo` on PATH".into()));
        assert_eq!(o.register, Register::Skipped);
    }
}

#[test]
fn a_second_run_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let env = env_in(tmp.path(), None);
    place_bins(&env);
    assert!(ensure(&env).iter().all(|o| o.register == Register::Added));
    let file = env.config_dir.join(".claude.json");
    let before = std::fs::read(&file).unwrap();
    let second = ensure(&env);
    assert!(second.iter().all(|o| o.install == Install::Present));
    assert!(second.iter().all(|o| o.register == Register::Unchanged));
    assert_eq!(std::fs::read(&file).unwrap(), before);
}

#[test]
fn a_user_edited_entry_is_kept_and_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let env = env_in(tmp.path(), None);
    place_bins(&env);
    let mine = serde_json::json!({"type":"stdio","command":"/opt/mine","args":["--x"]});
    mcp_config::add_server(&env.config_dir, "trusty-browser-mcp", mine.clone()).unwrap();
    let out = ensure(&env);
    assert!(matches!(&out[0].register, Register::KeptExisting(h) if h.contains("/opt/mine")));
    let got = mcp_config::get_server(&env.config_dir, "trusty-browser-mcp").unwrap();
    assert_eq!(got, Some(mine));
    assert!(report_lines(&out)[0].contains("NOT changed"));
}

#[test]
fn an_unwritable_config_is_failed_not_added() {
    let tmp = tempfile::tempdir().unwrap();
    let env = env_in(tmp.path(), None);
    place_bins(&env);
    std::fs::create_dir_all(&env.config_dir).unwrap();
    std::fs::set_permissions(&env.config_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let out = ensure(&env);
    std::fs::set_permissions(&env.config_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    for o in &out {
        assert!(
            matches!(o.register, Register::Failed(_)),
            "{:?}",
            o.register
        );
    }
    assert!(!env.config_dir.join(".claude.json").exists());
}

#[test]
fn report_lines_never_call_a_failure_registered() {
    let out = vec![
        Outcome {
            bin: "trusty-browser-mcp",
            install: Install::Present,
            register: Register::Failed("denied".into()),
        },
        Outcome {
            bin: "trusty-computer-mcp",
            install: Install::Missing("no `cargo` on PATH".into()),
            register: Register::Skipped,
        },
    ];
    let lines = report_lines(&out);
    assert_eq!(lines.len(), 2, "no grant line for an unregistered server");
    assert!(lines[0].contains("registration FAILED: denied"));
    assert!(!lines[0].contains("; registered"));
    assert!(lines[1].contains("not registered"));
}

#[test]
fn registered_servers_print_their_grant() {
    let tmp = tempfile::tempdir().unwrap();
    let env = env_in(tmp.path(), None);
    place_bins(&env);
    let lines = report_lines(&ensure(&env));
    assert!(lines.iter().any(|l| l.contains("Connect")));
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Accessibility and Screen Recording"))
    );
}

#[test]
fn doctor_rows_report_each_fact() {
    let tmp = tempfile::tempdir().unwrap();
    let env = env_in(tmp.path(), None);
    let rows = doctor_rows(&env);
    assert!(rows.iter().all(|r| r.status == CheckStatus::Warn));
    assert!(rows[0].message.contains("installed=false registered=false"));
    place_bins(&env);
    ensure(&env);
    let rows = doctor_rows(&env);
    assert!(rows.iter().all(|r| r.status == CheckStatus::Ok), "{rows:?}");
    assert!(rows[1].message.contains("command-path-exists=true"));
}

#[test]
fn a_hung_cargo_install_is_killed_at_the_deadline() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("hung-cargo");
    // `exec` so the sleep is the child itself: killing it leaves no orphan.
    std::fs::write(&path, "#!/bin/sh\nexec sleep 20\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut env = env_in(tmp.path(), Some(path));
    env.install_timeout = Duration::from_secs(1);
    let start = Instant::now();
    let got = ensure_binary(&env, &SERVERS[0]);
    let took = start.elapsed();
    assert_eq!(
        got,
        Install::Missing("cargo install timed out after 1 s".into())
    );
    assert!(took < Duration::from_secs(10), "waited {took:?}");
}

/// Whether `pid` still names a process (a zombie counts as alive).
fn pid_alive(pid: i32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
}

#[test]
fn a_timeout_kills_the_whole_process_group() {
    let tmp = tempfile::tempdir().unwrap();
    let pidfile = tmp.path().join("grandchild.pid");
    let path = tmp.path().join("forking-cargo");
    // A background grandchild: `Child::kill` on the shell alone would orphan it.
    let script = format!(
        "#!/bin/sh\nsleep 30 &\necho $! > '{}'\nwait\n",
        pidfile.display()
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut env = env_in(tmp.path(), Some(path));
    env.install_timeout = Duration::from_secs(1);
    let got = ensure_binary(&env, &SERVERS[0]);
    assert_eq!(
        got,
        Install::Missing("cargo install timed out after 1 s".into())
    );
    let pid: i32 = std::fs::read_to_string(&pidfile)
        .expect("fake cargo wrote the grandchild pid")
        .trim()
        .parse()
        .unwrap();
    // The orphaned/killed grandchild is reaped by init asynchronously.
    let gone_by = Instant::now() + Duration::from_secs(5);
    while pid_alive(pid) && Instant::now() < gone_by {
        std::thread::sleep(Duration::from_millis(50));
    }
    let alive = pid_alive(pid);
    if alive {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    assert!(!alive, "grandchild {pid} survived the timeout");
}

#[test]
fn doctor_rows_leave_a_malformed_config_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let env = env_in(tmp.path(), None);
    std::fs::create_dir_all(&env.config_dir).unwrap();
    let file = env.config_dir.join(".claude.json");
    std::fs::write(&file, "{ not json, oauth: secret").unwrap();
    let rows = doctor_rows(&env);
    assert!(
        rows.iter().all(|r| r.status == CheckStatus::Unknown),
        "{rows:?}"
    );
    assert_eq!(std::fs::read(&file).unwrap(), b"{ not json, oauth: secret");
    let names: Vec<_> = std::fs::read_dir(&env.config_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 1, "no .corrupt sibling: {names:?}");
}
