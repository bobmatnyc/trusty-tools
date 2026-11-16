//! Tests for the console's Linux lifecycle verbs (#11085).
//!
//! The module's stop, identity and spawn rules are tested in trusty-common's
//! `tests/linux_lifecycle.rs`. These pin what this crate supplies: the
//! descriptor, the environment, the `http_addr` fallback (AC10) and the exit
//! status of each verb, with real fixture processes for the fallback.
//! No test here mutates the process environment.

use std::ffi::OsString;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use trusty_common::linux_lifecycle::{PinnedProcess, ProcessTable, Signal};

use super::addr_pid::{AddrLookup, Listener, lookup, parse_listener};
use super::env::{CREDENTIAL_NAMED_EXCEPTIONS, EXCLUDED, EXCLUDED_PREFIXES};
use super::*;

/// The fixture's data dir; set only in the fixture child.
const FIXTURE_DIR_ENV: &str = "CONSOLE_LC_FIXTURE_DIR";
/// Set in the fixture child to skip writing `http_addr`.
const FIXTURE_NO_ADDR_ENV: &str = "CONSOLE_LC_FIXTURE_NO_ADDR";
/// Set in the fixture child to ignore SIGTERM.
const FIXTURE_IGNORE_TERM_ENV: &str = "CONSOLE_LC_FIXTURE_IGNORE_TERM";

fn target_in(dir: &Path, override_value: Option<&OsString>) -> Target {
    let mut target = target_for(
        override_value,
        dir.join("trusty-console.sock"),
        dir.join("http_addr"),
    )
    .expect("target");
    // An install dir with no binary: a start that ran fails BinaryMissing.
    target.desc.install_dir = dir.join("empty-install");
    target
}

#[test]
fn descriptor_names_serve_args_socket_signal_and_instance() {
    let data = OsString::from("/tmp/console-lc-data");
    let target = target_for(
        Some(&data),
        PathBuf::from("/tmp/c/trusty-console.sock"),
        PathBuf::from("/tmp/c/http_addr"),
    )
    .expect("target");
    let desc = &target.desc;
    assert_eq!(desc.binary, "trusty-console");
    assert_eq!(desc.socket, PathBuf::from("/tmp/c/trusty-console.sock"));
    assert_eq!(desc.stopped_signal, StoppedSignal::SocketRefuses);
    // A `port`/`config` CLI process lacks `serve`, so it never passes identity.
    assert_eq!(desc.liveness_args, ["serve"]);
    assert_eq!(desc.spawn_args, desc.liveness_args);
    assert_eq!(
        desc.instance_env,
        vec![(DATA_DIR_OVERRIDE_ENV.to_string(), Some(data))]
    );
    assert!(DaemonDescriptor::BASE_ENV.contains(&DATA_DIR_OVERRIDE_ENV));
    assert_eq!(desc.working_dir, PathBuf::from("/"));
    assert_eq!(target.addr_file, PathBuf::from("/tmp/c/http_addr"));
}

// #11085: a name missing here is one a `start`-spawned console silently drops.
#[test]
fn env_passthrough_carries_security_and_data_names_and_no_credential() {
    let target = target_in(Path::new("/tmp/c"), None);
    let passed = |name: &str| target.desc.env_passthrough.iter().any(|n| n == name);
    for name in [
        // Tailnet exposure of the console.
        "TRUSTY_CONSOLE_BIND",
        // Which daemon sockets the console dials.
        "TRUSTY_MPM_SOCKET",
        "TRUSTY_MEMORY_SOCKET",
        "TRUSTY_SEARCH_SOCKET",
        "TRUSTY_DATA_DIR",
        // The secrets bridge's registry root and external vault.
        "TRUSTY_MPM_ROOT",
        "TRUSTY_SECRETS_EXTERNAL",
        "TRUSTY_ANALYZE_EXTERNAL",
        "HTTPS_PROXY",
        "SSL_CERT_FILE",
        "CARGO_HOME",
        "RUST_LOG",
    ] {
        assert!(passed(name), "{name} must reach the console");
    }
    for name in &target.desc.env_passthrough {
        // #11085: passed on purpose, documented in `CREDENTIAL_NAMED_EXCEPTIONS`.
        if CREDENTIAL_NAMED_EXCEPTIONS.contains(&name.as_str()) {
            continue;
        }
        // By name segment: `TRUSTY_SECRETS_EXTERNAL` is a vault-location flag,
        // while `GITHUB_TOKEN` is a credential.
        let upper = name.to_ascii_uppercase();
        let credential = upper.split('_').any(|segment| {
            ["KEY", "APIKEY", "TOKEN", "SECRET", "PASSWORD", "PASS"].contains(&segment)
        });
        assert!(!credential, "{name} looks like a credential");
    }
    for (name, _) in EXCLUDED {
        assert!(!passed(name), "{name} is excluded");
    }
    for (prefix, _) in EXCLUDED_PREFIXES {
        let hit = target
            .desc
            .env_passthrough
            .iter()
            .find(|n| n.starts_with(prefix));
        assert!(hit.is_none(), "{hit:?} is excluded by prefix {prefix}");
    }
    assert!(!passed("GITHUB_TOKEN"));
}

