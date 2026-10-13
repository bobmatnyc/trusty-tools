//! The tier-3 client against fake servers and real pipes (#9071).
//!
//! Every socket is under a `TempDir`; no real server, home or Keychain is
//! touched. A fake server accepts one connection, reads the request to the
//! client's half-close, and answers with fixed bytes or stays silent.
//!
//! Test: itself.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, ErrorKind, Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;

use super::token::{parse_token_fd, take_inherited};
use super::wire::MAX_FRAME_BYTES;
use super::*;

/// A well-formed token whose text appears nowhere else.
const TOKEN: &str = "9071c0ffee12c0ffee12c0ffee12c0ffee12c0ffee12c0ffee12c0ffee12abcd";
/// A value whose text appears nowhere else.
const VALUE: &str = "granted-9071-value-Wq7Zr3";

fn key(name: &str) -> SecretKey {
    SecretKey::new(name).unwrap()
}

/// A pipe holding `bytes`, its write end closed; the read end.
fn pipe_with(bytes: &[u8]) -> OwnedFd {
    let (reader, mut writer) = io::pipe().unwrap();
    writer.write_all(bytes).unwrap();
    drop(writer);
    OwnedFd::from(reader)
}

/// An unlinked regular file holding `bytes`, open read-only: the shape of
/// the fd tm's exec stub hands the command (#9071 S10).
fn token_file(bytes: &[u8]) -> OwnedFd {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("token");
    fs::write(&path, bytes).unwrap();
    let file = fs::File::open(&path).unwrap();
    fs::remove_file(&path).unwrap();
    OwnedFd::from(file)
}

fn client_for(socket: &Path) -> GrantedClient {
    GrantedClient::from_token_fd(pipe_with(TOKEN.as_bytes()))
        .unwrap()
        .with_socket(socket)
}

/// A socket directory at `dir_mode` holding a listener at `sock_mode`.
struct Spot {
    tmp: TempDir,
    socket: PathBuf,
    listener: UnixListener,
}

