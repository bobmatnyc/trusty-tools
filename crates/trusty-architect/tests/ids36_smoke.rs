//! #12029 P2a: plain runs of the shipped `trusty-architect-ids36`.
//!
//! The replay recordings run the library through `examples/replay_port.rs`;
//! this proves the shipped binary's one subcommand, `next`, and its refusal
//! and usage paths, with the script's streams and exit codes.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_trusty-architect-ids36");

fn run(args: &[&str]) -> (String, String, Option<i32>) {
    let out = Command::new(BIN).args(args).output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

#[test]
fn shipped_ids36_next_and_usage() {
    assert_eq!(
        run(&["next", "0z"]),
        ("10\n".into(), String::new(), Some(0))
    );
    assert_eq!(
        run(&["next", "zz"]),
        ("01\n".into(), String::new(), Some(0))
    );
    assert_eq!(
        run(&["next", "0o"]),
        (
            String::new(),
            "ids36: not two base-34 digits: '0o'\n".into(),
            Some(1)
        )
    );
    let usage = "usage: ids36.py next <two digits>   (0-9 and a-z without o and l; e.g. 0f -> 0g, 0z -> 10)\n";
    for args in [&[][..], &["next"], &["next", "0f", "0g"], &["prev", "0f"]] {
        assert_eq!(
            run(args),
            (String::new(), usage.into(), Some(1)),
            "{args:?}"
        );
    }
}