// #11085: the token-file PATH reaches a spawned console, or a restarted
// console serves every TCP route with no token (#11074, #11089).
#[test]
fn env_passthrough_carries_the_token_file_path() {
    let target = target_in(Path::new("/tmp/c"), None);
    let name = trusty_common::console_token::TOKEN_FILE_ENV;
    assert!(
        target.desc.env_passthrough.iter().any(|n| n == name),
        "{name} must reach the console"
    );
    // The exception list holds that PATH, the webhook secret and the two
    // owner-approved credential pointers, nothing else.
    assert_eq!(
        CREDENTIAL_NAMED_EXCEPTIONS,
        [
            name,
            crate::webhook::SECRET_ENV,
            "SSH_AUTH_SOCK",
            "AWS_CONFIG_FILE"
        ]
    );
    assert!(EXCLUDED.iter().all(|(excluded, _)| *excluded != name));
}

// #11085 (owner decisions 2026-11-16 and the final review): the ssh agent
// socket and the AWS config file pass as documented credential pointers, the
// trusty-secrets server tunables and the AWS SDK settings pass, and
// `AWS_ROLE_ARN` without its token-file partner does not.
#[test]
fn env_passthrough_applies_the_owner_decisions_and_final_review_lists() {
    let target = target_in(Path::new("/tmp/c"), None);
    let passed = |name: &str| target.desc.env_passthrough.iter().any(|n| n == name);
    let excluded = |name: &str| EXCLUDED.iter().any(|(e, _)| *e == name);
    // Collected, so one run names every wrong entry.
    let mut wrong: Vec<String> = Vec::new();
    for name in [
        "SSH_AUTH_SOCK",
        "AWS_CONFIG_FILE",
        "TRUSTY_SECRETS_CACHE_MAX_AGE_SECS",
        "TRUSTY_SECRETS_IDLE_TIMEOUT_SECS",
        "AWS_ENDPOINT_URL",
        "AWS_ENDPOINT_URL_BEDROCK_RUNTIME",
        "AWS_USE_FIPS_ENDPOINT",
        "AWS_USE_DUALSTACK_ENDPOINT",
        "AWS_EC2_METADATA_DISABLED",
        "AWS_MAX_ATTEMPTS",
        "AWS_RETRY_MODE",
    ] {
        if !passed(name) {
            wrong.push(format!("{name} must reach the console"));
        }
    }
    for name in [
        "AWS_ROLE_ARN",
        "TRUSTY_SECRETS_SOCKET",
        "TRUSTY_SECRETS_INDEX_DIR",
    ] {
        if passed(name) || !excluded(name) {
            wrong.push(format!("{name} must be excluded with a reason"));
        }
    }
    for name in ["SSH_AUTH_SOCK", "AWS_CONFIG_FILE"] {
        if !CREDENTIAL_NAMED_EXCEPTIONS.contains(&name) {
            wrong.push(format!("{name} must be a documented exception"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

// #11085 (owner decision): `serve` reads the webhook secret from its
// environment only, so a verb-started console without it refuses webhooks.
#[test]
fn env_passthrough_carries_the_webhook_secret() {
    let target = target_in(Path::new("/tmp/c"), None);
    let name = crate::webhook::SECRET_ENV;
    assert_eq!(name, "GITHUB_WEBHOOK_SECRET");
    assert!(
        target.desc.env_passthrough.iter().any(|n| n == name),
        "{name} must reach the console"
    );
    assert!(EXCLUDED.iter().all(|(excluded, _)| *excluded != name));
}

// #11085: a console bridge auto-starts the memory and search daemons with the
// console's environment, so every name their own verbs pass must reach the
// console too. Fails naming each missing name.
#[test]
fn console_passthrough_carries_every_memory_and_search_name() {
    use trusty_common::linux_lifecycle::passthrough::{MEMORY_DAEMON, SEARCH_DAEMON};
    let target = target_in(Path::new("/tmp/c"), None);
    let passed = |name: &str| target.desc.env_passthrough.iter().any(|n| n == name);
    let missing: Vec<&str> = MEMORY_DAEMON
        .iter()
        .chain(SEARCH_DAEMON)
        .copied()
        .filter(|name| !passed(name))
        .collect();
    assert!(missing.is_empty(), "missing from the console: {missing:?}");
    for name in [
        "TRUSTY_MEMORY_MULTI_TENANT",
        "TRUSTY_EMBEDDER_MODEL",
        "TRUSTY_EMBEDDER",
        "TRUSTY_DEVICE",
        "ORT_DYLIB_PATH",
        "LD_LIBRARY_PATH",
        "DBUS_SESSION_BUS_ADDRESS",
        // The analyze and mpm daemons the other bridges auto-start.
        "TMUX_TMPDIR",
        "ANALYZE_LSP",
        // #11085: a clap `env` attribute; the auto-started `serve` has no flag.
        "TRUSTY_ANALYZER_FACTS",
        // #11085: read through `architect_store::resolve_store_root`'s env
        // lookup, so no literal `env::var` names them.
        "DASHBOARD_ROOT",
        "ARCHITECT_PROJECT_DIR",
        // #11085: the full sweep — mpm daemon services read through constants,
        // the AWS provider probe, and trusty-common's buffered tracing.
        "TRUSTY_MPM_WORKTREE_RECLAIM",
        "TRUSTY_MPM_SUPERVISOR_INTERVAL",
        "TRUSTY_MPM_RESUME_FLAP_THRESHOLD",
        "TRUSTY_DOCTOR_LOCK_STALL_SECS",
        "AWS_PROFILE",
        "RUST_LOG_BUFFER",
    ] {
        assert!(passed(name), "{name} must reach the console");
    }
    let unique: std::collections::HashSet<&String> = target.desc.env_passthrough.iter().collect();
    assert_eq!(
        unique.len(),
        target.desc.env_passthrough.len(),
        "no duplicates"
    );
}

/// Every `env = "NAME"` inside a `#[arg(..)]` / `#[clap(..)]` attribute in a
/// crate's non-test `src/`, mapped to the file it appears in.
fn clap_env_names(src: &Path) -> std::collections::BTreeMap<String, String> {
    assert!(
        src.is_dir(),
        "{} must exist in the workspace",
        src.display()
    );
    let mut found = std::collections::BTreeMap::new();
    let mut dirs = vec![src.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).expect("read src") {
            let path = entry.expect("entry").path();
            let file = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            if !file.ends_with(".rs") || file.contains("tests") || file.contains("_test") {
                continue;
            }
            // The whole file: a `#[cfg(test)] mod x_tests;` line near the top
            // of `main.rs` would otherwise hide every attribute below it.
            let text = std::fs::read_to_string(&path).expect("read file");
            for name in clap_env_names_in(&text) {
                found.insert(name, path.display().to_string());
            }
        }
    }
    found
}

/// Every `env = "NAME"` inside a `#[arg(..)]` / `#[clap(..)]` attribute of
/// one source text, in order.
///
/// #11085: an attribute ends where its own `#[` closes — bracket depth
/// counted outside string literals and `//` comments — never at the first
/// line that merely contains `)]`.
fn clap_env_names_in(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for attr in clap_attributes(text) {
        let compact: String = attr.chars().filter(|c| !c.is_whitespace()).collect();
        for (at, _) in compact.match_indices("env=\"") {
            let prev = compact[..at].chars().next_back();
            if prev.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
                continue;
            }
            let name = compact[at + 5..].split('"').next().unwrap_or("");
            found.push(name.to_string());
        }
    }
    found
}

/// The text of each `#[arg(..)]` / `#[clap(..)]` attribute, comments removed.
fn clap_attributes(text: &str) -> Vec<String> {
    let mut attrs = Vec::new();
    let mut rest = text;
    while let Some(start) = ["#[arg(", "#[clap("]
        .iter()
        .filter_map(|open| rest.find(open))
        .min()
    {
        // A start inside a `//` comment is no attribute.
        let line_start = rest[..start].rfind('\n').map_or(0, |i| i + 1);
        if rest[line_start..start].contains("//") {
            rest = &rest[start + 2..];
            continue;
        }
        let (mut depth, mut in_str, mut escaped, mut in_comment) = (0usize, false, false, false);
        let mut attr = String::new();
        let mut end = rest.len();
        let mut chars = rest[start..].char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            if in_comment {
                in_comment = c != '\n';
                continue;
            }
            if in_str {
                attr.push(c);
                match (escaped, c) {
                    (true, _) => escaped = false,
                    (false, '\\') => escaped = true,
                    (false, '"') => in_str = false,
                    _ => {}
                }
                continue;
            }
            if c == '/' && chars.peek().is_some_and(|&(_, n)| n == '/') {
                in_comment = true;
                continue;
            }
            attr.push(c);
            match c {
                '"' => in_str = true,
                '[' | '(' => depth += 1,
                ']' | ')' => depth = depth.saturating_sub(1),
                _ => {}
            }
            if depth == 0 && c == ']' {
                end = start + i + 1;
                break;
            }
        }
        attrs.push(attr);
        rest = &rest[end..];
    }
    attrs
}

// #11085: an inner line holding `)]` (here `vec![(1, 2)]`, or a string) must
// not end a multi-line attribute before its own closing `)]`.
#[test]
fn clap_scan_reads_past_an_inner_close_bracket() {
    let text = r#"
        #[arg(
            long,
            default_value_t = pick(vec![(1, 2)]),
            help = "a note (see #1)]",
            env = "LATE_NAME",
        )]
        late: u32,
        #[arg(long, env = "NEXT_NAME")] next: u32,
        #[serde(rename = "x")]
        not_clap: u32,
        #[arg(long)]
        // env = "IN_A_COMMENT"
        plain: u32,
    "#;
    assert_eq!(clap_env_names_in(text), ["LATE_NAME", "NEXT_NAME"]);
}

