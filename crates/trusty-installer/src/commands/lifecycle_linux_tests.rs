//! Tests for the Linux `tctl restart` path (#11086).
//!
//! Why: `restart` on Linux now spawns `<install dir>/<binary> restart` and
//! `--version` and reads the health probe; each acceptance criterion of #11086
//! needs that path driven end to end without a real daemon.
//! What: stub `#!/bin/sh` binaries in a temp install dir log every `restart`
//! they receive, [`FakeRestartHost`] supplies that dir and a canned health
//! answer, and [`LinuxEnv`] reports the named members as present. A test that
//! needs a PATH decoy prepends a temp dir under `ENV_TEST_LOCK`, so even a
//! regression to a PATH lookup runs a stub, never a binary on the host.
//! Test: this file is the test.

use super::tests::FakeRestartHost;
use super::*;
use crate::commands::probe_http::ProbeOutcome;
use crate::commands::test_support::{write_exec_script, ENV_TEST_LOCK};
use std::cell::RefCell;
use std::path::PathBuf;
use trusty_common::launchd_labels::LabelEviction;

/// A host where the named members are present and nothing launchd exists.
///
/// What: a bare name resolves when it is listed; an absolute path resolves when
/// the file exists, as `which` does for the real host. Every launchd seam
/// refuses, so a test that wrongly reaches launchd fails instead of acting.
struct LinuxEnv {
    members: Vec<&'static str>,
}

impl ServiceEnv for LinuxEnv {
    fn plist_present(&self, _binary: &str) -> bool {
        false
    }
    fn run_service_install(&self, binary: &str, _exe: Option<&Path>) -> anyhow::Result<()> {
        anyhow::bail!("linux test: no service install ({binary})")
    }
    fn is_loaded(&self, _binary: &str) -> bool {
        false
    }
    fn bootstrap_fallback(&self, binary: &str) -> anyhow::Result<()> {
        anyhow::bail!("linux test: no bootstrap ({binary})")
    }
    fn port_guard(&self, binary: &str) -> Result<(), String> {
        Err(format!("linux test: port guard refuses {binary}"))
    }
    fn evict_retired(&self, _binary: &str) -> Vec<LabelEviction> {
        Vec::new()
    }
    fn binary_resolves(&self, binary: &str) -> bool {
        let p = Path::new(binary);
        if p.is_absolute() {
            p.is_file()
        } else {
            self.members.contains(&binary)
        }
    }
    fn probe_plist(&self, _binary: &str) -> Result<bool, String> {
        Ok(false)
    }
    fn probe_loaded(&self, _binary: &str) -> Result<bool, String> {
        Ok(false)
    }
}

/// A temp root with an install dir `bin/` and the shared restart log.
struct Fixture {
    tmp: tempfile::TempDir,
    bin: PathBuf,
    log: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).expect("create bin");
    let log = tmp.path().join("restart.log");
    Fixture { tmp, bin, log }
}

/// Write a stub that prints `version_line` for `--version`, appends
/// `<$0> restart` to `log` and exits `restart_exit` for `restart`, and rejects
/// any other verb the way clap does.
fn stub(dir: &Path, name: &str, version_line: &str, restart_exit: i32, log: &Path) {
    let body = format!(
        "#!/bin/sh\ncase \"$1\" in\n  --version) echo '{version_line}' ;;\n  \
         restart) echo \"$0 restart\" >> '{log}'; \
         [ {restart_exit} -eq 0 ] || echo \"error: unrecognized subcommand 'restart'\" >&2; \
         exit {restart_exit} ;;\n  \
         *) echo \"error: unrecognized subcommand '$1'\" >&2; exit 2 ;;\nesac\n",
        log = log.display()
    );
    write_exec_script(&dir.join(name), &body);
}

fn serving(version: &str) -> ProbeOutcome {
    ProbeOutcome::Serving {
        status: "ok".to_owned(),
        version: Some(version.to_owned()),
    }
}

fn host(f: &Fixture, health: ProbeOutcome) -> FakeRestartHost {
    FakeRestartHost {
        dir: f.bin.clone(),
        health,
        probes: RefCell::new(Vec::new()),
    }
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

fn log_lines(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn restart(
    env: &LinuxEnv,
    host: &FakeRestartHost,
    members: &[&str],
) -> (i32, Vec<LifecycleOutcome>) {
    run_lifecycle_on(env, host, Verb::Restart, &names(members), true, false)
}

fn outcome<'a>(outcomes: &'a [LifecycleOutcome], member: &str) -> &'a LifecycleOutcome {
    outcomes
        .iter()
        .find(|o| o.member == member)
        .unwrap_or_else(|| panic!("no outcome for {member}: {outcomes:?}"))
}

