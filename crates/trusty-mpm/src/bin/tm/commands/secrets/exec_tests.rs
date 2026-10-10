//! Unit tests for the pure parts of `tm secrets exec` (#9621): the spec,
//! the stub's argv, the token check, the report frame and the scrubber.
//! The spawn, grant and revoke paths run the binary in
//! `tests/tm_secrets_exec.rs`.

use std::collections::BTreeMap;
use std::path::Path;

use clap::Parser;
use trusty_secrets::SecretKey;

use super::exec_scrub::Scrubber;
use super::exec_wire::{self, ExecSpec, Report, TOKEN_FD_ENV};
use crate::cli::SecretsAction;
use crate::cli::{Cli, Command};

/// A value the tests treat as secret.
const VALUE: &str = "sk-live-9621-SPLIT-c0ffee";

fn key(name: &str) -> SecretKey {
    SecretKey::new(name).expect("valid key")
}

fn command() -> Vec<String> {
    vec!["printenv".to_owned()]
}

#[test]
fn exec_spec_refuses_bad_bindings_without_quoting_them() {
    let ok = ExecSpec::parse(
        &["GH_TOKEN=GH_TOKEN", "B=OTHER"],
        Some("GH_TOKEN"),
        command(),
    )
    .expect("valid spec");
    assert_eq!(ok.env.len(), 2);
    assert_eq!(
        ok.keys().into_iter().collect::<Vec<_>>(),
        vec![key("GH_TOKEN"), key("OTHER")],
        "a key named twice is granted and resolved once"
    );
    for (env, stdin, cmd) in [
        (vec![VALUE], None, command()),
        (vec!["1BAD=K"], None, command()),
        (vec!["A-B=K"], None, command()),
        (vec!["A=K", "A=L"], None, command()),
        (vec![&*format!("{TOKEN_FD_ENV}=K")], None, command()),
        (vec![&*format!("A={VALUE} x")], None, command()),
        (vec![], Some(&*format!("{VALUE} x")), command()),
        (vec![], None, command()),
        (vec!["A=K"], None, vec![]),
    ] {
        let err = ExecSpec::parse(&env, stdin, cmd).expect_err("refused");
        let text = format!("{err:#}");
        assert!(
            !text.contains("c0ffee"),
            "error quotes the argument: {text}"
        );
    }
}

#[test]
fn stub_args_carry_names_and_fds_only() {
    let spec = ExecSpec::parse(
        &["A=GH_TOKEN"],
        Some("NPM_TOKEN"),
        vec!["gh".into(), "pr".into()],
    )
    .expect("valid spec");
    let args: Vec<String> = exec_wire::stub_args(Path::new("/run/s.sock"), 7, 9, &spec)
        .into_iter()
        .map(|a| a.into_string().expect("utf-8"))
        .collect();
    assert_eq!(
        args,
        [
            "secrets",
            "__exec-stub",
            "--socket",
            "/run/s.sock",
            "--token-fd",
            "7",
            "--report-fd",
            "9",
            "--env",
            "A=GH_TOKEN",
            "--stdin",
            "NPM_TOKEN",
            "--",
            "gh",
            "pr",
        ]
    );
    // The stub's argv parses back to the same spec.
    let argv = ["tm".to_owned()].into_iter().chain(args);
    let Some(Command::Secrets {
        action:
            SecretsAction::ExecStub {
                env,
                stdin,
                command,
                token_fd,
                report_fd,
                ..
            },
    }) = Cli::try_parse_from(argv).expect("parse").command
    else {
        panic!("not the stub");
    };
    assert_eq!((token_fd, report_fd), (7, 9));
    let env: Vec<&str> = env.iter().map(|e| e.text()).collect();
    let back = ExecSpec::parse(&env, stdin.as_ref().map(|s| s.text()), command).expect("spec");
    assert_eq!(back, spec);
}

#[test]
fn exec_parses_and_redacts_its_arguments() {
    let binding = format!("A={VALUE}");
    let argv = [
        "tm", "secrets", "exec", "--env", &binding, "--stdin", VALUE, "--", "true",
    ];
    let cli = Cli::try_parse_from(argv).expect("parse");
    assert!(
        !format!("{cli:?}").contains("c0ffee"),
        "Debug shows an argument"
    );
    // The command is required, after `--`.
    assert!(Cli::try_parse_from(["tm", "secrets", "exec", "--env", "A=K"]).is_err());
}