// #11085: the first variable trace read `env::var` / `var_os` only and missed
// `TRUSTY_ANALYZER_FACTS`, a clap `env` attribute. A clap env name in a crate
// whose daemon a console bridge starts must be passed or excluded with a reason.
#[test]
fn every_clap_env_name_a_bridge_daemon_crate_reads_is_passed_or_excluded() {
    let target = target_in(Path::new("/tmp/c"), None);
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut seen = std::collections::BTreeMap::new();
    for krate in [
        "trusty-memory",
        "trusty-search",
        "trusty-analyze",
        "trusty-mpm",
    ] {
        seen.extend(clap_env_names(&crates.join(krate).join("src")));
    }
    // The scan must see known attributes, or an empty scan would pass.
    for known in ["TRUSTY_ANALYZER_FACTS", "TRUSTY_INDEX", "TRUSTY_MPM_URL"] {
        assert!(
            seen.contains_key(known),
            "the scan missed {known}: {seen:?}"
        );
    }
    let missing: Vec<String> = seen
        .iter()
        .filter(|(name, _)| {
            !target.desc.env_passthrough.iter().any(|n| n == *name)
                && !DaemonDescriptor::BASE_ENV.contains(&name.as_str())
                && !EXCLUDED.iter().any(|(excluded, _)| excluded == name)
        })
        .map(|(name, file)| format!("{name} ({file})"))
        .collect();
    assert!(
        missing.is_empty(),
        "not passed and not excluded: {missing:?}"
    );
}

