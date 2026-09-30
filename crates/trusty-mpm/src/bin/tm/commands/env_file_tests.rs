//! Unit tests for `tm env set|keys` (`env_file.rs`, #8939).

use std::cell::Cell;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use clap::Parser;
use serde_json::json;

use super::*;
use crate::cli::{Cli, Command};
use crate::commands::pm_guard_architect_envfile::{
    evaluate_secret_file_read_gated, mint_envfile_grant,
};
use crate::commands::pm_guard_floor::ArchitectGate;
use crate::commands::pm_guard_trust_anchor::tests::{
    Fixture, allowlist, architect_env, fixture, payload, pm_env, spoof_env,
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

/// The direct `tm env` command the main thread would run for `action`.
fn command_for(action: &EnvAction) -> String {
    match action {
        EnvAction::Keys { path } => format!("tm env keys {}", path.display()),
        EnvAction::Set {
            path,
            key,
            from_keychain,
            account,
            extra,
        } => {
            let mut command = format!("tm env set {} {key}", path.display());
            if let (Some(s), Some(a)) = (from_keychain, account) {
                command.push_str(&format!(" --from-keychain {s} --account {a}"));
            }
            for word in extra {
                command.push_str(&format!(" {word}"));
            }
            command
        }
    }
}

/// The pm-guard's decision on the main thread's direct call for `action`,
/// minting its grant when it exempts the call; `true` when it did.
fn grant(fx: &Fixture, action: &EnvAction) -> bool {
    let call = payload(fx, "Bash", json!({ "command": command_for(action) }));
    let gate = ArchitectGate::new(&call, architect_env(fx), || allowlist(fx));
    let deny = evaluate_secret_file_read_gated("Bash", call.get("tool_input"), &fx.project, &gate);
    mint_envfile_grant(&gate);
    deny.is_none() && gate.envfile().is_some()
}

/// [`run_as`] as the Architect's main thread: the guard grants first.
fn run_granted(fx: &Fixture, action: EnvAction, stdin: &str) -> (anyhow::Result<()>, String) {
    grant(fx, &action);
    run_as(fx, architect_env(fx), action, stdin)
}

/// `path` as the shared policy places it for the Architect.
fn scoped(fx: &Fixture, path: &Path, may_be_absent: bool) -> Option<ScopedEnvfile> {
    let word = path.to_str().expect("utf-8 path");
    envfile_policy(word, &fx.project, may_be_absent, &spoof_env(fx), || {
        allowlist(fx)
    })
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
    let (result, out) = run_granted(&fx, keys(&path), "");
    result.expect("keys");
    assert_eq!(out, "A\nB\nC\n");
}

#[test]
fn env_keys_never_prints_a_line_inside_a_multiline_value() {
    let fx = fixture();
    let body = "KEY=\"-----BEGIN\nINNER=1\nMIIB=\n-----END\"\nSINGLE='a\nHIDDEN=2'\nNEXT=2\n";
    let path = write(&fx, ".env.local", body);
    let file = scoped(&fx, &path, false).expect("in scope");
    assert_eq!(list_keys(&file).expect("keys"), ["KEY", "SINGLE", "NEXT"]);
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
        let (result, out) = run_granted(&fx, keys(&path), "");
        let err = format!("{:#}", result.expect_err(body));
        assert!(
            err.contains(&format!("line {line}: not an assignment")),
            "{err}"
        );
        assert!(!err.contains("MIIB") && !err.contains("BEGIN"), "{err}");
        assert_eq!(out, "", "{body}");
    }
}

