//! `tm daemon --sandbox` refuses a secret-carrying environment (#9121).
//!
//! Why: a qa live-check started a "sandbox" daemon that inherited
//! `TELEGRAM_BOT_TOKEN` and polled the real Telegram bot. The unit tests in
//! `daemon_sandbox_tests.rs` pin each refusal arm; this drives the real binary
//! so the flag, the refusal, the exit status and the printed error are proven
//! together — including that the token's VALUE never reaches stdout or stderr.
//! What: each case runs `tm daemon --sandbox` from a cleared environment with
//! its own `$HOME` and data-dir override, plus fake secret variables. The
//! child must exit non-zero within the bound, name every variable, and print
//! no value. Every case is otherwise isolated, so a regression that let the
//! daemon start would start it in a scratch home with fake credentials; the
//! bound kills it.
//! Test: this file IS the test module.

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::common;

/// A fake bot token. Never a real credential.
const FAKE_TOKEN: &str = "9121000000:fake-telegram-token-never-real";

/// A fake API key. Never a real credential.
const FAKE_KEY: &str = "sk-fake-9121-api-key-never-real";

/// How long a refusal may take before the child is killed and the case fails.
const BOUND: Duration = Duration::from_secs(60);

/// A `tm daemon --sandbox` whose child sees only `vars` plus the isolation
/// variables.
fn sandbox_command(home: &Path, vars: &[(&str, &str)]) -> Command {
    let mut cmd = common::tm_command_in(home);
    cmd.env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("TRUSTY_DATA_DIR_OVERRIDE", home.join("data"))
        // #9178: required, as `scripts/sandbox_daemon.sh` passes it.
        .env("TRUSTY_SANDBOX", "1")
        .args(["daemon", "--sandbox"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in vars {
        cmd.env(name, value);
    }
    cmd
}

/// Wait for `child` within [`BOUND`]; kill it and fail when it outlives it.
///
/// Returns `(exit success, stdout, stderr)`.
fn wait_bounded(mut child: Child) -> (bool, String, String) {
    // Drain both pipes on their own threads so a chatty child cannot block on
    // a full pipe while this thread polls its exit.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_string(&mut text);
            }
            text
        })
    };
    let stdout = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let stderr = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            break status;
        }
        if started.elapsed() > BOUND {
            let _ = child.kill();
            let _ = child.wait();
            panic!("`tm daemon --sandbox` did not refuse within {BOUND:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let stdout = stdout.join().expect("stdout reader");
    let stderr = stderr.join().expect("stderr reader");
    (status.success(), stdout, stderr)
}

/// Why (#9121): the allowlist is closed, so a variable the process or the OS
/// adds on its own would make every sandbox refuse. With only allowlisted
/// names set and the data-dir override withheld, the binary must refuse for
/// that reason alone — proving the real process environment passes the
/// allowlist without starting a daemon.
/// Test: this test.
#[test]
fn an_allowlisted_environment_passes_the_allowlist_check() {
    let home = tempfile::tempdir().expect("scratch home");
    let mut cmd = sandbox_command(home.path(), &[("LANG", "C"), ("RUST_LOG", "warn")]);
    cmd.env_remove("TRUSTY_DATA_DIR_OVERRIDE");

    let (success, _stdout, stderr) = wait_bounded(cmd.spawn().expect("spawn"));

    assert!(!success, "the sandbox started without a data-dir override");
    assert!(
        stderr.contains("TRUSTY_DATA_DIR_OVERRIDE is not set"),
        "stderr: {stderr}"
    );
    assert!(
        !stderr.contains("outside the sandbox allowlist"),
        "an allowlisted environment was refused: {stderr}"
    );
}

/// Why (#9121): the incident path, end to end. A sandbox with a bot token, an
/// API key, or both must exit non-zero, name each variable, and print neither
/// value. On `origin/main` the flag does not exist, so clap's error names no
/// variable and this fails.
/// Test: this test.
#[test]
fn sandbox_refuses_secret_env_and_never_prints_a_value() {
    let cases: &[&[(&str, &str)]] = &[
        &[("TELEGRAM_BOT_TOKEN", FAKE_TOKEN)],
        &[("OPENAI_API_KEY", FAKE_KEY)],
        // #9121: closed allowlist — a credential with no secret-shaped suffix.
        &[("GITHUB_PAT", FAKE_KEY)],
        &[
            ("TELEGRAM_BOT_TOKEN", FAKE_TOKEN),
            ("OPENAI_API_KEY", FAKE_KEY),
        ],
    ];
    for vars in cases {
        let home = tempfile::tempdir().expect("scratch home");
        let child = sandbox_command(home.path(), vars)
            .spawn()
            .expect("spawn tm daemon --sandbox");

        let (success, stdout, stderr) = wait_bounded(child);

        assert!(!success, "the sandbox started with {vars:?}\n{stderr}");
        assert!(stderr.contains("#9121"), "stderr: {stderr}");
        for (name, value) in *vars {
            assert!(stderr.contains(name), "{name} not named\nstderr: {stderr}");
            assert!(!stdout.contains(value), "{name}'s value on stdout");
            assert!(!stderr.contains(value), "{name}'s value on stderr");
        }
    }
}

/// Why (#9178): without `TRUSTY_SANDBOX=1` trusty-common reads `.env.local`.
/// An otherwise isolated sandbox with the flag absent, or set to anything but
/// `1`, must exit non-zero, name the variable, and print no value.
/// Test: this test.
#[test]
fn sandbox_refuses_unless_trusty_sandbox_is_exactly_one() {
    const WRONG: &str = "fake-9178-flag-value";
    for value in [None, Some(WRONG)] {
        let home = tempfile::tempdir().expect("scratch home");
        let mut cmd = sandbox_command(home.path(), &[]);
        match value {
            None => cmd.env_remove("TRUSTY_SANDBOX"),
            Some(v) => cmd.env("TRUSTY_SANDBOX", v),
        };

        let (success, stdout, stderr) = wait_bounded(cmd.spawn().expect("spawn"));

        assert!(
            !success,
            "the sandbox started with flag {value:?}\n{stderr}"
        );
        assert!(
            stderr.contains("TRUSTY_SANDBOX is not set to exactly 1"),
            "flag {value:?}\nstderr: {stderr}"
        );
        assert!(!stdout.contains(WRONG) && !stderr.contains(WRONG));
    }
}

/// A fake provider key for the #10721 cases. Never a real credential.
const FAKE_ADMITTED: &str = "sk-ant-fake-10721-never-real";

/// Run `tm daemon --sandbox` plus `extra` args, with `vars`; the refusal.
fn refused_with(
    vars: &[(&str, &str)],
    extra: &[&str],
    tweak: impl FnOnce(&mut Command),
) -> (String, String) {
    let home = tempfile::tempdir().expect("scratch home");
    let mut cmd = sandbox_command(home.path(), vars);
    cmd.args(extra);
    tweak(&mut cmd);
    let (success, stdout, stderr) = wait_bounded(cmd.spawn().expect("spawn"));
    assert!(!success, "the sandbox started: {extra:?}\n{stderr}");
    for (_, value) in vars {
        assert!(
            !stdout.contains(value) && !stderr.contains(value),
            "a value printed"
        );
    }
    (stdout, stderr)
}

/// Why (#10721, fail closed): `--sandbox-credential` naming a variable off the
/// admission list refuses and names it; the variable is not exempted from the
/// allowlist either. On `origin/main` the flag does not exist, so clap's error
/// lacks the admission refusal and this fails.
/// Test: this test.
#[test]
fn sandbox_credential_off_the_list_refuses() {
    let (_, stderr) = refused_with(
        &[("TELEGRAM_BOT_TOKEN", FAKE_TOKEN)],
        &["--sandbox-credential", "TELEGRAM_BOT_TOKEN"],
        |_| {},
    );
    assert!(
        stderr.contains("TELEGRAM_BOT_TOKEN is not a credential a sandbox may admit"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("outside the sandbox allowlist"),
        "stderr: {stderr}"
    );
}

/// Why (#10721, fail closed): `--sandbox-credential NAME` with `NAME` unset
/// refuses, naming the variable; no other tier is consulted.
/// Test: this test.
#[test]
fn sandbox_credential_without_the_variable_refuses() {
    let (_, stderr) = refused_with(&[], &["--sandbox-credential", "ANTHROPIC_API_KEY"], |_| {});
    assert!(
        stderr.contains("ANTHROPIC_API_KEY is not set in the environment"),
        "stderr: {stderr}"
    );
}

/// Why (#10721): the admitted name is the one exemption from the allowlist.
/// With the key set and the data-dir override withheld, the binary refuses for
/// that reason alone — the key passed the allowlist — and prints no value.
/// Without the flag the same key still refuses: the default stays closed.
/// Test: this test.
#[test]
fn sandbox_credential_admits_only_its_own_name() {
    let vars = [("ANTHROPIC_API_KEY", FAKE_ADMITTED)];
    let (_, stderr) = refused_with(&vars, &["--sandbox-credential", "ANTHROPIC_API_KEY"], |c| {
        c.env_remove("TRUSTY_DATA_DIR_OVERRIDE");
    });
    assert!(
        stderr.contains("TRUSTY_DATA_DIR_OVERRIDE is not set"),
        "stderr: {stderr}"
    );
    assert!(
        !stderr.contains("outside the sandbox allowlist"),
        "the admitted name was refused: {stderr}"
    );

    let (_, stderr) = refused_with(&vars, &[], |_| {});
    assert!(
        stderr.contains("outside the sandbox allowlist") && stderr.contains("ANTHROPIC_API_KEY"),
        "a key without the flag was not refused: {stderr}"
    );

    // A second provider key is still refused by name.
    let both = [
        ("ANTHROPIC_API_KEY", FAKE_ADMITTED),
        ("OPENROUTER_API_KEY", FAKE_KEY),
    ];
    let (_, stderr) = refused_with(
        &both,
        &["--sandbox-credential", "ANTHROPIC_API_KEY"],
        |_| {},
    );
    assert!(
        stderr.contains("outside the sandbox allowlist") && stderr.contains("OPENROUTER_API_KEY"),
        "stderr: {stderr}"
    );
}