#[test]
fn descriptor_refuses_blank_and_non_utf8_override() {
    use std::os::unix::ffi::OsStringExt as _;
    let dir = Path::new("/tmp/c");
    for blank in ["", " ", "\t"] {
        let err = target_for(
            Some(&OsString::from(blank)),
            dir.join("s.sock"),
            dir.join("http_addr"),
        )
        .expect_err("a blank override must be refused");
        assert!(format!("{err:#}").contains("blank"), "{err:#}");
    }
    let raw = OsString::from_vec(b"/tmp/c-\xff".to_vec());
    let err = target_for(Some(&raw), dir.join("s.sock"), dir.join("http_addr"))
        .expect_err("a non-UTF-8 override must be refused");
    assert!(format!("{err:#}").contains("UTF-8"), "{err:#}");
}

#[test]
fn stop_report_maps_each_outcome_to_its_exit_status() {
    let stopped = StopOutcome::Stopped {
        pid: 7,
        forced: false,
    };
    assert!(stop_report(Ok((stopped, Route::HttpAddr(7)))).expect("stopped"));
    let none = stop_report(Ok((StopOutcome::NotRunning, Route::Socket))).expect_err("exit 1");
    assert!(format!("{none:#}").contains("No trusty-console running"));
    let failures = [
        LifecycleError::Survivor { pid: 4242 },
        LifecycleError::NotThisInstance {
            pid: 4242,
            binary: BINARY.into(),
        },
        LifecycleError::IdentityMismatch {
            pid: 4242,
            expected: BINARY.into(),
            found: "bash".into(),
        },
        LifecycleError::RestartBlocked {
            serving: vec![4242],
            binary: BINARY.into(),
            others: vec![4243],
        },
    ];
    for failure in failures {
        let shown = failure.to_string();
        let err = stop_report(Err(failure.into())).expect_err("every error fails");
        assert!(format!("{err:#}").contains(&shown), "{err:#}");
        assert!(format!("{err:#}").contains("4242"), "{err:#}");
    }
}