/// The policy refuses a symlink or directory; one swapped in after the policy
/// placed the file is refused by the no-follow open.
#[test]
fn env_keys_refuses_a_symlink() {
    let fx = fixture();
    let real = write(&fx, "real.txt", "A=1\n");
    let link = fx.project.join(".env.local");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    assert_eq!(scoped(&fx, &link, false), None, "a symlink");
    let dir = fx.project.join(".env.d");
    std::fs::create_dir(&dir).expect("mkdir");
    assert_eq!(scoped(&fx, &dir, false), None, "a directory");
    let path = write(&fx, ".env.swap", "A=1\n");
    let file = scoped(&fx, &path, false).expect("in scope");
    std::fs::remove_file(&path).expect("rm");
    std::os::unix::fs::symlink(&real, &path).expect("symlink");
    let err = format!("{:#}", list_keys(&file).expect_err("a symlink"));
    assert!(err.contains("symbolic link"), "{err}");
    std::fs::remove_file(&path).expect("rm");
    std::fs::create_dir(&path).expect("mkdir");
    let err = format!("{:#}", list_keys(&file).expect_err("a directory"));
    assert!(err.contains("not a regular file"), "{err}");
    let linked = write(&fx, ".env.hard", "A=1\n");
    let file = scoped(&fx, &linked, false).expect("in scope");
    std::fs::hard_link(&linked, fx.cwd.join("outside")).expect("hard link");
    let err = format!("{:#}", list_keys(&file).expect_err("a hard link"));
    assert!(err.contains("hard link"), "{err}");
}

#[test]
fn env_set_replaces_in_place_and_creates_0600() {
    let fx = fixture();
    let fresh = fx.project.join(".env.production");
    let file = scoped(&fx, &fresh, true).expect("in scope");
    assert_eq!(set_key(&file, "API_KEY", "abc").expect("add"), "added");
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
    let file = scoped(&fx, &path, true).expect("in scope");
    assert_eq!(
        set_key(&file, "API_KEY", "new value").expect("replace"),
        "replaced"
    );
    let text = std::fs::read_to_string(&path).expect("read");
    assert_eq!(text, "A=1\nexport API_KEY='new value'\nB=2\n");
    assert_eq!(mode(&path), 0o600);
    // A value with every quote round-trips through the parser.
    set_key(&file, "C", "it's \"$x\"").expect("set");
    assert_eq!(list_keys(&file).expect("keys"), ["A", "API_KEY", "B", "C"]);
    let link = fx.project.join(".env.link");
    std::os::unix::fs::symlink(&path, &link).expect("symlink");
    assert_eq!(scoped(&fx, &link, true), None, "a symlink is refused");
}

#[test]
fn env_set_never_echoes_the_value_on_success_or_error() {
    let fx = fixture();
    let path = fx.project.join(".env.local");
    let (result, out) = run_granted(&fx, set(&path, "API_KEY"), &format!("{VALUE}\n"));
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
        let (result, out) = run_granted(&fx, action, &stdin);
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
        let (result, _) = run_granted(&fx, action, "v\n");
        assert_eq!(format!("{}", result.expect_err("refused")), ARGV_REFUSED);
    }
    assert!(!path.exists(), "nothing was written");
}

#[test]
fn env_set_reads_the_value_from_stdin_or_the_keychain() {
    let fx = fixture();
    let path = fx.project.join(".env.local");
    run_granted(&fx, set(&path, "K"), "v1\r\n")
        .0
        .expect("stdin");
    assert_eq!(std::fs::read_to_string(&path).expect("read"), "K=v1\n");
    let keychain = |service: &str| set_with(&path, "K", Some((service, "bob")), Vec::new());
    run_granted(&fx, keychain("iris"), "ignored\n")
        .0
        .expect("keychain");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "K=from-keychain\n"
    );
    assert!(run_granted(&fx, keychain("absent"), "").0.is_err());
    assert!(
        run_granted(&fx, set(&path, "K"), "").0.is_err(),
        "empty stdin"
    );
    let mut input: &[u8] = b"v\n";
    let tty = Sources {
        stdin: &mut input,
        stdin_is_tty: true,
        keychain: &FakeKeychain,
    };
    let action = set(&path, "K");
    assert!(grant(&fx, &action), "granted");
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
        grant(&fx, &action);
        let (result, out) = run_as(&fx, pm_env(&fx), action, "v\n");
        let err = format!("{:#}", result.expect_err("refused"));
        assert!(err.contains("not the Architect"), "{err}");
        assert_eq!(out, "");
    }
    assert_eq!(std::fs::read_to_string(&path).expect("read"), "A=1\n");
}