fn spot(dir_mode: u32, sock_mode: u32) -> Spot {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("run");
    fs::create_dir(&dir).unwrap();
    let socket = dir.join("s.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(sock_mode)).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(dir_mode)).unwrap();
    Spot {
        tmp,
        socket,
        listener,
    }
}

impl Spot {
    /// How many clients connected so far.
    fn connections(&self) -> usize {
        self.listener.set_nonblocking(true).unwrap();
        let mut count = 0;
        loop {
            match self.listener.accept() {
                Ok(_) => count += 1,
                Err(e) if e.kind() == ErrorKind::WouldBlock => return count,
                Err(e) => panic!("{e}"),
            }
        }
    }
}

/// How a fake server answers its one connection.
enum Reply {
    /// These bytes, then close.
    Bytes(Vec<u8>),
    /// Nothing, holding the connection open this long.
    Silent(Duration),
    /// One byte every 50 ms, never a newline, for this long.
    Trickle(Duration),
}

/// Serve one connection on a trusted spot; the thread returns the request.
fn serve(reply: Reply) -> (Spot, JoinHandle<Vec<u8>>) {
    let spot = spot(0o700, 0o600);
    let listener = spot.listener.try_clone().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut request = Vec::new();
        let _ = stream.read_to_end(&mut request);
        match reply {
            Reply::Bytes(bytes) => {
                let _ = stream.write_all(&bytes);
            }
            Reply::Silent(hold) => std::thread::sleep(hold),
            Reply::Trickle(hold) => {
                let started = Instant::now();
                while started.elapsed() < hold && stream.write_all(b" ").is_ok() {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
        request
    });
    (spot, server)
}

/// `body` as one newline-terminated frame.
fn frame(body: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(body).unwrap();
    bytes.push(b'\n');
    bytes
}

/// A successful answer naming `name`.
fn ok_answer(name: &str, value: &str) -> Vec<u8> {
    frame(&json!({"jsonrpc": "2.0", "id": 1, "result": {"key": name, "value": value}}))
}

/// The error a fake server answering `bytes` provokes.
fn error_for(bytes: Vec<u8>) -> GrantedError {
    let (spot, _server) = serve(Reply::Bytes(bytes));
    client_for(&spot.socket)
        .resolve(&key("API_KEY"))
        .unwrap_err()
}

/// Why: AC1 at the unit level — one frame, half-closed, carrying the
/// method, the token and the key; the answered value comes back.
/// Test: itself.
#[test]
fn granted_client_sends_one_resolve_frame() {
    let (spot, server) = serve(Reply::Bytes(ok_answer("API_KEY", VALUE)));
    let got = client_for(&spot.socket).resolve(&key("API_KEY")).unwrap();
    assert_eq!(got.expose(), VALUE);
    let request = server.join().unwrap();
    assert_eq!(request.last(), Some(&b'\n'));
    assert_eq!(request.iter().filter(|b| **b == b'\n').count(), 1);
    let request: Value = serde_json::from_slice(&request).unwrap();
    assert_eq!(
        request,
        json!({"jsonrpc": "2.0", "id": 1, "method": "secrets.resolve",
            "params": {"token": TOKEN, "key": "API_KEY"}})
    );
}

/// Why: AC2 — with nothing serving, the answer is `Unavailable` at once,
/// and the client creates no socket and starts no server: the path is
/// still absent afterwards. A stale socket file is dialed, refused, and
/// left as it was.
/// Test: itself.
#[test]
fn granted_client_never_spawns_a_server() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("run");
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = dir.join("s.sock");
    let started = Instant::now();
    let error = client_for(&socket).resolve(&key("API_KEY")).unwrap_err();
    assert!(
        matches!(&error, GrantedError::Unavailable { path, .. } if *path == socket),
        "{error:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    // A spawned server would bind within this window.
    std::thread::sleep(Duration::from_millis(300));
    assert!(socket.symlink_metadata().is_err(), "a socket appeared");

    let missing = tmp.path().join("absent").join("s.sock");
    let error = client_for(&missing).resolve(&key("API_KEY")).unwrap_err();
    assert!(
        matches!(error, GrantedError::Unavailable { .. }),
        "{error:?}"
    );
    assert!(!tmp.path().join("absent").exists(), "a directory appeared");

    let Spot {
        tmp: _keep,
        socket: stale,
        listener,
    } = spot(0o700, 0o600);
    drop(listener);
    // A concurrent test's fork can hold the listener until its exec; wait
    // until the socket refuses before the client dials it.
    let started = Instant::now();
    while !matches!(
        std::os::unix::net::UnixStream::connect(&stale),
        Err(e) if e.kind() == ErrorKind::ConnectionRefused
    ) {
        assert!(started.elapsed() < Duration::from_secs(5), "never refused");
        std::thread::sleep(Duration::from_millis(20));
    }
    let error = client_for(&stale).resolve(&key("API_KEY")).unwrap_err();
    assert!(
        matches!(error, GrantedError::Unavailable { .. }),
        "{error:?}"
    );
    assert!(
        std::os::unix::fs::FileTypeExt::is_socket(&stale.symlink_metadata().unwrap().file_type()),
        "the stale socket was replaced"
    );
}

/// Why: AC5, and S10 AC2 — a token file holding anything but 64 lowercase
/// hex digits (63 or 65 bytes included) is refused when it is read, before
/// any client exists, so nothing is dialed: the listener sees no connection.
/// Red when the reader trims or truncates to 64 bytes.
/// Test: itself.
#[test]
fn granted_client_checks_the_token_before_dialing() {
    let spot = spot(0o700, 0o600);
    let bad: Vec<Vec<u8>> = vec![
        Vec::new(),
        TOKEN.as_bytes()[..63].to_vec(),
        format!("{TOKEN}0").into_bytes(),
        TOKEN.to_uppercase().into_bytes(),
        format!("{TOKEN}\n").into_bytes(),
        format!(" {}", &TOKEN[1..]).into_bytes(),
        format!("{}g", &TOKEN[..63]).into_bytes(),
        vec![b'a'; 300],
        vec![0xff; 64],
    ];
    for bytes in bad {
        let built =
            GrantedClient::from_token_fd(token_file(&bytes)).map(|c| c.with_socket(&spot.socket));
        assert!(
            matches!(built, Err(GrantedError::InvalidToken)),
            "{} bytes: {built:?}",
            bytes.len()
        );
    }
    assert_eq!(spot.connections(), 0);
}

/// Why: S10 AC2 and AC3 — only a read-only, unlinked regular file is a
/// token fd. A pipe (the S9 shape, which a read drains), a named file, a
/// writable fd, a directory, a socket and a device are `TokenUnreadable`
/// at once, before anything is dialed. Red when the reader accepts a FIFO,
/// a named file or a writable fd.
/// Test: itself.
#[test]
fn granted_client_refuses_a_token_fd_of_the_wrong_shape() {
    let spot = spot(0o700, 0o600);
    let tmp = TempDir::new().unwrap();
    let named = tmp.path().join("named");
    fs::write(&named, TOKEN).unwrap();
    // Unlinked, 64 bytes, regular: only its read-write mode is wrong.
    let mut writable = tempfile::tempfile().unwrap();
    writable.write_all(TOKEN.as_bytes()).unwrap();
    let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
    let (held, held_writer) = io::pipe().unwrap();
    let cases: Vec<(&str, OwnedFd)> = vec![
        ("a pipe holding a token", pipe_with(TOKEN.as_bytes())),
        ("an empty pipe with an open writer", OwnedFd::from(held)),
        (
            "a named file",
            OwnedFd::from(fs::File::open(&named).unwrap()),
        ),
        ("a read-write file", OwnedFd::from(writable)),
        (
            "a directory",
            OwnedFd::from(fs::File::open(tmp.path()).unwrap()),
        ),
        ("a socket", OwnedFd::from(socket)),
        (
            "/dev/null",
            OwnedFd::from(fs::File::open("/dev/null").unwrap()),
        ),
    ];
    for (what, fd) in cases {
        let started = Instant::now();
        let built = GrantedClient::from_token_fd(fd).map(|c| c.with_socket(&spot.socket));
        assert!(
            matches!(built, Err(GrantedError::TokenUnreadable(_))),
            "{what}: {built:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{what}: the read blocked"
        );
    }
    drop(held_writer);
    assert_eq!(spot.connections(), 0);
}

/// Why: S10 AC4 — the token is read at offset 0 and the shared file offset
/// is left alone: a reader whose offset another process moved (to 10, or to
/// the end) still reads the token, and moves nothing for the next reader.
/// Red when the reader uses a plain `read`.
/// Test: itself.
#[test]
fn granted_client_reads_the_token_at_any_offset() {
    let mut file = fs::File::from(token_file(TOKEN.as_bytes()));
    for offset in [SeekFrom::Start(10), SeekFrom::End(0)] {
        let at = file.seek(offset).unwrap();
        let token = take_inherited(file.as_raw_fd()).unwrap();
        assert_eq!(token.expose(), TOKEN);
        assert_eq!(file.stream_position().unwrap(), at, "the read moved it");
    }
}

/// Why: AC5 — only a decimal fd number above stderr names a token pipe;
/// anything else is `NoGrant`.
/// Test: itself.
#[test]
fn parse_token_fd_refuses_unset_garbage_and_stdio() {
    assert!(matches!(parse_token_fd(None), Err(GrantedError::NoGrant)));
    for raw in [
        "",
        "abc",
        "-1",
        "+5",
        " 5",
        "5 ",
        "0",
        "1",
        "2",
        "3.0",
        "0x10",
        "99999999999",
    ] {
        assert!(
            matches!(
                parse_token_fd(Some(OsStr::new(raw))),
                Err(GrantedError::NoGrant)
            ),
            "{raw:?}"
        );
    }
    let not_utf8 = OsStr::from_bytes(b"1\xff");
    assert!(matches!(
        parse_token_fd(Some(not_utf8)),
        Err(GrantedError::NoGrant)
    ));
    assert_eq!(parse_token_fd(Some(OsStr::new("3"))).unwrap(), 3);
    assert_eq!(parse_token_fd(Some(OsStr::new("17"))).unwrap(), 17);
}

/// Why: Fail-Open Check — an fd that is not open, not a pipe, or a pipe
/// whose open writer has sent nothing or half a token is `TokenUnreadable`
/// at once; no read blocks. A whole token behind an open writer is read.
/// Test: itself.
#[test]
fn granted_client_token_fd_errors_never_block() {
    let file = tempfile::tempfile().unwrap();
    let error = GrantedClient::from_token_fd(OwnedFd::from(file)).unwrap_err();
    assert!(
        matches!(error, GrantedError::TokenUnreadable(_)),
        "{error:?}"
    );

    let (reader, writer) = io::pipe().unwrap();
    let started = Instant::now();
    let error = GrantedClient::from_token_fd(OwnedFd::from(reader)).unwrap_err();
    assert!(
        matches!(error, GrantedError::TokenUnreadable(_)),
        "{error:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the read blocked"
    );
    drop(writer);

    // A whole token with the writer still open (a concurrent fork can hold
    // a copy until its exec) is read without waiting for EOF; half a token
    // is not.
    let (reader, mut writer) = io::pipe().unwrap();
    writer.write_all(TOKEN.as_bytes()).unwrap();
    let client = GrantedClient::from_token_fd(OwnedFd::from(reader)).unwrap();
    assert!(format!("{client:?}").contains("<redacted>"));
    let (reader, mut held) = io::pipe().unwrap();
    held.write_all(&TOKEN.as_bytes()[..32]).unwrap();
    let error = GrantedClient::from_token_fd(OwnedFd::from(reader)).unwrap_err();
    assert!(
        matches!(error, GrantedError::TokenUnreadable(_)),
        "{error:?}"
    );
    drop((writer, held));

    // No process holds this many descriptors, so the number is not open.
    let error = take_inherited(RawFd::MAX).unwrap_err();
    assert!(
        matches!(error, GrantedError::TokenUnreadable(_)),
        "{error:?}"
    );
}

/// Why: Fail-Open Check — an unset or empty `$HOME` is an error before the
/// token fd is touched; a set `$HOME` with no fd variable is `NoGrant`.
/// Test: itself.
#[test]
fn granted_env_without_home_is_an_error() {
    for home in [None, Some(OsString::new())] {
        let error = GrantedClient::from_env_parts(home, Some(OsStr::new("3"))).unwrap_err();
        assert!(matches!(error, GrantedError::HomeUnavailable), "{error:?}");
    }
    let error = GrantedClient::from_env_parts(Some("/home/x".into()), None).unwrap_err();
    assert!(matches!(error, GrantedError::NoGrant), "{error:?}");
}

/// Why: AC6 — a socket whose directory is not 0700, which is not 0600, is
/// a symlink, is not a socket, or sits in a symlinked directory is refused
/// before a byte is written: the listener sees no connection.
/// Test: itself.
#[test]
fn granted_client_refuses_an_untrusted_socket() {
    for (dir_mode, sock_mode, reason) in [(0o755, 0o600, "0700"), (0o700, 0o666, "0600")] {
        let spot = spot(dir_mode, sock_mode);
        let error = client_for(&spot.socket)
            .resolve(&key("API_KEY"))
            .unwrap_err();
        assert!(
            matches!(&error, GrantedError::UntrustedSocket { reason: r, .. } if r.contains(reason)),
            "{error:?}"
        );
        assert_eq!(spot.connections(), 0);
    }

    let spot = spot(0o700, 0o600);
    let link = spot.socket.with_file_name("link.sock");
    symlink(&spot.socket, &link).unwrap();
    let plain = spot.socket.with_file_name("plain");
    fs::write(&plain, b"").unwrap();
    fs::set_permissions(&plain, fs::Permissions::from_mode(0o600)).unwrap();
    let linked_dir = spot.tmp.path().join("linked");
    symlink(spot.socket.parent().unwrap(), &linked_dir).unwrap();
    for (path, reason) in [
        (link, "symlink"),
        (plain, "not a socket"),
        (linked_dir.join("s.sock"), "symlink"),
    ] {
        let error = client_for(&path).resolve(&key("API_KEY")).unwrap_err();
        assert!(
            matches!(&error, GrantedError::UntrustedSocket { reason: r, .. } if r.contains(reason)),
            "{}: {error:?}",
            path.display()
        );
    }
    assert_eq!(spot.connections(), 0);
}

/// Why: AC7 — the wait bounds the whole call, not each read: a silent
/// server and one that trickles bytes both end in `Timeout` within twice
/// the budget.
/// Test: itself.
#[test]
fn granted_client_times_out_on_a_silent_server() {
    let budget = Duration::from_millis(300);
    for reply in [
        Reply::Silent(Duration::from_secs(5)),
        Reply::Trickle(Duration::from_secs(5)),
    ] {
        let (spot, _server) = serve(reply);
        let started = Instant::now();
        let error = client_for(&spot.socket)
            .with_wait(budget)
            .resolve(&key("API_KEY"))
            .unwrap_err();
        let took = started.elapsed();
        assert!(matches!(error, GrantedError::Timeout), "{error:?}");
        assert!(took >= budget && took < budget * 2, "{took:?}");
    }
}

/// Why: AC4 — an answer for another key is refused and its value dropped.
/// Test: itself.
#[test]
fn granted_client_rejects_an_answer_for_another_key() {
    let error = error_for(ok_answer("OTHER_KEY", VALUE));
    assert!(matches!(error, GrantedError::KeyMismatch), "{error:?}");
    assert!(!format!("{error} {error:?}").contains(VALUE));
}

/// Why: the answer is read under the server's 8 MiB cap; an unterminated
/// frame past it is a transport error, never an unbounded read.
/// Test: itself.
#[test]
fn granted_client_rejects_an_unterminated_oversized_frame() {
    let error = error_for(vec![b'a'; MAX_FRAME_BYTES + 1]);
    assert!(
        matches!(&error, GrantedError::Transport(e) if e.kind() == ErrorKind::InvalidData),
        "{error:?}"
    );
}

/// Why: AC4 — an answer with no result, a `null` result, or no bytes at
/// all is an error, never an empty value.
/// Test: itself.
#[test]
fn granted_client_empty_response_is_an_error() {
    for body in [
        json!({"jsonrpc": "2.0", "id": 1}),
        json!({"jsonrpc": "2.0", "id": 1, "result": null}),
    ] {
        let error = error_for(frame(&body));
        assert!(
            matches!(error, GrantedError::EmptyResponse),
            "{body}: {error:?}"
        );
    }
    let error = error_for(Vec::new());
    assert!(
        matches!(&error, GrantedError::Transport(e) if e.kind() == ErrorKind::UnexpectedEof),
        "{error:?}"
    );
}

/// Why: Fail-Open Check — an answer that is not JSON, names another
/// request id, or carries a result that does not decode is `Decode`.
/// Test: itself.
#[test]
fn granted_client_decode_failure_is_an_error() {
    let answers = [
        b"not json\n".to_vec(),
        frame(&json!({"jsonrpc": "2.0", "id": 2, "result": {"key": "API_KEY", "value": VALUE}})),
        frame(&json!({"jsonrpc": "2.0", "id": 1, "result": {"key": "API_KEY"}})),
        frame(&json!({"jsonrpc": "2.0", "id": 1, "result": {"key": "bad key!", "value": VALUE}})),
    ];
    for bytes in answers {
        let error = error_for(bytes);
        assert!(matches!(error, GrantedError::Decode), "{error:?}");
    }
}

/// Why: AC4 — any server error is a `Refusal` carrying the wire code, the
/// fixed message and the kind when present; an error beside a result still
/// refuses.
/// Test: itself.
#[test]
fn granted_client_server_error_is_a_refusal() {
    let internal = frame(&json!({"jsonrpc": "2.0", "id": 1,
        "error": {"code": -32603, "message": "internal error"}}));
    match error_for(internal) {
        GrantedError::Refused(refusal) => {
            assert_eq!(refusal.code, -32603);
            assert_eq!(refusal.message, "internal error");
            assert_eq!(refusal.kind, None);
            assert_eq!(refusal.to_string(), "[-32603] internal error");
        }
        other => panic!("{other:?}"),
    }
    let both = frame(&json!({"jsonrpc": "2.0", "id": 1,
        "result": {"key": "API_KEY", "value": VALUE},
        "error": {"code": -32082, "message": "refused", "data": {"kind": "grant_refused"}}}));
    match error_for(both) {
        GrantedError::Refused(refusal) => {
            assert_eq!(refusal.kind.as_deref(), Some("grant_refused"));
        }
        other => panic!("{other:?}"),
    }
}

/// Why: AC8 — no `Debug` or error text shows the token or a value, even
/// when the server's answer carries both.
/// Test: itself.
#[test]
fn granted_client_debug_and_errors_never_show_token_or_value() {
    let client = client_for(Path::new("/nonexistent/run/s.sock"));
    let debug = format!("{client:?}");
    assert!(
        !debug.contains(TOKEN) && debug.contains("<redacted>"),
        "{debug}"
    );
    let answers = [
        ok_answer("OTHER_KEY", VALUE),
        frame(&json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32082,
            "message": "refused", "data": {"kind": "grant_refused", "token": TOKEN, "value": VALUE}}})),
        format!("{{\"value\": \"{VALUE}\", {TOKEN}\n").into_bytes(),
        frame(
            &json!({"jsonrpc": "2.0", "id": 1, "result": {"key": 7, "value": VALUE, "t": TOKEN}}),
        ),
    ];
    for bytes in answers {
        let error = error_for(bytes);
        let source = std::error::Error::source(&error).map(ToString::to_string);
        let text = format!("{error} {error:?} {source:?}");
        assert!(!text.contains(TOKEN) && !text.contains(VALUE), "{text}");
    }
}

