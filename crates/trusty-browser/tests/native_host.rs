//! The `native-host` relay (`native_host.py` `bridge`): framing and refusals.

mod common;

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::thread;

use common::state_dir;
use trusty_browser::native_host::{connect, relay, transfer};
use trusty_browser::peer_darwin::ORIGIN;

fn framed(body: &[u8]) -> Vec<u8> {
    [(body.len() as u32).to_ne_bytes().to_vec(), body.to_vec()].concat()
}

#[test]
fn relay_copies_frames_both_ways() {
    let dir = state_dir();
    let path = dir.path().join("service.sock");
    let listener = UnixListener::bind(&path).expect("bind");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");
    let server = thread::spawn(move || {
        let (mut conn, _) = listener.accept().expect("accept");
        let mut got = vec![0u8; 4 + 7];
        conn.read_exact(&mut got).expect("request");
        conn.write_all(&framed(br#"{"ok":1}"#)).expect("reply");
        got
    });
    let service = connect(&path, ORIGIN).expect("connect");
    let (chrome_in, mut chrome_side) = UnixStream::pair().expect("pair");
    let (chrome_out, mut chrome_read) = UnixStream::pair().expect("pair");
    let relayed = thread::spawn(move || relay(service, chrome_in, chrome_out));
    chrome_side
        .write_all(&framed(br#"{"a":1}"#))
        .expect("write");
    let mut reply = vec![0u8; 4 + 8];
    chrome_read.read_exact(&mut reply).expect("read");
    assert_eq!(reply, framed(br#"{"ok":1}"#));
    assert_eq!(server.join().expect("server"), framed(br#"{"a":1}"#));
    drop(chrome_side);
    assert!(
        relayed.join().expect("relay").is_ok(),
        "clean EOF ends the relay"
    );
}

#[test]
fn transfer_refuses_bad_lengths_and_truncation() {
    let mut sink = Vec::new();
    assert!(
        transfer(&[0u8, 0, 0, 0][..], &mut sink).is_err(),
        "length 0"
    );
    let huge = ((1024 * 1024 + 1) as u32).to_ne_bytes();
    assert!(transfer(&huge[..], &mut sink).is_err(), "over 1 MiB");
    assert!(
        transfer(&framed(b"{}")[..3], &mut sink).is_err(),
        "torn header"
    );
    assert!(
        transfer(&framed(b"{}")[..5], &mut sink).is_err(),
        "torn body"
    );
    assert!(sink.is_empty());
}

#[test]
fn relay_refuses_bad_origin_and_shared_socket() {
    let dir = state_dir();
    let path = dir.path().join("service.sock");
    let _listener = UnixListener::bind(&path).expect("bind");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o660)).expect("chmod");
    let err = connect(&path, ORIGIN).err().map(|e| e.to_string());
    assert_eq!(
        err.as_deref(),
        Some("expected a private user-owned service socket")
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");
    let err = connect(&path, "chrome-extension://other/")
        .err()
        .map(|e| e.to_string());
    assert_eq!(err.as_deref(), Some("unexpected Chrome extension origin"));
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o750)).expect("chmod");
    assert!(connect(&path, ORIGIN).is_err(), "shared state directory");
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).expect("chmod");
}

#[test]
fn enrollment_socket_refuses_a_broker_that_is_not_this_binary() {
    // A plain listener in this test process is not `<exe> --state <dir> serve`.
    let dir = state_dir();
    let path = dir.path().join("enrollment.sock");
    let _listener = UnixListener::bind(&path).expect("bind");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");
    assert!(connect(&path, ORIGIN).is_err());
}