#[test]
fn token_check_refuses_short_and_padded_tokens() {
    let good = "0123456789abcdef".repeat(4);
    assert!(exec_wire::is_token(&good));
    for bad in [
        String::new(),
        good[..63].to_owned(),
        format!("{good}\n"),
        good.to_uppercase(),
        format!("{}g", &good[..63]),
    ] {
        assert!(!exec_wire::is_token(&bad), "accepted {bad:?}");
    }
}

#[test]
fn report_round_trips_and_a_cut_report_is_none() {
    let mut values = BTreeMap::new();
    values.insert(key("A"), VALUE.to_owned());
    values.insert(key("B"), String::new());
    let ok = exec_wire::encode_ok(&values);
    assert_eq!(
        exec_wire::decode_report(&ok),
        Some(Report::Resolved(values))
    );
    let fail = exec_wire::encode_fail(Some(&key("A")), "grant refused");
    assert_eq!(
        exec_wire::decode_report(&fail),
        Some(Report::Failed {
            key: Some(key("A")),
            reason: "grant refused".to_owned()
        })
    );
    // Every proper prefix — a stub that died mid-write — is no report.
    for cut in 0..ok.len() {
        assert_eq!(exec_wire::decode_report(&ok[..cut]), None, "cut at {cut}");
    }
    let mut trailing = ok.clone();
    trailing.push(b'x');
    assert_eq!(exec_wire::decode_report(&trailing), None);
}

/// Feed `chunks` through one scrubber; the whole output.
fn scrub(values: &[&str], chunks: &[&[u8]]) -> Vec<u8> {
    let mut scrubber = Scrubber::new(values);
    let mut out = Vec::new();
    for chunk in chunks {
        out.extend(scrubber.push(chunk));
    }
    out.extend(scrubber.finish());
    out
}

#[test]
fn scrubber_removes_a_value_split_across_reads() {
    let line = format!("token={VALUE}\n");
    let bytes = line.as_bytes();
    // Every split point, including inside the value.
    for cut in 0..=bytes.len() {
        let out = scrub(&[VALUE], &[&bytes[..cut], &bytes[cut..]]);
        assert_eq!(out, b"token=[REDACTED]\n", "split at {cut}");
    }
    // One byte at a time.
    let singles: Vec<&[u8]> = bytes.chunks(1).collect();
    assert_eq!(scrub(&[VALUE], &singles), b"token=[REDACTED]\n");
    // A near miss is released once it cannot be a value, not held forever.
    let mut scrubber = Scrubber::new(&[VALUE]);
    assert_eq!(
        scrubber.push(b"sk-live-9621-X tail"),
        b"sk-live-9621-X tail"
    );
}

#[test]
fn scrubber_never_cuts_a_value_that_starts_with_another() {
    let short = &VALUE[..12];
    let out = scrub(
        &[short, VALUE],
        &[&VALUE.as_bytes()[..12], &VALUE.as_bytes()[12..]],
    );
    assert_eq!(out, b"[REDACTED]");
    let out = scrub(&[short, VALUE], &[format!("{short}!").as_bytes()]);
    assert_eq!(out, b"[REDACTED]!");
}

#[test]
fn scrubber_passes_short_values_and_binary_through() {
    let binary: &[u8] = &[0xff, 0x00, 0xfe, b'\n'];
    let head: &[u8] = b"seven77 ";
    assert_eq!(
        scrub(&["seven77"], &[head, binary]),
        [head, binary].concat()
    );
    assert_eq!(scrub(&[] as &[&str], &[binary]), binary);
}

/// Why: #9621 — tm clears `FD_CLOEXEC` on the report pipe so the stub
/// inherits it; the stub must set it again, or a server its first resolve
/// auto-spawns holds the write end and tm never sees EOF.
/// Red when `inherited` leaves the flag clear (the child sees the fd).
/// Test: itself.
#[test]
fn stub_inherited_fd_is_close_on_exec() {
    use std::os::fd::{AsRawFd, IntoRawFd};

    let (_read, write) = std::io::pipe().expect("pipe");
    let fd = write.into_raw_fd();
    super::exec_proc::clear_cloexec(fd).expect("clear the flag, as tm does");
    let report = super::exec_stub::inherited(fd).expect("an open fd is taken");
    assert_eq!(report.as_raw_fd(), fd);
    // SAFETY: `fcntl(F_GETFD)` only reads the flags of an fd we own.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0, "flags {flags}");
    let status = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("[ -e /dev/fd/{fd} ]"))
        .status()
        .expect("spawn sh");
    assert!(!status.success(), "a spawned child inherited fd {fd}");
}