/// The guard's grant is the main-thread proof: it runs one exact call once.
#[test]
fn the_main_thread_grant_lets_the_verb_run_once() {
    let fx = fixture();
    let path = write(&fx, ".env.local", "A=1\n");
    assert!(
        grant(&fx, &keys(&path)),
        "the guard exempts the main thread"
    );
    let (result, out) = run_as(&fx, architect_env(&fx), keys(&path), "");
    result.expect("the granted call");
    assert_eq!(out, "A\n");
    let (result, out) = run_as(&fx, architect_env(&fx), keys(&path), "");
    assert_eq!(format!("{}", result.expect_err("spent")), NO_GRANT);
    assert_eq!(out, "");
}

#[test]
fn a_grant_expires_and_binds_its_exact_call() {
    let fx = fixture();
    let path = write(&fx, ".env.local", "A=1\n");
    let file = scoped(&fx, &path, true).expect("in scope");
    let call = |verb, key: &str, keychain: Option<(&str, &str)>| EnvfileCall {
        verb,
        path: file.path().to_path_buf(),
        keys: (!key.is_empty())
            .then(|| key.to_owned())
            .into_iter()
            .collect(),
        keychain: keychain.map(|(s, a)| (s.to_owned(), a.to_owned())),
    };
    let home = &fx.home;
    let granted = call("set", "K", Some(("iris", "bob")));
    env_file_grant::mint(home, &granted).expect("mint");
    let later = SystemTime::now() + env_file_grant::GRANT_TTL + Duration::from_secs(5);
    assert!(
        !env_file_grant::consume_at(home, &granted, later),
        "expired"
    );
    env_file_grant::mint(home, &granted).expect("mint");
    for other in [
        call("keys", "", None),
        call("set", "K", None),
        call("set", "J", Some(("iris", "bob"))),
        call("set", "K", Some(("iris", "eve"))),
    ] {
        assert!(!env_file_grant::consume(home, &other), "{other:?}");
    }
    assert!(env_file_grant::consume(home, &granted), "the exact call");
    assert!(!env_file_grant::consume(home, &granted), "spent");
}

/// A directory on the checked path swapped for a symlink after the policy
/// placed the file is refused by the no-follow walk.
#[test]
fn a_parent_swapped_for_a_symlink_after_the_check_is_refused() {
    let fx = fixture();
    let sub = fx.project.join("sub");
    std::fs::create_dir(&sub).expect("mkdir");
    let path = sub.join(".env.local");
    std::fs::write(&path, "A=1\n").expect("write");
    let file = scoped(&fx, &path, true).expect("in scope");
    std::fs::write(fx.cwd.join(".env.local"), "OUT=1\n").expect("write outside");
    std::fs::rename(&sub, fx.project.join("sub.moved")).expect("move sub");
    std::os::unix::fs::symlink(&fx.cwd, &sub).expect("symlink sub");
    for err in [
        list_keys(&file).expect_err("keys"),
        set_key(&file, "B", "2").expect_err("set"),
    ] {
        assert!(format!("{err:#}").contains("symbolic link"), "{err:#}");
    }
    let outside = std::fs::read_to_string(fx.cwd.join(".env.local")).expect("read");
    assert_eq!(outside, "OUT=1\n", "nothing written out of scope");
}

// ---- #8939 fix round: red against 0cacf1c564 (old `run_with`) ----

/// A Keychain that counts its lookups.
struct CountingKeychain(Cell<usize>);

impl Keychain for CountingKeychain {
    fn password(&self, _service: &str, _account: &str) -> anyhow::Result<String> {
        self.0.set(self.0.get() + 1);
        Ok(VALUE.to_owned())
    }
}