/// Why: AC3 and ruling 34 — the module can resolve and nothing else: no
/// grant, revoke, list or set method, no spawn, no supervisor. The scan
/// must see the real dial site and method name, so it cannot pass on an
/// empty or stub module.
/// Test: itself.
#[test]
fn granted_module_has_no_grant_or_spawn_entry_point() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/granted");
    let mut production = String::new();
    let mut files = Vec::new();
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name.ends_with(".rs") && !name.ends_with("_tests.rs") {
            production.push_str(&fs::read_to_string(&path).unwrap());
            files.push(name);
        }
    }
    for anchor in ["UnixStream::connect", "method::RESOLVE"] {
        assert!(production.contains(anchor), "{anchor} not in {files:?}");
    }
    for forbidden in [
        "method::GRANT",
        "method::REVOKE",
        "method::SET",
        "method::LIST",
        "method::COPY",
        "method::DELETE",
        "\"secrets.grant\"",
        "\"secrets.revoke\"",
        "SpawnSpec",
        "Command::new",
        "OnDemandSecrets",
        "UdsServiceSupervisor",
        "trusty_common::",
        "tokio::",
    ] {
        assert!(!production.contains(forbidden), "{forbidden} in {files:?}");
    }
}

/// Why: D3, DOC-74 §10.5 — reading the token puts it back on the same fd
/// number, not close-on-exec. A second reader of that fd (here this
/// process again, as a grandchild would) gets the token and re-arms it in
/// turn, and a child spawned afterwards reads the same token.
/// Test: itself.
#[test]
fn granted_client_rearms_the_token_for_a_child() {
    let fd = pipe_with(TOKEN.as_bytes()).into_raw_fd();
    assert_eq!(take_inherited(fd).unwrap().expose(), TOKEN);
    assert_eq!(take_inherited(fd).unwrap().expose(), TOKEN);
    // SAFETY: `F_GETFD` reads the flags of an fd this test owns.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert!(flags >= 0 && flags & libc::FD_CLOEXEC == 0, "flags {flags}");
    let out = Command::new("cat")
        .arg(format!("/dev/fd/{fd}"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(out.stdout, TOKEN.as_bytes(), "the child read another token");
    // SAFETY: `fd` is this test's own descriptor and nothing else owns it.
    drop(unsafe { OwnedFd::from_raw_fd(fd) });
}

/// Set in a re-run of this test binary: the socket a sibling dials.
const SIBLING_SOCKET: &str = "TS_GRANTED_SIBLING_SOCKET";

/// Serve one connection per answer, in turn, on a trusted spot; the thread
/// returns every request.
fn serve_each(answers: Vec<Vec<u8>>) -> (Spot, JoinHandle<Vec<Vec<u8>>>) {
    let spot = spot(0o700, 0o600);
    let listener = spot.listener.try_clone().unwrap();
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for answer in answers {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut request = Vec::new();
            let _ = stream.read_to_end(&mut request);
            let _ = stream.write_all(&answer);
            requests.push(request);
        }
        requests
    });
    (spot, server)
}

