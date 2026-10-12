//! #9171 A3a fix round: the public `Tmux::run` never sends keys.
//!
//! Why: `send_guarded` checks the input box before it types. A public `run`
//! that accepted `send-keys` would let any caller type into a pane unguarded.
//! Test: this file; the guarded path is covered by
//! `send_guarded_refuses_typed_and_sends_on_empty`.

use trusty_architect::{FakeTmux, ScriptedTmux, SystemTmux, Tmux, TmuxError};

fn refused(r: Result<String, TmuxError>) -> bool {
    matches!(r, Err(TmuxError::SendKeysRefused))
}

#[test]
fn run_refuses_send_keys_on_every_implementation() {
    let argv = ["send-keys", "-t", "%1", "-l", "--", "hello"];

    let fake = FakeTmux::new();
    assert!(refused(fake.run(&argv)), "FakeTmux must refuse");
    assert!(fake.calls().is_empty(), "a refused verb is not recorded");

    // /bin/echo stands in for tmux: had the verb run, it would exit 0.
    let system = SystemTmux::new().with_program("/bin/echo");
    assert!(refused(system.run(&argv)), "SystemTmux must refuse");

    // #9171 A3b-1: the scripted fake refuses too, before its script sees it.
    let scripted = ScriptedTmux::new().fallback("send-keys", "");
    assert!(refused(scripted.run(&argv)), "ScriptedTmux must refuse");
    assert!(
        scripted.calls().is_empty(),
        "a refused verb is not recorded"
    );

    // Other verbs still run.
    assert!(fake.run(&["list-panes"]).is_ok());
    assert_eq!(fake.calls(), [["list-panes"]]);
    assert!(system.run(&["list-panes"]).is_ok());
}
