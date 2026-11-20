//! #9779: `tm architect serve`'s `tmux -C` clients against a fake tmux.
//!
//! The fake is a shell script in a tempdir; no tmux server is touched.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use trusty_architect::SystemTmux;
use trusty_architect::serve::{ControlClients, Event};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// A fake tmux: `list-sessions` prints `s1` and `poll` (or fails with a
/// `noserver` file); `has-session` fails with a `gone` file; `attach-session`
/// logs its argv and, with an `up` file, prints two notifications and
/// stays attached, else exits 1.
fn fake_tmux(dir: &Path) -> TestResult<SystemTmux> {
    let d = dir.display();
    let script = format!(
        "#!/bin/sh\n\
         case \"$*\" in\n\
         *list-sessions*) [ -f {d}/noserver ] && exit 1; printf 's1\\npoll\\n' ;;\n\
         *has-session*) [ -f {d}/gone ] && exit 1; exit 0 ;;\n\
         *attach-session*) echo \"$*\" >> {d}/attach.log\n\
           [ -f {d}/up ] || exit 1\n\
           echo '%session-changed $1 s1'; echo '%output %1 hi'; exec sleep 30 ;;\n\
         esac\n"
    );
    let path = dir.join("tmux");
    fs::write(&path, script)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
    Ok(SystemTmux::new().with_program(path))
}

/// The first event matching `want` within 3 s, if any.
fn expect(rx: &Receiver<Event>, want: impl Fn(&Event) -> bool) -> Option<Event> {
    let end = Instant::now() + Duration::from_secs(3);
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(e) if want(&e) => return Some(e),
            Ok(_) => {}
            Err(_) => return None,
        }
    }
    None
}

/// A notification from the client is an event; the poll session gets no
/// client; the client attaches read-only and ignores size.
#[test]
fn control_output_is_an_event() -> TestResult {
    let dir = tempfile::tempdir()?;
    fs::write(dir.path().join("up"), "")?;
    let (tx, rx) = channel();
    let mut clients = ControlClients::new(fake_tmux(dir.path())?, vec!["poll".into()], tx);
    assert_eq!(clients.reconcile()?, Vec::<String>::new());
    assert_eq!(clients.sessions(), ["s1"]);
    let got = expect(&rx, |e| matches!(e, Event::Changed(_)));
    assert_eq!(got, Some(Event::Changed("tmux:s1".into())));
    clients.shutdown();
    let argv = fs::read_to_string(dir.path().join("attach.log"))?;
    assert!(
        argv.contains("-C attach-session -f read-only,ignore-size -t =s1"),
        "{argv}"
    );
    Ok(())
}

/// A client that stays down past the alert time raises `tmux-s1`; when it
/// connects again the fault clears.
#[test]
fn control_down_alerts_then_clears() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (tx, rx) = channel();
    let mut clients = ControlClients::new(fake_tmux(dir.path())?, vec!["poll".into()], tx)
        .with_timing(Duration::from_millis(200), Duration::from_millis(50));
    clients.reconcile()?;
    let fault = expect(&rx, |e| matches!(e, Event::Fault { .. }));
    assert!(
        matches!(&fault, Some(Event::Fault { kind, .. }) if kind == "tmux-s1"),
        "{fault:?}"
    );
    fs::write(dir.path().join("up"), "")?;
    let clear = expect(&rx, |e| matches!(e, Event::Clear(_)));
    assert_eq!(clear, Some(Event::Clear("tmux-s1".into())));
    let attaches = fs::read_to_string(dir.path().join("attach.log"))?
        .lines()
        .count();
    assert!(attaches >= 2, "it reconnected: {attaches}");
    clients.shutdown();
    Ok(())
}

/// A client whose session is gone ends without a fault and is not "died".
#[test]
fn control_client_for_a_gone_session_ends_quietly() -> TestResult {
    let dir = tempfile::tempdir()?;
    fs::write(dir.path().join("gone"), "")?;
    let (tx, rx) = channel();
    let mut clients = ControlClients::new(fake_tmux(dir.path())?, vec!["poll".into()], tx)
        .with_timing(Duration::from_millis(50), Duration::from_millis(20));
    clients.reconcile()?;
    std::thread::sleep(Duration::from_millis(400));
    let events: Vec<Event> = rx.try_iter().collect();
    assert!(
        !events.iter().any(|e| matches!(e, Event::Fault { .. })),
        "{events:?}"
    );
    assert_eq!(clients.reconcile()?, Vec::<String>::new());
    clients.shutdown();
    Ok(())
}

/// A server that cannot list sessions is a reconcile error.
#[test]
fn control_reconcile_failure_is_an_error() -> TestResult {
    let dir = tempfile::tempdir()?;
    fs::write(dir.path().join("noserver"), "")?;
    let (tx, _rx) = channel();
    let mut clients = ControlClients::new(fake_tmux(dir.path())?, Vec::new(), tx);
    assert!(clients.reconcile().is_err());
    assert!(clients.sessions().is_empty());
    Ok(())
}