/// Why: S10 AC1, inverting S9's `granted_client_sibling_finds_the_pipe_drained`
/// — two sibling processes, one after the other, inherit one token fd from
/// their parent (as `a && b` does) and both resolve. Red under any
/// consuming read: the first sibling leaves the second nothing.
/// What: re-runs this test binary as each sibling; a sibling builds its
/// client from the inherited fd number and resolves against a fake server.
/// Test: itself.
#[test]
fn granted_client_siblings_both_resolve() {
    if let Some(socket) = std::env::var_os(SIBLING_SOCKET) {
        let fd = std::env::var_os(crate::api::TOKEN_FD_ENV);
        let client = GrantedClient::from_env_parts(Some("/nonexistent".into()), fd.as_deref())
            .expect("the sibling read the token")
            .with_socket(PathBuf::from(socket));
        let value = client
            .resolve(&key("API_KEY"))
            .expect("the sibling resolved");
        assert_eq!(value.expose(), VALUE);
        return;
    }
    let (spot, server) = serve_each(vec![ok_answer("API_KEY", VALUE); 2]);
    let fd = token_file(TOKEN.as_bytes());
    // SAFETY: `F_SETFD` on an fd this test owns, so the siblings inherit it.
    assert_eq!(unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, 0) }, 0);
    let (_, path) = module_path!().split_once("::").unwrap();
    let name = format!("{path}::granted_client_siblings_both_resolve");
    for sibling in 0..2 {
        let out = Command::new(std::env::current_exe().unwrap())
            .args([name.as_str(), "--exact", "--nocapture", "--test-threads=1"])
            .env(SIBLING_SOCKET, &spot.socket)
            .env(crate::api::TOKEN_FD_ENV, fd.as_raw_fd().to_string())
            .output()
            .unwrap();
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "sibling {sibling} did not resolve: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests {
        let request: Value = serde_json::from_slice(&request).unwrap();
        assert_eq!(request["params"]["token"], TOKEN);
    }
}