/// Prepends a decoy dir to `PATH` and restores the old value on drop, so a
/// failed assertion cannot leave the decoy on `PATH`.
struct DecoyPath {
    prev: std::ffi::OsString,
}

impl DecoyPath {
    fn prepend(dir: &Path) -> Self {
        let prev = std::env::var_os("PATH").unwrap_or_default();
        let mut parts = vec![dir.to_owned()];
        parts.extend(std::env::split_paths(&prev));
        std::env::set_var("PATH", std::env::join_paths(parts).expect("join PATH"));
        Self { prev }
    }
}

impl Drop for DecoyPath {
    fn drop(&mut self) {
        std::env::set_var("PATH", &self.prev);
    }
}

/// REGRESSION (#11086 AC1): `tctl restart` runs the absolute
/// `<install dir>/<binary>`, never the copy first on `PATH`.
///
/// Why: trusty-mpm restarted through a PATH lookup, so a host with two copies
/// bounced a different binary from the one `tctl upgrade` placed.
/// What: decoys named `trusty-search` and `trusty-mpm` sit first on `PATH`.
/// Both members restart; the log holds only the two install-dir paths and the
/// decoys never write theirs.
/// Test: this is the test.
#[test]
fn linux_restart_runs_the_install_dir_binary_not_the_path_decoy() {
    let _lock = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let f = fixture();
    stub(&f.bin, "trusty-search", "trusty-search 0.63.1", 0, &f.log);
    stub(&f.bin, "tm", "trusty-mpm 0.63.1", 0, &f.log);
    let decoy = f.tmp.path().join("decoy");
    std::fs::create_dir_all(&decoy).expect("create decoy dir");
    let decoy_log = f.tmp.path().join("decoy.log");
    stub(
        &decoy,
        "trusty-search",
        "trusty-search 0.63.1",
        0,
        &decoy_log,
    );
    stub(&decoy, "trusty-mpm", "trusty-mpm 0.63.1", 0, &decoy_log);
    stub(&decoy, "tm", "trusty-mpm 0.63.1", 0, &decoy_log);
    let env = LinuxEnv {
        members: vec!["trusty-search", "trusty-mpm"],
    };
    let host = host(&f, serving("0.63.1"));

    let (code, outcomes) = {
        let _path = DecoyPath::prepend(&decoy);
        restart(&env, &host, &["trusty-search", "trusty-mpm"])
    };

    assert_eq!(code, 0, "{outcomes:?}");
    assert_eq!(
        log_lines(&f.log),
        vec![
            format!("{} restart", f.bin.join("tm").display()),
            format!("{} restart", f.bin.join("trusty-search").display()),
        ]
    );
    assert!(
        log_lines(&decoy_log).is_empty(),
        "a decoy ran: {decoy_log:?}"
    );
}

/// REGRESSION (#11086 AC2): the installed version is the install-dir binary's
/// `--version`, never `~/.cargo/.crates.toml`.
///
/// Why: `.crates.toml` recorded trusty-mpm 1.6.3 on a host running 1.12.2.
/// What: a `.crates.toml` beside the install dir records trusty-memory 0.20.0;
/// the binary reports 0.36.7 and so does the health probe. The member is ok,
/// and its line names 0.36.7 and never 0.20.0.
/// Test: this is the test.
#[test]
fn installed_version_comes_from_the_binary_not_crates_toml() {
    let f = fixture();
    std::fs::write(
        f.tmp.path().join(".crates.toml"),
        "[v1]\n\"trusty-memory 0.20.0 (registry+https://github.com/rust-lang/crates.io-index)\" \
         = [\"trusty-memory\"]\n",
    )
    .expect("write .crates.toml");
    stub(&f.bin, "trusty-memory", "trusty-memory 0.36.7", 0, &f.log);
    let env = LinuxEnv {
        members: vec!["trusty-memory"],
    };
    let (code, outcomes) = restart(&env, &host(&f, serving("0.36.7")), &["trusty-memory"]);
    let memory = outcome(&outcomes, "trusty-memory");
    assert_eq!(code, 0, "{outcomes:?}");
    assert!(
        memory.ok && memory.detail.contains("installed 0.36.7"),
        "{memory:?}"
    );
    assert!(!memory.detail.contains("0.20.0"), "{memory:?}");
}

