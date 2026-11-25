//! Broker sockets: peer uid check, private modes, one broker per state dir.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use common::rig::Rig;
use trusty_computer::broker_server::{BrokerServer, serve_connection};
use trusty_computer::{peer, private};

#[test]
fn same_user_socketpair_passes_peer_auth() {
    let (a, b) = UnixStream::pair().expect("socketpair");
    assert_eq!(peer::peer_uid(&a).ok(), Some(private::uid()));
    assert_eq!(peer::verify_peer(&b), Ok(private::uid()));
}

#[test]
fn foreign_or_unreadable_peer_is_refused() {
    let me = private::uid();
    assert_eq!(peer::admit(Ok(me), me), Ok(me));
    let foreign = peer::admit(Ok(me.wrapping_add(1)), me).expect_err("foreign");
    assert_eq!(foreign.code, "foreign_uid");
    let unread = peer::admit(Err(std::io::Error::other("no cred")), me).expect_err("unread");
    assert_eq!(unread.code, "peer_unverified");
}

#[test]
fn foreign_uid_connection_is_refused_and_audited() {
    let rig = Rig::new();
    let (mut client, server) = UnixStream::pair().expect("socketpair");
    let broker = std::sync::Arc::clone(&rig.broker);
    let other = private::uid().wrapping_add(1);
    let worker = std::thread::spawn(move || serve_connection(&broker, server, other, false));
    let req = common::rig::call(&rig.grant.id, "list_apps", "x", serde_json::json!({}));
    writeln!(client, "{req}").expect("send");
    // EOF after one line, so an admitted (fail-open) worker also returns.
    client
        .shutdown(std::net::Shutdown::Write)
        .expect("shutdown");
    let mut line = String::new();
    BufReader::new(&client).read_line(&mut line).expect("read");
    worker.join().expect("worker");
    let reply: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert_eq!(reply["code"], "foreign_uid", "{reply}");
    assert_eq!(rig.audit().last().expect("line")["refused"], "foreign_uid");
    assert!(rig.agent_calls().is_empty());
}

#[test]
fn second_broker_on_a_state_dir_is_refused() {
    let rig = Rig::new();
    let err = BrokerServer::bind(&rig.state()).err().expect("refused");
    assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
}