/// Why: Fail-Open Check — when the process-wide client failed to build,
/// every later call returns that first error, without reading the drained
/// pipe again.
/// Test: itself.
#[test]
fn granted_resolve_replays_the_first_init_error() {
    let cell = OnceLock::new();
    let mut calls = 0;
    let first = resolve_in(
        &cell,
        || {
            calls += 1;
            Err(GrantedError::TokenUnreadable(io::Error::from_raw_os_error(
                libc::EBADF,
            )))
        },
        &key("API_KEY"),
    )
    .unwrap_err();
    let second = resolve_in(
        &cell,
        || {
            calls += 1;
            Err(GrantedError::InvalidToken)
        },
        &key("API_KEY"),
    )
    .unwrap_err();
    assert_eq!(calls, 1);
    assert!(
        matches!(second, GrantedError::TokenUnreadable(_)),
        "{second:?}"
    );
    let source = |e: &GrantedError| std::error::Error::source(e).map(ToString::to_string);
    assert_eq!(first.to_string(), second.to_string());
    assert_eq!(source(&first), source(&second));
}

/// Why: the answer cap is trusty-common's frame cap, copied because `api`
/// links no trusty-common; a drift fails here.
/// Test: itself.
#[cfg(feature = "server")]
#[test]
fn granted_frame_cap_matches_the_uds_cap() {
    assert_eq!(
        u64::try_from(MAX_FRAME_BYTES).unwrap(),
        trusty_common::uds::MAX_FRAME_BYTES
    );
}

/// Why: the fixtures above rely on a well-formed token.
/// Test: itself.
#[test]
fn granted_test_token_is_well_formed() {
    assert_eq!(TOKEN.len(), 64);
    assert!(
        TOKEN
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
}