/// REGRESSION (#11086 AC3): trusty-mpm's installed version comes from
/// `<install dir>/tm --version`, not the `trusty-mpm` shim.
///
/// Why: the member binary is a shim that execs `tm`; its own version can lag.
/// What: the shim reports 1.6.3 and `tm` reports 1.14.0, which the probe also
/// reports. The member is ok with `installed 1.14.0`, only `tm` received
/// `restart`, and the probe asked about `trusty-mpm`. A decoy shim first on
/// `PATH` keeps a regression to the PATH lookup off the host's own binary.
/// Test: this is the test.
#[test]
fn mpm_installed_version_comes_from_tm_not_the_shim() {
    let _lock = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let f = fixture();
    stub(&f.bin, "trusty-mpm", "trusty-mpm 1.6.3", 0, &f.log);
    stub(&f.bin, "tm", "trusty-mpm 1.14.0", 0, &f.log);
    let decoy = f.tmp.path().join("decoy");
    std::fs::create_dir_all(&decoy).expect("create decoy dir");
    stub(&decoy, "trusty-mpm", "trusty-mpm 1.6.3", 0, &f.log);
    let env = LinuxEnv {
        members: vec!["trusty-mpm"],
    };
    let host = host(&f, serving("1.14.0"));

    let (code, outcomes) = {
        let _path = DecoyPath::prepend(&decoy);
        restart(&env, &host, &["trusty-mpm"])
    };

    let mpm = outcome(&outcomes, "trusty-mpm");
    assert_eq!(code, 0, "{outcomes:?}");
    assert!(mpm.ok && mpm.detail.contains("installed 1.14.0"), "{mpm:?}");
    assert_eq!(
        log_lines(&f.log),
        vec![format!("{} restart", f.bin.join("tm").display())]
    );
    assert_eq!(*host.probes.borrow(), vec!["trusty-mpm".to_owned()]);
}

/// REGRESSION (#11086 AC4): a `restart` that exits 0 while the daemon still
/// runs another version is FAILED, names both versions, and exits nonzero.
///
/// Why: trusty-memory 0.28.0 kept running after 0.36.7 was installed, and no
/// command said so.
/// What: the stub restarts with exit 0 and reports 0.36.7; the probe reports
/// 0.28.0.
/// Test: this is the test.
#[test]
fn running_version_mismatch_is_failed_with_both_versions() {
    let f = fixture();
    stub(&f.bin, "trusty-memory", "trusty-memory 0.36.7", 0, &f.log);
    let env = LinuxEnv {
        members: vec!["trusty-memory"],
    };
    let (code, outcomes) = restart(&env, &host(&f, serving("0.28.0")), &["trusty-memory"]);
    let memory = outcome(&outcomes, "trusty-memory");
    assert_ne!(code, 0, "{outcomes:?}");
    assert!(!memory.ok, "{memory:?}");
    assert!(
        memory.detail.contains("0.28.0") && memory.detail.contains("0.36.7"),
        "{memory:?}"
    );
}

/// REGRESSION (#11086 AC5): a `restart` that exits 0 with a refused health
/// probe afterwards is FAILED.
///
/// Why: exit 0 from the verb says nothing about whether a daemon now serves.
/// What: the stub restarts with exit 0; the probe is refused.
/// Test: this is the test.
#[test]
fn refused_health_probe_after_restart_is_failed() {
    let f = fixture();
    stub(&f.bin, "trusty-search", "trusty-search 0.63.1", 0, &f.log);
    let env = LinuxEnv {
        members: vec!["trusty-search"],
    };
    let (code, outcomes) = restart(&env, &host(&f, ProbeOutcome::Refused), &["trusty-search"]);
    let search = outcome(&outcomes, "trusty-search");
    assert_eq!(code, 2, "{outcomes:?}");
    assert!(
        !search.ok && search.detail.contains("refused"),
        "{search:?}"
    );
    assert_eq!(log_lines(&f.log).len(), 1, "the restart ran once");
}

