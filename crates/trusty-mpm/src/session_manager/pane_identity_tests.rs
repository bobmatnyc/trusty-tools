//! Tests for #9004's pane identity: the strict parse and the capture.

use super::{PaneIdentity, capture};
use crate::session_manager::{ManagedError, ManagedTmuxDriver};

#[test]
fn a_well_formed_identity_line_parses() {
    let id = PaneIdentity::parse("%3", "%3|$2|4242:1790954333|tm-p1:x\n").expect("parses");
    assert_eq!(
        id,
        PaneIdentity {
            pane_id: "%3".into(),
            session_id: "$2".into(),
            server: "4242:1790954333".into(),
            // Only the last field is free text, so a `|` in it survives.
            session_name: "tm-p1:x".into(),
        }
    );
    let piped = PaneIdentity::parse("%3", "%3|$2|1:2|a|b").expect("parses");
    assert_eq!(piped.session_name, "a|b");
}

/// tmux 3.6b answers `display-message -t %999` with exit 0 and empty fields;
/// that, and every other malformed line, is an `Err`, never an identity.
#[test]
fn a_missing_pane_answer_is_rejected() {
    for line in [
        "||94895:1790954333|",
        "",
        "%4|$2|4242:1790954333|s",
        "%3||4242:1790954333|s",
        "%3|2|4242:1790954333|s",
        "%3|$x|4242:1790954333|s",
        "%3|$2|4242|s",
        "%3|$2|:1790954333|s",
        "%3|$2|4242:|s",
        "%3|$2|4242:1790954333|",
        "%3|$2|4242:1790954333",
    ] {
        assert!(
            PaneIdentity::parse("%3", line).is_err(),
            "{line:?} parsed as an identity"
        );
    }
}

/// A driver that answers only the two capture reads.
struct CaptureDriver {
    pane: Option<&'static str>,
    identity: Option<&'static str>,
}

impl ManagedTmuxDriver for CaptureDriver {
    fn create_session(&self, _: &str, _: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn kill_session(&self, _: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn send_line(&self, _: &str, _: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn capture(&self, _: &str, _: usize) -> Result<String, ManagedError> {
        Ok(String::new())
    }
    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(Vec::new())
    }
    fn get_pane_id(&self, _: &str) -> Option<String> {
        self.pane.map(str::to_owned)
    }
    fn pane_identity(&self, pane_id: &str) -> Result<PaneIdentity, ManagedError> {
        let line = self
            .identity
            .ok_or_else(|| ManagedError::TmuxUnavailable("lost server".into()))?;
        PaneIdentity::parse(pane_id, line).map_err(ManagedError::TmuxUnavailable)
    }
}

#[test]
fn capture_pairs_the_pane_with_its_server() {
    let tmux = CaptureDriver {
        pane: Some("%7"),
        identity: Some("%7|$1|4242:1790954333|s"),
    };
    assert_eq!(
        capture(&tmux, "s"),
        (Some("%7".into()), Some("4242:1790954333".into()))
    );
    let none = CaptureDriver {
        pane: None,
        identity: Some("%7|$1|4242:1790954333|s"),
    };
    assert_eq!(capture(&none, "s"), (None, None));
}

/// A failed or malformed server read stores the pane with no server, so the
/// record fails closed rather than carrying a server it never proved. So does
/// an identity naming another session: the server restarted between the pane
/// id read and the identity read, and `%7` is now another session's pane.
#[test]
fn capture_without_a_readable_server_stores_no_server() {
    for identity in [
        None,
        Some("||4242:1790954333|"),
        Some("%7|$1|5151:1790990000|tm-other"),
    ] {
        let tmux = CaptureDriver {
            pane: Some("%7"),
            identity,
        };
        assert_eq!(
            capture(&tmux, "s"),
            (Some("%7".into()), None),
            "{identity:?}"
        );
    }
}

/// The real driver against a private `-L` tmux server: the capture pairs the
/// pane with that server's `<pid>:<start_time>`, and a missing pane — which
/// tmux answers with exit 0 and empty fields — is an `Err`.
#[serial_test::serial]
#[test]
fn the_real_driver_reads_a_live_pane_identity() {
    use crate::test_support::tmux_session::{
        PrivateTmuxServer, ScratchTmuxSession, reserved_session_name,
    };
    if !ScratchTmuxSession::tmux_available("tmux") {
        eprintln!("tmux not available; skipping");
        return;
    }
    let server = PrivateTmuxServer::new("tmux", "9004-identity");
    let name = reserved_session_name("9004-identity");
    let _session =
        ScratchTmuxSession::spawn_on_socket("tmux", Some(server.name()), &name, "sleep 600");
    let driver = crate::session_manager::RealTmuxDriver::from_driver_for_test(
        crate::daemon::tmux::TmuxDriver::with_tmux_path_for_test(server.shim_bin()),
    );
    let expected = server.query(&[
        "display-message",
        "-p",
        "-t",
        &trusty_common::tmux::exact_window_target(&name),
        "#{pane_id} #{pid}:#{start_time}",
    ]);
    let (pane, live_server) = capture(&driver, &name);

    let pane = pane.expect("the live session has a pane");
    assert_eq!(
        Some(format!("{pane} {}", live_server.as_deref().unwrap_or("-"))),
        expected
    );
    let identity = driver.pane_identity(&pane).expect("a live pane's identity");
    assert_eq!(identity.session_name, name);
    assert!(identity.session_id.starts_with('$'), "{identity:?}");
    assert!(driver.pane_identity("%999999").is_err());
}
