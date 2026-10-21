//! Tests for the #8928 step 3 `sccache_socket` doctor row.

use std::path::PathBuf;

use super::*;

fn probed(version: Result<(u32, u32, u32), String>, state: SocketState) -> SccacheFacts {
    SccacheFacts::Probed {
        bin: PathBuf::from("/opt/homebrew/bin/sccache"),
        version,
        socket: PathBuf::from("/h/.trusty-mpm/sccache.sock"),
        state,
    }
}

#[test]
fn a_listening_server_is_ok_and_names_the_client_env() {
    let row = build_check(&probed(Ok((0, 17, 0)), SocketState::Listening));
    assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
    assert!(
        row.message
            .contains("SCCACHE_SERVER_UDS=/h/.trusty-mpm/sccache.sock")
    );
    assert!(row.message.contains("SCCACHE_CLIENT_SIDE=1"));
    assert_eq!(row.name, CHECK_NAME);
}

#[test]
fn not_requested_is_not_applicable() {
    let row = build_check(&SccacheFacts::NotRequested);
    assert_eq!(row.status, CheckStatus::Ok);
    assert!(row.message.contains("not applicable"), "{}", row.message);
}

#[test]
fn a_missing_or_stale_socket_or_binary_warns() {
    for facts in [
        probed(Ok((0, 17, 0)), SocketState::Missing),
        probed(Ok((0, 9, 0)), SocketState::Stale("refused".into())),
        SccacheFacts::NoBinary,
    ] {
        let row = build_check(&facts);
        assert_eq!(row.status, CheckStatus::Warn, "{facts:?}: {}", row.message);
    }
    let row = build_check(&probed(Ok((0, 17, 0)), SocketState::Missing));
    assert!(row.message.contains("tm restart"), "{}", row.message);
}

#[test]
fn an_sccache_without_uds_support_fails() {
    let row = build_check(&probed(Ok((0, 8, 2)), SocketState::Listening));
    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
    assert!(row.message.contains("0.9.0"), "{}", row.message);
}

/// The Fail-Open Check: a probe that learned nothing never reads healthy.
#[test]
fn what_cannot_be_read_is_unknown_never_ok() {
    for facts in [
        SccacheFacts::ConfigUnreadable("bad yaml".into()),
        probed(Err("timed out".into()), SocketState::Listening),
        probed(Ok((0, 17, 0)), SocketState::Unknown("not a socket".into())),
    ] {
        let row = build_check(&facts);
        assert_eq!(
            row.status,
            CheckStatus::Unknown,
            "{facts:?}: {}",
            row.message
        );
    }
}

#[test]
fn gather_reports_not_requested_for_an_empty_home() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert_eq!(gather(dir.path()), SccacheFacts::NotRequested);
    let config = trusty_common::crate_config::crate_config_path_at(
        dir.path(),
        crate::core::trusty_tools_config::CRATE_NAME,
    );
    std::fs::create_dir_all(config.parent().expect("parent")).expect("mkdir");
    std::fs::write(&config, "build: [unclosed\n").expect("write");
    assert!(matches!(
        gather(dir.path()),
        SccacheFacts::ConfigUnreadable(_)
    ));
}