/// REGRESSION (#11086 AC6): on Linux no lifecycle line says "only supported on
/// macOS"; a member without Linux verbs gets the manual note.
///
/// Why: the launchd stub's platform error told the operator nothing to do.
/// What: `restart` of trusty-events (no Linux verbs, D3) and `start`/`stop` of
/// trusty-search (no routing change for those verbs) each carry the manual
/// note for their verb; nothing is spawned.
/// Test: this is the test.
#[test]
fn linux_lifecycle_output_never_says_macos_only() {
    let f = fixture();
    let env = LinuxEnv {
        members: vec!["trusty-events", "trusty-search"],
    };
    let host = host(&f, serving("0.0.0"));
    let cases = [
        (Verb::Restart, "trusty-events"),
        (Verb::Start, "trusty-search"),
        (Verb::Stop, "trusty-search"),
    ];
    for (verb, member) in cases {
        let (_, outcomes) = run_lifecycle_on(&env, &host, verb, &names(&[member]), true, false);
        let o = outcome(&outcomes, member);
        assert!(!o.detail.contains("only supported on macOS"), "{o:?}");
        assert_eq!(
            o.detail,
            manual_lifecycle_note(verb.name(), member),
            "{o:?}"
        );
    }
    // A bare restart over every daemon member, all present but none in the
    // empty install dir: each line fails, and none names the platform.
    let every = LinuxEnv {
        members: vec![
            "trusty-search",
            "trusty-memory",
            "trusty-events",
            "trusty-console",
            "trusty-mpm",
        ],
    };
    let (_, outcomes) = restart(&every, &host, &[]);
    assert_eq!(outcomes.len(), 5, "{outcomes:?}");
    for o in &outcomes {
        assert!(!o.detail.contains("only supported on macOS"), "{o:?}");
    }
    assert!(log_lines(&f.log).is_empty());
    assert!(host.probes.borrow().is_empty());
}

/// REGRESSION (#11086 AC7, D4): a member whose binary rejects `restart` is
/// FAILED, and the members after it are still restarted.
///
/// Why: a binary older than the Linux verbs rejects `restart`; that must not
/// stop the rest of the stack from restarting.
/// What: restart order is trusty-console, then trusty-memory. Console's stub
/// rejects `restart` the way clap does; memory's accepts and its versions
/// agree. Console is FAILED with no version read, memory is ok, exit 2.
/// Test: this is the test.
#[test]
fn rejected_restart_is_failed_and_later_members_still_restart() {
    let f = fixture();
    stub(&f.bin, "trusty-console", "trusty-console 0.12.0", 2, &f.log);
    stub(&f.bin, "trusty-memory", "trusty-memory 0.36.7", 0, &f.log);
    let env = LinuxEnv {
        members: vec!["trusty-console", "trusty-memory"],
    };
    let host = host(&f, serving("0.36.7"));
    let (code, outcomes) = restart(&env, &host, &["trusty-memory", "trusty-console"]);
    assert_eq!(code, 2, "{outcomes:?}");
    let console = outcome(&outcomes, "trusty-console");
    assert!(
        !console.ok && console.detail.contains("restart` exited with"),
        "{console:?}"
    );
    assert!(outcome(&outcomes, "trusty-memory").ok, "{outcomes:?}");
    assert_eq!(
        log_lines(&f.log),
        vec![
            format!("{} restart", f.bin.join("trusty-console").display()),
            format!("{} restart", f.bin.join("trusty-memory").display()),
        ]
    );
    assert_eq!(*host.probes.borrow(), vec!["trusty-memory".to_owned()]);
}

/// The version parse reads each daemon's real `--version` line (#11086 risk).
///
/// Why: a wrong parse reports FAILED on a correct restart.
/// What: each line is what the installed binary printed on 2026-11-21.
/// Test: this is the test.
#[test]
fn installed_version_parses_each_daemons_real_version_line() {
    let f = fixture();
    let cases = [
        ("tm", "trusty-mpm 1.14.0", "1.14.0"),
        ("trusty-console", "trusty-console 0.13.0", "0.13.0"),
        ("trusty-memory", "trusty-memory 0.36.7", "0.36.7"),
        ("trusty-search", "trusty-search 0.63.1", "0.63.1"),
    ];
    for (name, line, want) in cases {
        stub(&f.bin, name, line, 0, &f.log);
        let got = linux::installed_version(&f.bin.join(name)).expect("version");
        assert_eq!(got, want, "{name}");
    }
}

/// #11086 review: every probe answer other than a matching `Serving` version
/// is FAILED, including the catch-all arm.
///
/// Why: an `Err` → `Ok` slip in the catch-all would report `ok` for a daemon
/// whose version nobody read.
/// What: a reply with no version, a probe that could not run, and a binary
/// the probe could not find each give `Err` naming the installed version.
/// Test: this is the test.
#[test]
fn verdict_fails_closed_on_every_other_probe_answer() {
    let cases = [
        ProbeOutcome::Serving {
            status: "ok".to_owned(),
            version: None,
        },
        ProbeOutcome::ProbeFailed {
            detail: "no runtime".to_owned(),
        },
        ProbeOutcome::NotInstalled,
    ];
    for health in cases {
        let err = linux::verdict("x restart ok", "0.36.7", &health)
            .expect_err(&format!("{health:?} must be FAILED"));
        assert!(err.contains("installed is 0.36.7"), "{err}");
    }
}