// Fail-open guard: AlreadyRunning is the one error turned into success.
#[test]
fn start_report_treats_only_already_running_as_success() {
    let already = LifecycleError::AlreadyRunning {
        socket: PathBuf::from("/tmp/c/trusty-console.sock"),
        pid: Some(9),
    };
    assert!(start_report(Err(already)).is_ok());
    let failures = [
        LifecycleError::ChildExited { pid: 5 },
        LifecycleError::RunningUnserved {
            socket: PathBuf::from("/tmp/c/trusty-console.sock"),
            binary: BINARY.into(),
            pids: vec![11],
        },
        LifecycleError::NotThisInstance {
            pid: 5,
            binary: BINARY.into(),
        },
    ];
    for failure in failures {
        let shown = failure.to_string();
        assert!(start_report(Err(failure)).is_err(), "{shown} must fail");
    }
}

#[test]
fn proc_net_tcp_lines_parse_to_listeners() {
    let v4 = "   0: 0100007F:1E6C 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 424242 1 0000000000000000 100 0 0 10 0";
    assert_eq!(
        parse_listener(v4),
        Some(Listener {
            addr: "127.0.0.1:7788".parse().expect("addr"),
            uid: 1000,
            inode: 424242,
        })
    );
    // ESTABLISHED (01) is not a listener.
    assert_eq!(parse_listener(&v4.replace(" 0A ", " 01 ")), None);
    let v6 = "   0: 00000000000000000000000001000000:1E6C 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 99 1 0000000000000000 100 0 0 10 0";
    let parsed = parse_listener(v6).expect("v6 listener");
    assert_eq!(
        parsed.addr,
        "[::1]:7788".parse::<SocketAddr>().expect("addr")
    );
    assert_eq!(parse_listener("garbage"), None);
}

#[test]
fn addr_lookup_finds_this_process_and_nothing_on_a_dead_port() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("http_addr");
    assert_eq!(lookup(&file).expect("absent"), AddrLookup::NoAddrFile);

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::fs::write(&file, addr.to_string()).expect("write");
    assert_eq!(
        lookup(&file).expect("live"),
        AddrLookup::Pid(std::process::id(), addr)
    );

    // #11085: a dropped listener frees its port for any parallel test to take,
    // so the dead port is one this test holds bound, never in LISTEN.
    let (_reservation, dead) = reserved_loopback_port();
    std::fs::write(&file, dead.to_string()).expect("write");
    assert_eq!(lookup(&file).expect("dead"), AddrLookup::NoListener(dead));
    drop(listener);
}

