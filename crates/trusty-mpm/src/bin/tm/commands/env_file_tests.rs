//! Unit tests for `tm env set|keys` (`env_file.rs`, #8939).

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use clap::Parser;

use super::*;
use crate::cli::{Cli, Command};
use crate::commands::pm_guard_trust_anchor::tests::{
    Fixture, allowlist, architect_env, fixture, pm_env,
};

/// A value no output or error may carry.
const VALUE: &str = "s3cr3t-VALUE";

/// A Keychain holding one item, `iris`/`bob`.
struct FakeKeychain;

impl Keychain for FakeKeychain {
    fn password(&self, service: &str, account: &str) -> anyhow::Result<String> {
        if (service, account) == ("iris", "bob") {
            return Ok("from-keychain".to_owned());
        }
        bail!("tm env set: no login-Keychain item for service `{service}`, account `{account}`")
    }
}

/// [`run_with`] with `stdin` piped in: the result and stdout.
fn run_as(
    fx: &Fixture,
    env: HookEnv,
    action: EnvAction,
    stdin: &str,
) -> (anyhow::Result<()>, String) {
    let mut input = stdin.as_bytes();
    let mut out = Vec::new();
    let sources = Sources {
        stdin: &mut input,
        stdin_is_tty: false,
        keychain: &FakeKeychain,
    };
    let result = run_with(
        action,
        env,
        || allowlist(fx),
        &fx.project,
        sources,
        &mut out,
    );
    (result, String::from_utf8(out).expect("utf-8"))
}

fn keys(path: &Path) -> EnvAction {
    EnvAction::Keys {
        path: path.to_path_buf(),
    }
}

fn set(path: &Path, key: &str) -> EnvAction {
    set_with(path, key, None, Vec::new())
}

/// `tm env set` with a Keychain `(service, account)` and extra arguments.
fn set_with(
    path: &Path,
    key: &str,
    keychain: Option<(&str, &str)>,
    extra: Vec<String>,
) -> EnvAction {
    EnvAction::Set {
        path: path.to_path_buf(),
        key: key.to_owned(),
        from_keychain: keychain.map(|k| k.0.to_owned()),
        account: keychain.map(|k| k.1.to_owned()),
        extra,
    }
}

fn write(fx: &Fixture, name: &str, body: &str) -> PathBuf {
    let path = fx.project.join(name);
    std::fs::write(&path, body).expect("write env file");
    path
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
}

#[test]
fn cli_parses_env_set_and_keys() {
    let cli = Cli::try_parse_from(["tm", "env", "keys", ".env.local"]).expect("parse");
    assert!(matches!(
        cli.command,
        Some(Command::Env {
            action: EnvAction::Keys { .. }
        })
    ));
    let argv = [
        "tm",
        "env",
        "set",
        "p",
        "K",
        "--from-keychain",
        "s",
        "--account",
        "a",
    ];
    let cli = Cli::try_parse_from(argv).expect("parse");
    assert!(matches!(
        cli.command,
        Some(Command::Env {
            action: EnvAction::Set {
                from_keychain: Some(_),
                account: Some(_),
                ..
            }
        })
    ));
    // `--from-keychain` without `--account` is a usage error.
    assert!(Cli::try_parse_from(["tm", "env", "set", "p", "K", "--from-keychain", "s"]).is_err());
}

#[test]
fn env_keys_prints_names_only() {
    let fx = fixture();
    let body = format!("# note\nA={VALUE}\nexport B='{VALUE} b'\n\n  C=\"{VALUE}\" # c\nA=dup\n");
    let path = write(&fx, ".env.local", &body);
    let (result, out) = run_as(&fx, architect_env(&fx), keys(&path), "");
    result.expect("keys");
    assert_eq!(out, "A\nB\nC\n");
}

#[test]
fn env_keys_never_prints_a_line_inside_a_multiline_value() {
    let fx = fixture();
    let body = "KEY=\"-----BEGIN\nINNER=1\nMIIB=\n-----END\"\nSINGLE='a\nHIDDEN=2'\nNEXT=2\n";
    let path = write(&fx, ".env.local", body);
    assert_eq!(list_keys(&path).expect("keys"), ["KEY", "SINGLE", "NEXT"]);
}

#[test]
fn env_keys_refuses_a_non_assignment_line_and_prints_nothing() {
    let fx = fixture();
    for (body, line) in [
        ("A=1\n-----BEGIN PRIVATE KEY-----\nMIIBxyz=\n", 2),
        ("A=1\nB='unterminated\nMIIBxyz=\n", 2),
        ("A=\"x\" trailing\n", 1),
        ("1A=x\n", 1),
    ] {
        let path = write(&fx, ".env.local", body);
        let (result, out) = run_as(&fx, architect_env(&fx), keys(&path), "");
        let err = format!("{:#}", result.expect_err(body));
        assert!(
            err.contains(&format!("line {line}: not an assignment")),
            "{err}"
        );
        assert!(!err.contains("MIIB") && !err.contains("BEGIN"), "{err}");
        assert_eq!(out, "", "{body}");
    }
}

#[test]
fn env_keys_refuses_a_symlink() {
    let fx = fixture();
    let real = write(&fx, "real.txt", "A=1\n");
    let link = fx.project.join(".env.local");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    let err = format!("{:#}", list_keys(&link).expect_err("a symlink"));
    assert!(err.contains("symbolic link"), "{err}");
    let dir = fx.project.join(".env.d");
    std::fs::create_dir(&dir).expect("mkdir");
    let err = format!("{:#}", list_keys(&dir).expect_err("a directory"));
    assert!(err.contains("not a regular file"), "{err}");
}