/// Write a trusty-memory stub that accepts `restart` and answers `--version`
/// with `version_arm` (shell text).
fn stub_version_arm(f: &Fixture, version_arm: &str) {
    let body = format!(
        "#!/bin/sh\ncase \"$1\" in\n  --version) {version_arm} ;;\n  \
         restart) echo \"$0 restart\" >> '{log}' ;;\n  *) exit 2 ;;\nesac\n",
        log = f.log.display()
    );
    write_exec_script(&f.bin.join("trusty-memory"), &body);
}

/// #11086 review: an installed version tctl cannot read is FAILED, before any
/// health probe.
///
/// Why: swallowing the version-read error would compare the running version
/// against nothing, or skip the check.
/// What: `--version` exits 1, then prints `no version here`. Each time the
/// restart runs, the member is FAILED naming `--version`, tctl exits 2, and the
/// probe is never asked.
/// Test: this is the test.
#[test]
fn unreadable_installed_version_is_failed_before_the_probe() {
    for arm in ["exit 1", "echo 'no version here'"] {
        let f = fixture();
        stub_version_arm(&f, arm);
        let env = LinuxEnv {
            members: vec!["trusty-memory"],
        };
        let host = host(&f, serving("0.36.7"));
        let (code, outcomes) = restart(&env, &host, &["trusty-memory"]);
        let memory = outcome(&outcomes, "trusty-memory");
        assert_eq!(code, 2, "{arm}: {outcomes:?}");
        assert!(
            !memory.ok && memory.detail.contains("--version"),
            "{arm}: {memory:?}"
        );
        assert_eq!(log_lines(&f.log).len(), 1, "{arm}: the restart ran once");
        assert!(host.probes.borrow().is_empty(), "{arm}: probe was asked");
    }
}

/// #11086 review: trusty-mpm with no `tm` in the install dir is FAILED, and
/// neither the shim nor a `PATH` copy is restarted.
///
/// Why: dropping the absolute-path hand-off would send the `OwnVerb` arm to a
/// `PATH` lookup of `trusty-mpm`.
/// What: the install dir holds only the shim; a decoy shim sits first on
/// `PATH`. Both log to the restart log, which stays empty; exit 2, no probe.
/// Test: this is the test.
#[test]
fn mpm_without_tm_in_the_install_dir_is_failed() {
    let _lock = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let f = fixture();
    stub(&f.bin, "trusty-mpm", "trusty-mpm 1.14.0", 0, &f.log);
    let decoy = f.tmp.path().join("decoy");
    std::fs::create_dir_all(&decoy).expect("create decoy dir");
    stub(&decoy, "trusty-mpm", "trusty-mpm 1.14.0", 0, &f.log);
    let env = LinuxEnv {
        members: vec!["trusty-mpm"],
    };
    let host = host(&f, serving("1.14.0"));

    let (code, outcomes) = {
        let _path = DecoyPath::prepend(&decoy);
        restart(&env, &host, &["trusty-mpm"])
    };

    let mpm = outcome(&outcomes, "trusty-mpm");
    assert_eq!(code, 2, "{outcomes:?}");
    assert!(!mpm.ok && mpm.detail.contains("/tm"), "{mpm:?}");
    assert!(log_lines(&f.log).is_empty(), "something restarted");
    assert!(host.probes.borrow().is_empty());
}

/// #11086 review: an install-dir binary the restart cannot spawn is FAILED,
/// naming the spawn, before any probe.
///
/// Why: mapping the spawn error to ok would hand the verdict a restart that
/// never happened.
/// What: `trusty-search` in the install dir is mode 0644; exit 2, the detail
/// names the failed spawn, no probe.
/// Test: this is the test.
#[test]
fn unspawnable_install_binary_is_failed() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let exe = f.bin.join("trusty-search");
    std::fs::write(&exe, "#!/bin/sh\nexit 0\n").expect("write binary");
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o644)).expect("chmod 0644");
    let env = LinuxEnv {
        members: vec!["trusty-search"],
    };
    let host = host(&f, serving("0.63.1"));
    let (code, outcomes) = restart(&env, &host, &["trusty-search"]);
    let search = outcome(&outcomes, "trusty-search");
    assert_eq!(code, 2, "{outcomes:?}");
    // The `restart` spawn itself, not the later `--version` spawn, which also
    // fails on this file.
    let spawn = format!("failed to spawn `{} restart`", exe.display());
    assert!(!search.ok && search.detail.contains(&spawn), "{search:?}");
    assert!(host.probes.borrow().is_empty());
}