/// The Architect-bound [`run_with`] with `keychain`; the result and stdout.
fn run_counting(
    fx: &Fixture,
    action: EnvAction,
    keychain: &CountingKeychain,
) -> (anyhow::Result<()>, String) {
    let mut input: &[u8] = b"v\n";
    let mut out = Vec::new();
    let sources = Sources {
        stdin: &mut input,
        stdin_is_tty: false,
        keychain,
    };
    let result = run_with(
        action,
        architect_env(fx),
        || allowlist(fx),
        &fx.project,
        sources,
        &mut out,
    );
    (result, String::from_utf8(out).expect("utf-8"))
}

/// Critic CRITICAL: a Keychain value written to a file that is not
/// secret-named, which the guard never sees and a `cat` then prints.
#[test]
fn an_architect_bound_set_on_a_non_env_file_never_reads_the_keychain() {
    let fx = fixture();
    let keychain = CountingKeychain(Cell::new(0));
    for leak in [fx.project.join("leak.txt"), fx.cwd.join("leak.txt")] {
        let action = set_with(&leak, "K", Some(("iris", "bob")), Vec::new());
        let (result, out) = run_counting(&fx, action, &keychain);
        assert!(result.is_err(), "{}", leak.display());
        assert!(!leak.exists(), "{} created", leak.display());
        assert_eq!(out, "");
    }
    assert_eq!(keychain.0.get(), 0, "the Keychain was read");
}

/// Critic HIGH: the verb checked no scope.
#[test]
fn an_out_of_scope_env_file_is_refused_by_the_verb() {
    let fx = fixture();
    let outside = fx.cwd.join(".env.local");
    std::fs::write(&outside, "OUT=1\n").expect("write outside");
    let fresh = fx.cwd.join(".env.production");
    let keychain = CountingKeychain(Cell::new(0));
    for action in [
        keys(&outside),
        set(&outside, "K"),
        set_with(&fresh, "K", Some(("iris", "bob")), Vec::new()),
    ] {
        let (result, out) = run_counting(&fx, action, &keychain);
        assert!(result.is_err(), "out of scope ran");
        assert_eq!(out, "");
    }
    let text = std::fs::read_to_string(&outside).expect("read");
    assert_eq!(text, "OUT=1\n");
    assert!(!fresh.exists(), "created out of scope");
    assert_eq!(keychain.0.get(), 0, "the Keychain was read");
}

/// Critic HIGH: a parent directory that is a symlink out of scope.
#[test]
fn a_parent_directory_symlink_out_of_scope_is_refused() {
    let fx = fixture();
    let link = fx.project.join("linked");
    std::os::unix::fs::symlink(&fx.cwd, &link).expect("symlink");
    let target = fx.cwd.join(".env.local");
    let keychain = CountingKeychain(Cell::new(0));
    let via = link.join(".env.local");
    let action = set_with(&via, "K", Some(("iris", "bob")), Vec::new());
    let (result, _) = run_counting(&fx, action, &keychain);
    assert!(result.is_err(), "wrote through the link");
    assert!(!target.exists(), "created out of scope");
    assert_eq!(keychain.0.get(), 0, "the Keychain was read");
}

/// Architect ruling on item 5 (#8878 owner ruling 2026-09-29): a Claude Code
/// subagent shares the Architect's `claude` and environment and carries no
/// `CLAUDE_MPM_SUB_AGENT`, so it passes the binding; with no main-thread
/// grant from the guard, it is refused before the file or the Keychain.
#[test]
fn a_subagent_shaped_caller_without_a_grant_is_refused() {
    let fx = fixture();
    let path = write(&fx, ".env.local", "A=1\n");
    let keychain = CountingKeychain(Cell::new(0));
    assert!(!architect_env(&fx).sub_agent, "no CLAUDE_MPM_SUB_AGENT");
    for action in [
        keys(&path),
        set_with(&path, "K", Some(("iris", "bob")), Vec::new()),
    ] {
        let (result, out) = run_counting(&fx, action, &keychain);
        assert!(result.is_err(), "a subagent-shaped caller ran the verb");
        assert_eq!(out, "");
    }
    assert_eq!(std::fs::read_to_string(&path).expect("read"), "A=1\n");
    assert_eq!(keychain.0.get(), 0, "the Keychain was read");
}