#[test]
fn env_set_replaces_in_place_and_creates_0600() {
    let fx = fixture();
    let fresh = fx.project.join(".env.production");
    assert_eq!(set_key(&fresh, "API_KEY", "abc").expect("add"), "added");
    assert_eq!(
        std::fs::read_to_string(&fresh).expect("read"),
        "API_KEY=abc\n"
    );
    assert_eq!(mode(&fresh), 0o600);
    let path = write(
        &fx,
        ".env",
        "A=1\nexport API_KEY='old\nmulti'\nB=2\nAPI_KEY=dup\n",
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    assert_eq!(
        set_key(&path, "API_KEY", "new value").expect("replace"),
        "replaced"
    );
    let text = std::fs::read_to_string(&path).expect("read");
    assert_eq!(text, "A=1\nexport API_KEY='new value'\nB=2\n");
    assert_eq!(mode(&path), 0o600);
    // A value with every quote round-trips through the parser.
    set_key(&path, "C", "it's \"$x\"").expect("set");
    assert_eq!(list_keys(&path).expect("keys"), ["A", "API_KEY", "B", "C"]);
    let link = fx.project.join(".env.link");
    std::os::unix::fs::symlink(&path, &link).expect("symlink");
    assert!(set_key(&link, "A", "x").is_err(), "a symlink is refused");
}

#[test]
fn env_set_never_echoes_the_value_on_success_or_error() {
    let fx = fixture();
    let path = fx.project.join(".env.local");
    let (result, out) = run_as(
        &fx,
        architect_env(&fx),
        set(&path, "API_KEY"),
        &format!("{VALUE}\n"),
    );
    result.expect("set");
    assert_eq!(out, "set API_KEY (added)\n");
    let link = fx.project.join(".env.link");
    std::os::unix::fs::symlink(&path, &link).expect("symlink");
    let bad = write(&fx, ".env.bad", "-----BEGIN\n");
    let argv = set_with(&path, "API_KEY", None, vec![VALUE.to_owned()]);
    let failures = [
        (set(&link, "API_KEY"), format!("{VALUE}\n")),
        (set(&bad, "API_KEY"), format!("{VALUE}\n")),
        (set(&path, "API_KEY"), format!("{VALUE}\nsecond line\n")),
        (set(&path, &format!("API_KEY={VALUE}")), String::new()),
        (set(&path, &format!("{VALUE} x")), format!("{VALUE}\n")),
        (argv, String::new()),
    ];
    for (action, stdin) in failures {
        let (result, out) = run_as(&fx, architect_env(&fx), action, &stdin);
        let err = result.expect_err("refused");
        for text in [format!("{err:#}"), format!("{err:?}"), out] {
            assert!(!text.contains(VALUE), "echoed: {text}");
        }
    }
    let text = std::fs::read_to_string(&path).expect("read");
    assert_eq!(text, format!("API_KEY={VALUE}\n"), "no failure wrote");
}

#[test]
fn env_set_refuses_the_value_in_argv() {
    let fx = fixture();
    let path = fx.project.join(".env.local");
    let cli = Cli::try_parse_from(["tm", "env", "set", "p", "K", "v"]).expect("parse");
    let Some(Command::Env {
        action: EnvAction::Set { extra, .. },
    }) = cli.command
    else {
        panic!("not `env set`");
    };
    assert_eq!(extra, ["v"]);
    let argv = set_with(&path, "K", None, extra);
    for action in [set(&path, "K=v"), argv] {
        let (result, _) = run_as(&fx, architect_env(&fx), action, "v\n");
        assert_eq!(format!("{}", result.expect_err("refused")), ARGV_REFUSED);
    }
    assert!(!path.exists(), "nothing was written");
}

#[test]
fn env_set_reads_the_value_from_stdin_or_the_keychain() {
    let fx = fixture();
    let path = fx.project.join(".env.local");
    run_as(&fx, architect_env(&fx), set(&path, "K"), "v1\r\n")
        .0
        .expect("stdin");
    assert_eq!(std::fs::read_to_string(&path).expect("read"), "K=v1\n");
    let keychain = |service: &str| set_with(&path, "K", Some((service, "bob")), Vec::new());
    run_as(&fx, architect_env(&fx), keychain("iris"), "ignored\n")
        .0
        .expect("keychain");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "K=from-keychain\n"
    );
    assert!(
        run_as(&fx, architect_env(&fx), keychain("absent"), "")
            .0
            .is_err()
    );
    assert!(
        run_as(&fx, architect_env(&fx), set(&path, "K"), "")
            .0
            .is_err(),
        "empty stdin"
    );
    let mut input: &[u8] = b"v\n";
    let tty = Sources {
        stdin: &mut input,
        stdin_is_tty: true,
        keychain: &FakeKeychain,
    };
    let action = set(&path, "K");
    let result = run_with(
        action,
        architect_env(&fx),
        || allowlist(&fx),
        &fx.project,
        tty,
        &mut Vec::new(),
    );
    assert!(result.is_err(), "a terminal is refused");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "K=from-keychain\n"
    );
}

/// Architect ruling Q5: the verb checks the binding itself, before any file.
#[test]
fn env_verbs_refuse_a_session_that_is_not_the_architect() {
    let fx = fixture();
    let path = write(&fx, ".env.local", "A=1\n");
    for action in [keys(&path), set(&path, "B")] {
        let (result, out) = run_as(&fx, pm_env(&fx), action, "v\n");
        let err = format!("{:#}", result.expect_err("refused"));
        assert!(err.contains("not the Architect"), "{err}");
        assert_eq!(out, "");
    }
    assert_eq!(std::fs::read_to_string(&path).expect("read"), "A=1\n");
}