/// A TCP socket bound to a fresh 127.0.0.1 port and never put in LISTEN.
///
/// Why: without `SO_REUSEADDR`/`SO_REUSEPORT` the kernel binds that port for no
/// other socket while the fd lives, and the socket adds no LISTEN row to
/// `/proc/net/tcp` — a port that is free of listeners for the whole test.
fn reserved_loopback_port() -> (OwnedFd, SocketAddr) {
    // SAFETY: socket(2) has no memory preconditions; the result is checked.
    let raw = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    assert!(raw >= 0, "socket: {}", io::Error::last_os_error());
    // SAFETY: `raw` is a fresh descriptor nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: sockaddr_in is plain data; all-zero is a valid value.
    let mut sa: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    sa.sin_family = libc::AF_INET as libc::sa_family_t;
    sa.sin_addr.s_addr = u32::from(Ipv4Addr::LOCALHOST).to_be();
    let mut len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
    // SAFETY: `sa` is a valid sockaddr_in of `len` bytes for the call.
    let rc = unsafe { libc::bind(fd.as_raw_fd(), (&raw const sa).cast(), len) };
    assert_eq!(rc, 0, "bind: {}", io::Error::last_os_error());
    // SAFETY: `sa` and `len` are writable and `len` is the buffer size.
    let rc = unsafe { libc::getsockname(fd.as_raw_fd(), (&raw mut sa).cast(), &mut len) };
    assert_eq!(rc, 0, "getsockname: {}", io::Error::last_os_error());
    let port = u16::from_be(sa.sin_port);
    assert_ne!(port, 0, "the kernel assigned no port");
    (fd, SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
}

#[test]
fn addr_lookup_refuses_an_unparseable_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("http_addr");
    std::fs::write(&file, "not-an-address").expect("write");
    assert!(lookup(&file).is_err());
}

/// Delivers SIGTERM, drops SIGKILL: a survivor an unprivileged test can make.
struct DropKill;

impl SignalDelivery for DropKill {
    fn deliver(&self, target: &PinnedProcess, signal: Signal) -> io::Result<()> {
        match signal {
            Signal::Kill => Ok(()),
            other => target.send_signal(other),
        }
    }
}

