//! The real runner: argv without a shell, the kill at the deadline, a missing
//! program (MB1595). These start real, harmless processes.

use std::path::Path;
use std::time::{Duration, Instant};

use trusty_architect::sched::{JobRunner, RunSpec, SystemRunner};

fn spec(dir: &Path, argv: &[&str], timeout: Duration) -> RunSpec {
    RunSpec {
        argv: argv.iter().map(|s| (*s).to_owned()).collect(),
        cwd: dir.to_owned(),
        env: vec![
            ("PATH".to_owned(), "/bin:/usr/bin".to_owned()),
            ("KEEP".to_owned(), "yes".to_owned()),
        ],
        stdin: None,
        stdout: dir.join("logs/out.log"),
        stderr: dir.join("logs/out.log"),
        timeout,
    }
}

#[test]
fn system_runner_runs_argv_without_a_shell() {
    let dir = tempfile::tempdir().unwrap();
    // `;` and `$HOME` are plain text to a program, not shell syntax.
    let s = spec(
        dir.path(),
        &["echo", "a;b", "$HOME"],
        Duration::from_secs(10),
    );
    let r = SystemRunner.run(&s);
    assert_eq!((r.exit, r.timed_out, r.error), (Some(0), false, None));
    assert_eq!(std::fs::read_to_string(&s.stdout).unwrap(), "a;b $HOME\n");
    // Only the given environment is inherited, and cwd is honoured.
    let mut env = spec(dir.path(), &["env"], Duration::from_secs(10));
    env.stdout = dir.path().join("env.log");
    SystemRunner.run(&env);
    let text = std::fs::read_to_string(&env.stdout).unwrap();
    assert!(
        text.contains("KEEP=yes") && !text.contains("HOME="),
        "{text}"
    );
    let mut pwd = spec(dir.path(), &["pwd"], Duration::from_secs(10));
    pwd.stdout = dir.path().join("pwd.log");
    SystemRunner.run(&pwd);
    let printed = std::fs::read_to_string(&pwd.stdout).unwrap();
    assert_eq!(
        Path::new(printed.trim()).canonicalize().unwrap(),
        dir.path().canonicalize().unwrap()
    );
}

#[test]
fn system_runner_reports_exit_codes_and_a_missing_program() {
    let dir = tempfile::tempdir().unwrap();
    let r = SystemRunner.run(&spec(
        dir.path(),
        &["sh", "-c", "exit 3"],
        Duration::from_secs(10),
    ));
    assert_eq!(r.exit, Some(3));
    let r = SystemRunner.run(&spec(
        dir.path(),
        &["no-such-program-mb1595"],
        Duration::from_secs(10),
    ));
    assert_eq!(r.exit, None);
    assert!(
        r.error
            .as_deref()
            .is_some_and(|e| e.contains("no-such-program-mb1595")),
        "{r:?}"
    );
    let r = SystemRunner.run(&spec(dir.path(), &[], Duration::from_secs(10)));
    assert!(r.error.is_some());
}

#[test]
fn system_runner_kills_the_group_at_the_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("child.pid");
    // The shell starts a grandchild and waits on it; both must die.
    let script = format!("sleep 60 & echo $! > {}; wait", pidfile.display());
    let started = Instant::now();
    let r = SystemRunner.run(&spec(
        dir.path(),
        &["sh", "-c", &script],
        Duration::from_secs(1),
    ));
    assert!(r.timed_out, "{r:?}");
    assert_eq!(r.exit, None);
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "the kill is prompt"
    );
    let pid = std::fs::read_to_string(&pidfile).unwrap().trim().to_owned();
    // `kill -0` fails once the grandchild is gone (it may need a moment).
    let alive = || {
        std::process::Command::new("kill")
            .args(["-0", &pid])
            .output()
            .unwrap()
            .status
            .success()
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive(), "the grandchild survived the group kill");
}