/// The fixture: when [`FIXTURE_DIR_ENV`] is set, act as a console whose UDS
/// bind failed — try the socket (the parent planted a file there, so the bind
/// fails), listen on TCP, write `http_addr`, and run until the dir is gone.
#[test]
fn lifecycle_fixture_entry() {
    let Some(dir) = std::env::var_os(FIXTURE_DIR_ENV).map(PathBuf::from) else {
        return;
    };
    if std::env::var_os(FIXTURE_IGNORE_TERM_ENV).is_some() {
        // SAFETY: installing SIG_IGN for SIGTERM has no preconditions.
        unsafe {
            libc::signal(libc::SIGTERM, libc::SIG_IGN);
        }
    }
    let socket = dir.join("trusty-console.sock");
    assert!(
        std::os::unix::net::UnixListener::bind(&socket).is_err(),
        "the socket path must be unbindable"
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("fixture bind");
    if std::env::var_os(FIXTURE_NO_ADDR_ENV).is_none() {
        let addr = listener.local_addr().expect("addr").to_string();
        std::fs::write(dir.join("http_addr.tmp"), addr).expect("write addr");
        std::fs::rename(dir.join("http_addr.tmp"), dir.join("http_addr")).expect("rename");
    } else {
        std::fs::write(dir.join("ready"), b"").expect("ready marker");
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    while dir.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    std::process::exit(0);
}

/// Spawn the fixture as `<root>/bin/trusty-console serve …` with
/// `TRUSTY_DATA_DIR_OVERRIDE=<override>`, its data dir `dir`, and a regular
/// file planted at the socket path so its UDS bind fails (AC10).
fn spawn_fixture(root: &Path, dir: &Path, override_value: &Path, extra: &[&str]) -> Child {
    std::fs::create_dir_all(dir).expect("data dir");
    std::fs::write(dir.join("trusty-console.sock"), b"not a socket").expect("block the socket");
    let mut cmd = Command::new(install_fixture(root));
    // libtest takes `serve` as a second exact filter, which matches no test.
    cmd.args([
        "linux_lifecycle::tests::lifecycle_fixture_entry",
        "serve",
        "--exact",
        "--nocapture",
    ])
    .env(FIXTURE_DIR_ENV, dir)
    .env(DATA_DIR_OVERRIDE_ENV, override_value)
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    for name in extra {
        cmd.env(name, "1");
    }
    let child = cmd.spawn().expect("spawn fixture");
    let marker = if extra.contains(&FIXTURE_NO_ADDR_ENV) {
        dir.join("ready")
    } else {
        dir.join("http_addr")
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    while !marker.exists() {
        assert!(Instant::now() < deadline, "fixture never came up");
        std::thread::sleep(Duration::from_millis(20));
    }
    child
}

/// Link the test binary as `<root>/bin/trusty-console`; returns its path.
fn install_fixture(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).expect("bin dir");
    let link = bin.join(BINARY);
    if !link.exists() {
        let exe = std::env::current_exe().expect("current exe");
        std::fs::hard_link(&exe, &link)
            .or_else(|_| std::fs::copy(&exe, &link).map(|_| ()))
            .expect("link the fixture");
    }
    link
}

/// A short root (sun_path is 108 bytes) with `data/` as the override.
fn fixture_root() -> (Roots, PathBuf, PathBuf) {
    let exe = std::env::current_exe().expect("exe");
    // Beside the test binary, so the hard link stays on one filesystem.
    let bin = tempfile::tempdir_in(exe.parent().expect("exe dir")).expect("tempdir");
    // A short data dir: the exe-dir path would overrun sun_path's 108 bytes.
    let short = tempfile::tempdir().expect("short tempdir");
    let data = short.path().join("data");
    let dir = data.join(BINARY);
    (Roots { bin, _short: short }, data, dir)
}

/// The fixture's two temp dirs, removed together on drop.
struct Roots {
    bin: tempfile::TempDir,
    _short: tempfile::TempDir,
}

impl Roots {
    fn path(&self) -> &Path {
        self.bin.path()
    }
}

/// A process table under `dir` holding only `pids`, each a link to its real
/// `/proc` entry.
///
/// Why: #11085 — a scan of the live `/proc` fails closed on a host
/// trusty-console whose environ it cannot read, which failed these tests on
/// such a host. The fixture stays visible where the test needs the scan to
/// find it.
fn own_procs(dir: &Path, pids: &[u32]) -> ProcessTable {
    std::fs::create_dir_all(dir).expect("proc table dir");
    for pid in pids {
        std::os::unix::fs::symlink(format!("/proc/{pid}"), dir.join(pid.to_string()))
            .expect("link the proc entry");
    }
    ProcessTable::at(dir)
}

// AC10: the socket bind failed, so only `http_addr` finds the console.
#[tokio::test(flavor = "multi_thread")]
async fn stop_finds_the_console_through_http_addr_when_its_socket_bind_failed() {
    let (root, data, dir) = fixture_root();
    let mut fixture = spawn_fixture(root.path(), &dir, &data, &[]);
    let target = target_in(&dir, Some(&data.clone().into_os_string()));
    let procs = own_procs(&root.path().join("proc"), &[fixture.id()]);

    let result = stop_with(&target, &KernelDelivery, &procs).await;
    let exited = fixture.try_wait().expect("try_wait").is_some();
    let _ = fixture.kill();
    let _ = fixture.wait();
    let (outcome, route) = result.expect("the fallback must stop the console");
    assert_eq!(
        outcome,
        StopOutcome::Stopped {
            pid: fixture.id(),
            forced: false
        }
    );
    assert_eq!(
        route,
        Route::HttpAddr(fixture.id()),
        "the fallback identity is used"
    );
    assert!(exited, "the console exited");
}

// Fail-open guard: RunningUnserved with no http_addr stays an error, unsignalled.
#[tokio::test(flavor = "multi_thread")]
async fn stop_with_an_unserved_console_and_no_http_addr_signals_nothing() {
    let (root, data, dir) = fixture_root();
    let mut fixture = spawn_fixture(root.path(), &dir, &data, &[FIXTURE_NO_ADDR_ENV]);
    let target = target_in(&dir, Some(&data.clone().into_os_string()));
    let procs = own_procs(&root.path().join("proc"), &[fixture.id()]);

    let result = stop_with(&target, &KernelDelivery, &procs).await;
    let alive = fixture.try_wait().expect("try_wait").is_none();
    let _ = fixture.kill();
    let _ = fixture.wait();
    let err = result.expect_err("an unserved console with no http_addr is an error");
    assert!(
        format!("{err:#}").contains(&fixture.id().to_string()),
        "{err:#}"
    );
    assert!(alive, "nothing was signalled");
}

// Fail-open guard: NotRunning is kept only when http_addr leads to no
// listener; a live listener that is not this console is an error.
#[tokio::test(flavor = "multi_thread")]
async fn stop_on_not_running_with_a_foreign_listener_in_http_addr_is_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let override_value = dir.path().join("data").into_os_string();
    let target = target_in(dir.path(), Some(&override_value));
    // This test process holds the listener; its exe is not trusty-console.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::fs::write(&target.addr_file, addr.to_string()).expect("write");
    // No console of this test runs, so the scan sees an empty table.
    let procs = own_procs(&dir.path().join("proc"), &[]);

    let err = stop_with(&target, &KernelDelivery, &procs)
        .await
        .expect_err("a foreign listener must not read as not running");
    assert!(format!("{err:#}").contains(BINARY), "{err:#}");
    drop(listener);
    let (outcome, _) = stop_with(&target, &KernelDelivery, &procs)
        .await
        .expect("a dead address is not running");
    assert_eq!(outcome, StopOutcome::NotRunning);
}

// AC2: a survivor found through http_addr fails the restart and starts nothing.
#[tokio::test(flavor = "multi_thread")]
async fn restart_through_http_addr_reports_a_survivor_and_starts_nothing() {
    let (root, data, dir) = fixture_root();
    let mut fixture = spawn_fixture(root.path(), &dir, &data, &[FIXTURE_IGNORE_TERM_ENV]);
    let mut target = target_in(&dir, Some(&data.clone().into_os_string()));
    // #11085: restart checks the binary before its stop; the stop error
    // below returns before any start.
    target.desc.install_dir = root.path().join("bin");
    target.desc.stop_grace = Duration::from_millis(300);
    target.desc.kill_wait = Duration::from_millis(500);

    let procs = own_procs(&root.path().join("proc"), &[fixture.id()]);
    let result = restart_report(restart_with(&target, &DropKill, &procs).await);
    let alive = fixture.try_wait().expect("try_wait").is_none();
    let _ = fixture.kill();
    let _ = fixture.wait();
    let shown = format!("{:#}", result.expect_err("a survivor fails the restart"));
    assert!(shown.contains(&fixture.id().to_string()), "{shown}");
    assert!(shown.contains("survived SIGKILL"), "{shown}");
    assert!(alive, "the fixture survived, as the seam made it");
}

// AC11 / P-b: `stop` on a stopped console exits 1; `restart` reaches start.
// The spawned fixture finds no fixture dir in its cleared environment and
// exits, so the start fails after its spawn.
#[tokio::test(flavor = "multi_thread")]
async fn stop_on_nothing_running_exits_1_and_restart_reaches_start() {
    let (root, data, dir) = fixture_root();
    let mut target = target_in(&dir, Some(&data.into_os_string()));
    target.desc.install_dir = install_fixture(root.path())
        .parent()
        .expect("bin dir")
        .to_path_buf();
    let args = [
        "linux_lifecycle::tests::lifecycle_fixture_entry",
        "serve",
        "--exact",
    ];
    target.desc.spawn_args = args.map(String::from).to_vec();

    let procs = own_procs(&root.path().join("proc"), &[]);
    let stop = stop_report(stop_with(&target, &KernelDelivery, &procs).await).expect_err("exit 1");
    assert!(format!("{stop:#}").contains("No trusty-console running"));
    let restart = restart_report(restart_with(&target, &KernelDelivery, &procs).await);
    let shown = format!("{:#}", restart.expect_err("the fixture exits at boot"));
    assert!(
        shown.contains("exited before serving the socket"),
        "restart reached start: {shown}"
    );
}
